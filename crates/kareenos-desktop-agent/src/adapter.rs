//! Action → Terminator. The ONLY place the channel's bounded vocabulary meets
//! the Terminator core (the twin of the extension's browser-channel-adapter).
//!
//! Every action resolves its target the same way: `ref` (from the lane's latest
//! snapshot) or `selector` (Terminator grammar, `text=Label` sugar), scoped to a
//! process when given, else to the focused window's process. Several visible
//! matches answer AMBIGUOUS with candidates; a ref from an older snapshot answers
//! STALE_REF. Mutating actions return a `settle` report built from Terminator's
//! before/after tree diff.

use crate::envelope::{self, CommandEnvelope, ResultEnvelope, MAX_RESULT_BYTES, RUN_STEPS_ALLOWED, RUN_STEPS_MAX};
use crate::policy::LocalPolicy;
use crate::presence::Presence;
use crate::refbook::{pick_candidate, Bounds, LaneBook, RefBook, RefEntry, RefError, Root};
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use terminator::ui_tree_diff::simple_ui_tree_diff;
use terminator::{
    format_ui_node_as_compact_yaml, AutomationError, Browser, Desktop, PropertyLoadingMode, ScreenshotResult,
    TreeBuildConfig, UIElement, UINode,
};

#[derive(Debug, Clone)]
pub struct Settings {
    pub screenshot_max_px: u32,
    /// Contract 1.1 (from bind_ack): the cap of one file transfer and the hold window.
    pub file_max_bytes: u64,
    pub hold_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { screenshot_max_px: 1280, file_max_bytes: envelope::FILE_MAX_BYTES_DEFAULT, hold_ms: envelope::HOLD_MS_DEFAULT }
    }
}

#[derive(Debug, Clone)]
pub struct ActionError {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ActionError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ActionError { code, message: message.into(), details: None }
    }
    pub fn with(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        ActionError { code, message: message.into(), details: Some(details) }
    }
    fn missing(what: &str) -> Self {
        ActionError::new("EXECUTION_ERROR", format!("Missing argument: {what}"))
    }
}

impl From<AutomationError> for ActionError {
    fn from(e: AutomationError) -> Self {
        use AutomationError::*;
        match e {
            ElementNotFound(m) => ActionError::new("NOT_FOUND", m),
            Timeout(m) => ActionError::new("TIMEOUT", m),
            ElementNotVisible(m) => ActionError::new("NOT_VISIBLE", m),
            ElementObscured(m) => ActionError::new("NOT_VISIBLE", format!("covered by another element: {m}")),
            ElementNotEnabled(m) => ActionError::new("DISABLED", m),
            ElementNotStable(m) => ActionError::new("NOT_STABLE", m),
            ElementDetached(m) => ActionError::new("STALE_REF", m),
            OperationCancelled(m) => ActionError::new("TIMEOUT", m),
            InvalidSelector(m) => ActionError::new("EXECUTION_ERROR", format!("invalid selector: {m}")),
            other => ActionError::new("EXECUTION_ERROR", other.to_string()),
        }
    }
}

type AResult = Result<(String, Value), ActionError>;

#[derive(Debug, Clone, Serialize)]
struct WindowInfo {
    title: String,
    process: String,
    pid: u32,
    hwnd: Option<isize>,
}

fn window_info(el: &UIElement) -> WindowInfo {
    WindowInfo {
        title: el.name().unwrap_or_default(),
        process: el.process_name().unwrap_or_default(),
        pid: el.process_id().unwrap_or(0),
        hwnd: el.get_native_window_handle().ok(),
    }
}

struct Target {
    element: UIElement,
    reference: Option<String>,
    role: String,
    name: String,
    pid: u32,
    process: String,
}

impl Target {
    fn of(element: UIElement, reference: Option<String>) -> Target {
        Target {
            role: element.role(),
            name: element.name().unwrap_or_default(),
            pid: element.process_id().unwrap_or(0),
            process: element.process_name().unwrap_or_default(),
            reference,
            element,
        }
    }
    fn json(&self) -> Value {
        json!({ "ref": self.reference, "role": self.role, "name": self.name })
    }
}

#[derive(Debug, Default, Serialize)]
struct Settle {
    ui_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui_diff: Option<String>,
    focus_changed: bool,
    window_changed: bool,
    new_window: bool,
    dialog_opened: bool,
}

struct State {
    tree_json: Option<String>,
    focused_id: Option<String>,
    window_title: Option<String>,
    window_count: usize,
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}
fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.trim() {
            "true" => true,
            "false" => false,
            _ => default,
        },
        _ => default,
    }
}
fn arg_num(args: &Value, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}
fn clamp_usize(v: f64, lo: usize, hi: usize) -> usize {
    if !v.is_finite() {
        return lo;
    }
    (v.max(lo as f64).min(hi as f64)) as usize
}
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
/// Cut a multi-line text at `max_chars`, whole lines only, with the browser's "+N lines omitted" tail.
fn cap_lines(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        return (text.to_string(), false);
    }
    let mut out = String::new();
    let mut omitted = 0usize;
    for line in text.lines() {
        if out.len() + line.len() + 1 <= max_chars {
            out.push_str(line);
            out.push('\n');
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        out.push_str(&format!("… +{omitted} lines omitted"));
    }
    (out, true)
}
fn parse_bool_value(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "checked" => Some(true),
        "false" | "0" | "no" | "off" | "unchecked" => Some(false),
        _ => None,
    }
}
fn normalize_selector(sel: &str) -> String {
    let t = sel.trim();
    if let Some(v) = t.strip_prefix("text=") {
        format!("name:{}", v.trim())
    } else {
        t.to_string()
    }
}
fn tree_cfg(include_all_bounds: bool, with_values: bool, depth: usize) -> TreeBuildConfig {
    TreeBuildConfig {
        property_mode: if with_values { PropertyLoadingMode::Complete } else { PropertyLoadingMode::Fast },
        timeout_per_operation_ms: Some(50),
        yield_every_n_elements: Some(50),
        batch_size: Some(50),
        max_depth: Some(depth),
        include_all_bounds,
        ..TreeBuildConfig::default()
    }
}

pub struct Adapter {
    desktop: Arc<Desktop>,
    refbook: RefBook,
    policy: LocalPolicy,
    presence: Arc<Presence>,
    settings: Mutex<Settings>,
}

impl Adapter {
    pub fn new(desktop: Arc<Desktop>, policy: LocalPolicy, presence: Arc<Presence>) -> Self {
        Adapter { desktop, refbook: RefBook::new(), policy, presence, settings: Mutex::new(Settings::default()) }
    }

    /// Platform-driven settings from bind_ack: `{ human_idle_ms, screenshot_max_px }`.
    /// The local policy's human_idle_ms wins when set.
    pub fn apply_settings(&self, v: &Value) {
        if let Some(px) = v.get("screenshot_max_px").and_then(|x| x.as_u64()) {
            self.settings.lock().unwrap().screenshot_max_px = (px as u32).clamp(320, 4096);
        }
        if let Some(ms) = v.get("human_idle_ms").and_then(|x| x.as_u64()) {
            let effective = self.policy.human_idle_ms.unwrap_or(ms);
            self.presence.set_idle_window(effective);
        }
    }

    /// Contract 1.1: what the tier's bind_ack carries besides `settings`.
    pub fn apply_ack(&self, ack: &Value) {
        let mut s = self.settings.lock().unwrap();
        if let Some(b) = ack.get("file_max_bytes").and_then(|x| x.as_u64()).filter(|b| *b > 0) {
            s.file_max_bytes = b;
        }
        if let Some(h) = ack.get("hold_ms").and_then(|x| x.as_u64()).filter(|h| *h > 0) {
            s.hold_ms = h;
        }
    }

    pub async fn execute(&self, cmd: &CommandEnvelope, budget_ms: u64) -> ResultEnvelope {
        let action = cmd.action.as_str();
        if !envelope::is_supported(action) {
            return ResultEnvelope::failure(cmd, "UNSUPPORTED_ACTION", format!("Unsupported action: {action}"), None);
        }
        if action != "lane_close" && self.presence.is_locked() {
            return ResultEnvelope::failure(
                cmd,
                "COMPUTER_LOCKED",
                "This computer is locked or at the Windows sign-in screen, so nothing on its desktop can be driven. Tell the user to sign in and keep it unlocked (an unattended computer must stay signed in); retry after that.",
                Some(json!({ "retry_after_ms": 60000 })),
            );
        }
        if action != "lane_close" && self.presence.is_paused() {
            return ResultEnvelope::failure(
                cmd,
                "PAUSED_BY_USER",
                "The user paused agents on this computer (tray → Pause agents). Tell them you are waiting, and retry only after they resume.",
                Some(json!({ "retry_after_ms": 60000 })),
            );
        }
        if !envelope::is_human_safe(action) {
            if let Some(h) = self.presence.human_active() {
                return ResultEnvelope::failure(
                    cmd,
                    "HUMAN_ACTIVE",
                    format!("The person is using the computer right now (mouse or keyboard input {} ms ago). Retry in {} ms.", h.idle_ms, h.retry_after_ms),
                    Some(json!({ "idle_ms": h.idle_ms, "retry_after_ms": h.retry_after_ms })),
                );
            }
        }
        let lane = cmd.lane();
        let t0 = Instant::now();
        let out = if action == "run_steps" {
            self.run_steps(&lane, &cmd.args, budget_ms).await
        } else {
            self.run_single(&lane, action, &cmd.args, budget_ms).await
        };
        let env = match out {
            Ok((msg, data)) => ResultEnvelope::success(cmd, msg, data),
            Err(e) => ResultEnvelope::failure(cmd, e.code, e.message, e.details),
        };
        tracing::info!(command = %cmd.command_id, action, lane = %lane, status = env.status, ms = t0.elapsed().as_millis() as u64, "action done");
        let bytes = serde_json::to_vec(&env).map(|v| v.len()).unwrap_or(0);
        if bytes > MAX_RESULT_BYTES {
            return ResultEnvelope::failure(
                cmd,
                "RESULT_TOO_LARGE",
                format!("The {action} result is {bytes} bytes; results are capped at {MAX_RESULT_BYTES} bytes. Narrow the scope (a ref subtree, a smaller max_chars or max_px)."),
                None,
            );
        }
        env
    }

    async fn run_single(&self, lane: &str, action: &str, args: &Value, budget_ms: u64) -> AResult {
        match action {
            "snapshot" => self.snapshot(lane, args).await,
            "apps" => self.apps(args).await,
            "open" => self.open(args).await,
            "focus" => self.focus(lane, args).await,
            "read" => self.read(lane, args).await,
            "screenshot" => self.screenshot(lane, args).await,
            "scroll" => self.scroll(lane, args).await,
            "wait" => self.wait(lane, args, budget_ms).await,
            "click" => self.click(lane, args).await,
            "fill" => self.fill(lane, args).await,
            "press" => self.press(lane, args).await,
            "lane_close" => self.lane_close(lane, args),
            other => Err(ActionError::new("UNSUPPORTED_ACTION", format!("Unsupported action: {other}"))),
        }
    }

    // ------------------------------------------------------------------ windows

    fn focused_window(&self) -> Result<UIElement, ActionError> {
        let focused = self.desktop.focused_element()?;
        if focused.role().eq_ignore_ascii_case("window") {
            return Ok(focused);
        }
        match focused.window() {
            Ok(Some(w)) => Ok(w),
            _ => Ok(focused),
        }
    }

    /// Top-level windows of every application: (window element, owning app name).
    fn all_windows(&self) -> Vec<UIElement> {
        let mut out = vec![];
        if let Ok(apps) = self.desktop.applications() {
            for app in apps {
                let mut found = false;
                if let Ok(children) = app.children() {
                    for c in children {
                        if c.role().eq_ignore_ascii_case("window") {
                            found = true;
                            out.push(c);
                        }
                    }
                }
                if !found && app.role().eq_ignore_ascii_case("window") {
                    out.push(app);
                }
            }
        }
        out
    }

    fn window_count(&self, pid: u32) -> usize {
        if pid == 0 {
            return 0;
        }
        self.all_windows().iter().filter(|w| w.process_id().unwrap_or(0) == pid).count()
    }

    fn window_for_pid(&self, pid: u32) -> Option<UIElement> {
        self.all_windows().into_iter().find(|w| w.process_id().unwrap_or(0) == pid)
    }

    fn find_window_by_title(&self, title: &str) -> Result<UIElement, ActionError> {
        let needle = title.trim().to_lowercase();
        let hit = self.all_windows().into_iter().find(|w| w.name().unwrap_or_default().to_lowercase().contains(&needle));
        hit.ok_or_else(|| {
            ActionError::with(
                "WINDOW_NOT_FOUND",
                format!("No window whose title contains \"{title}\". Call desktop_apps to see the open windows."),
                json!({ "window": title }),
            )
        })
    }

    // ------------------------------------------------------------------ targets

    fn resolve_ref(&self, lane: &str, reference: &str) -> Result<RefEntry, ActionError> {
        match self.refbook.resolve(lane, reference) {
            Ok(e) => Ok(e),
            Err(RefError::BadRef(r)) => Err(ActionError::new("EXECUTION_ERROR", format!("ref must look like e12 (got \"{r}\")"))),
            Err(RefError::Stale { reference, epoch }) => Err(ActionError::with(
                "STALE_REF",
                format!("Ref {reference} is not in this task's latest snapshot (epoch {epoch}); the window changed or no snapshot was taken yet. Call desktop_snapshot again and use a ref from it."),
                json!({ "ref": reference, "epoch": epoch }),
            )),
        }
    }

    /// The live element behind a snapshot ref. Terminator's recorded role/name path
    /// is the same for unnamed siblings (five `[Edit]` fields of one form all read
    /// `… >> role:Edit`), so following the path lands on the first of them — the
    /// 2026-11-02 dogfood bug where every value of a form went into its first box.
    /// The path is therefore only the search key: every live element of the ref's
    /// role and name inside the snapshot's window is listed, and the one at the
    /// ref's recorded bounds is taken (`refbook::pick_candidate`: the window's move
    /// and the ref's ordinal are the fallbacks). Live matches with none at the ref's
    /// place answer STALE_REF — the window changed, snapshot again.
    async fn element_for_entry(&self, lane: &str, e: &RefEntry) -> Result<UIElement, ActionError> {
        let Some(sel) = e.selector.as_deref() else {
            return Err(ActionError::with(
                "STALE_REF",
                format!("Ref e{} ({} \"{}\") has no stable role/name path; snapshot again or target it with a selector.", e.index, e.role, e.name),
                json!({ "ref": e.index }),
            ));
        };
        let book = self.refbook.lane(lane);
        let (head, last) = match sel.rsplit_once(" >> ") {
            Some((h, l)) => (Some(h), l),
            None => (None, sel),
        };
        if let Some(root) = self.snapshot_root(book.as_ref(), e, head).await {
            let mut found: Vec<UIElement> = match root.locator(last) {
                Ok(loc) => loc.all(Some(Duration::from_millis(2500)), Some(30)).await.unwrap_or_default(),
                Err(_) => vec![],
            };
            if !found.is_empty() {
                let bounds: Vec<Option<Bounds>> = found.iter().map(|c| c.bounds().ok()).collect();
                let shift = match (book.as_ref().and_then(|b| b.window_bounds), root.bounds().ok()) {
                    (Some(then), Some(now)) => Some((now.0 - then.0, now.1 - then.1)),
                    _ => None,
                };
                let ordinal = book.as_ref().and_then(|b| b.ordinal(e.index)).map(|(o, _)| o);
                if let Some(i) = pick_candidate(e.bounds, shift, ordinal, &bounds) {
                    return Ok(found.swap_remove(i));
                }
                let live: Vec<Value> = bounds.iter().take(8).map(|b| json!(b.map(|b| [b.0, b.1, b.2, b.3]))).collect();
                return Err(ActionError::with(
                    "STALE_REF",
                    format!(
                        "Ref e{} ({} \"{}\") is no longer where the snapshot saw it in \"{}\" ({} {} element(s) there now, none at its place) — the window changed; call desktop_snapshot again.",
                        e.index, e.role, e.name, e.window_title, found.len(), e.role
                    ),
                    json!({ "ref": e.index, "selector": sel, "bounds": [e.bounds.0, e.bounds.1, e.bounds.2, e.bounds.3], "live_bounds": live }),
                ));
            }
        }
        // Nothing of the ref's kind inside its window (or the window is gone): the recorded path itself.
        let chain = if sel.starts_with("process:") || e.process.is_empty() {
            sel.to_string()
        } else {
            format!("process:{} >> {}", e.process, sel)
        };
        match self.desktop.locator(chain.as_str()).first(Some(Duration::from_millis(2500))).await {
            Ok(el) => Ok(el),
            Err(err) => Err(ActionError::with(
                "STALE_REF",
                format!(
                    "Ref e{} ({} \"{}\") no longer resolves in \"{}\" — the window changed; call desktop_snapshot again. ({})",
                    e.index, e.role, e.name, e.window_title, err
                ),
                json!({ "ref": e.index, "selector": chain }),
            )),
        }
    }

    /// The window a lane's snapshot was taken in: by native handle (among the listed
    /// windows and the focused one), else the first segment of the ref's recorded
    /// path — the window node, process-scoped, exactly where path resolution starts.
    async fn snapshot_root(&self, book: Option<&LaneBook>, e: &RefEntry, head: Option<&str>) -> Option<UIElement> {
        if let Some(h) = book.and_then(|b| b.hwnd) {
            let mut windows = self.all_windows();
            if let Ok(f) = self.focused_window() {
                windows.push(f);
            }
            if let Some(w) = windows.into_iter().find(|w| w.get_native_window_handle().ok() == Some(h)) {
                return Some(w);
            }
        }
        let first = head?.split(" >> ").next()?;
        let chain = if first.starts_with("process:") || e.process.is_empty() {
            first.to_string()
        } else {
            format!("process:{} >> {}", e.process, first)
        };
        self.desktop.locator(chain.as_str()).first(Some(Duration::from_millis(2500))).await.ok()
    }

    async fn resolve_selector(&self, lane: &str, selector: &str, process: Option<String>) -> Result<Target, ActionError> {
        let base = normalize_selector(selector);
        let scoped = if base.starts_with("process:") || base.starts_with("window:") {
            base.clone()
        } else {
            let p = match process {
                Some(p) => Some(p),
                None => self.focused_window().ok().and_then(|w| w.process_name().ok()),
            };
            match p {
                Some(p) if !p.is_empty() => format!("process:{p} >> {base}"),
                _ => base.clone(),
            }
        };
        let found = match self.desktop.locator(scoped.as_str()).all(Some(Duration::from_millis(3000)), None).await {
            Ok(v) => v,
            Err(AutomationError::ElementNotFound(_)) | Err(AutomationError::Timeout(_)) => vec![],
            Err(e) => return Err(e.into()),
        };
        let visible: Vec<UIElement> = found.into_iter().filter(|e| e.is_visible().unwrap_or(true)).collect();
        match visible.len() {
            0 => Err(ActionError::with(
                "NOT_FOUND",
                format!("Nothing visible matched selector \"{scoped}\". Call desktop_snapshot and use a ref, or another name."),
                json!({ "selector": scoped }),
            )),
            1 => Ok(Target::of(visible.into_iter().next().unwrap(), None)),
            n => {
                let candidates: Vec<Value> = visible
                    .iter()
                    .take(8)
                    .map(|e| {
                        let role = e.role();
                        let name = e.name().unwrap_or_default();
                        let reference = self.refbook.find_ref(lane, &role, &name, e.bounds().ok()).map(|i| format!("e{i}"));
                        let window = e.window().ok().flatten().and_then(|w| w.name()).unwrap_or_default();
                        json!({ "ref": reference, "role": role, "name": name, "window": window })
                    })
                    .collect();
                Err(ActionError::with(
                    "AMBIGUOUS",
                    format!("{n} visible elements match selector \"{scoped}\"; retry with the ref of the one you mean (desktop_snapshot lists them)."),
                    json!({ "selector": scoped, "count": n, "candidates": candidates }),
                ))
            }
        }
    }

    async fn resolve_target(&self, lane: &str, args: &Value, required: bool) -> Result<Target, ActionError> {
        if let Some(r) = arg_str(args, "ref") {
            let entry = self.resolve_ref(lane, &r)?;
            let el = self.element_for_entry(lane, &entry).await?;
            let mut t = Target::of(el, Some(format!("e{}", entry.index)));
            if t.pid == 0 {
                t.pid = entry.pid;
            }
            if t.process.is_empty() {
                t.process = entry.process.clone();
            }
            return Ok(t);
        }
        if let Some(sel) = arg_str(args, "selector") {
            return self.resolve_selector(lane, &sel, arg_str(args, "process")).await;
        }
        if required {
            return Err(ActionError::new("EXECUTION_ERROR", "Provide ref (from desktop_snapshot) or selector."));
        }
        let w = self.focused_window()?;
        Ok(Target::of(w, None))
    }

    // ------------------------------------------------------------------ settle

    fn capture_state(&self, pid: u32) -> State {
        let tree_json = if pid > 0 {
            self.desktop.get_window_tree(pid, None, Some(tree_cfg(false, false, 30))).ok().and_then(|t| serde_json::to_string(&t).ok())
        } else {
            None
        };
        let focused = self.desktop.focused_element().ok();
        let focused_id = focused.as_ref().and_then(|f| f.id());
        let window_title = focused.as_ref().and_then(|f| f.window().ok().flatten()).and_then(|w| w.name());
        State { tree_json, focused_id, window_title, window_count: self.window_count(pid) }
    }

    fn settle_from(before: &State, after: &State) -> Settle {
        let diff = match (&before.tree_json, &after.tree_json) {
            (Some(a), Some(b)) => simple_ui_tree_diff(a, b).ok().flatten(),
            _ => None,
        };
        let dialog_opened = diff
            .as_ref()
            .map(|d| d.lines().any(|l| l.starts_with('+') && (l.contains("\"Dialog\"") || l.contains("[Dialog]"))))
            .unwrap_or(false);
        Settle {
            ui_changed: diff.is_some(),
            ui_diff: diff.as_ref().map(|d| truncate_chars(d, 2000)),
            focus_changed: before.focused_id != after.focused_id,
            window_changed: before.window_title != after.window_title,
            new_window: after.window_count > before.window_count,
            dialog_opened: dialog_opened || after.window_count > before.window_count,
        }
    }

    async fn with_settle<F>(&self, pid: u32, act: F) -> Result<(Value, Settle), ActionError>
    where
        F: FnOnce() -> Result<Value, ActionError>,
    {
        let before = self.capture_state(pid);
        let data = act()?;
        self.presence.note_agent_input();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let after = self.capture_state(pid);
        Ok((data, Self::settle_from(&before, &after)))
    }

    // ------------------------------------------------------------------ actions

    async fn snapshot(&self, lane: &str, args: &Value) -> AResult {
        let scope = arg_str(args, "scope").unwrap_or_default();
        let full = arg_str(args, "mode").as_deref() == Some("full");
        let values = arg_bool(args, "include_values", false);
        let max_chars = clamp_usize(arg_num(args, "max_chars").unwrap_or(12000.0), 2000, 60000);
        let cfg = tree_cfg(full, values, 30);
        let (info, tree, root_window): (WindowInfo, UINode, Option<UIElement>) = if scope.is_empty() || scope == "focused" {
            let w = self.focused_window()?;
            let t = self.desktop.get_tree_from_element(&w, Some(cfg))?;
            let info = window_info(&w);
            (info, t, Some(w))
        } else if let Some(p) = scope.strip_prefix("process:") {
            let p = p.trim();
            let pid = terminator::find_pid_for_process(&self.desktop, p).map_err(|_| {
                ActionError::with(
                    "APP_NOT_FOUND",
                    format!("No running application with process name \"{p}\". Call desktop_apps to see what is running, or desktop_open to start it."),
                    json!({ "process": p }),
                )
            })?;
            let t = self.desktop.get_window_tree(pid, None, Some(cfg))?;
            // The window the tree was built from: the process's window with the tree's title, else its first.
            let title = tree_title(&t);
            let mut wins: Vec<UIElement> = self.all_windows().into_iter().filter(|w| w.process_id().unwrap_or(0) == pid).collect();
            let root_window = match wins.iter().position(|w| w.name().unwrap_or_default() == title) {
                Some(i) => Some(wins.swap_remove(i)),
                None => wins.into_iter().next(),
            };
            let info = match &root_window {
                Some(w) => window_info(w),
                None => WindowInfo { title, process: p.to_string(), pid, hwnd: None },
            };
            (info, t, root_window)
        } else if let Some(title) = scope.strip_prefix("window:") {
            let w = self.find_window_by_title(title)?;
            let t = self.desktop.get_tree_from_element(&w, Some(cfg))?;
            let info = window_info(&w);
            (info, t, Some(w))
        } else {
            let entry = self.resolve_ref(lane, &scope)?;
            let el = self.element_for_entry(lane, &entry).await?;
            let t = self.desktop.get_tree_from_element(&el, Some(cfg))?;
            let root_window = el.window().ok().flatten();
            let info = match &root_window {
                Some(w) => window_info(w),
                None => WindowInfo { title: entry.window_title.clone(), process: entry.process.clone(), pid: entry.pid, hwnd: None },
            };
            (info, t, root_window)
        };
        let fmt = format_ui_node_as_compact_yaml(&tree, 0);
        // The refs' home: the window's handle and bounds let a ref be found again by its
        // place even when siblings share its role/name path (see element_for_entry).
        let root = Root {
            pid: info.pid,
            process: info.process.clone(),
            window_title: info.title.clone(),
            hwnd: info.hwnd,
            window_bounds: root_window.as_ref().and_then(|w| w.bounds().ok()),
        };
        let epoch = self.refbook.record(lane, &root, &fmt.index_to_bounds);
        let windows_total = self.window_count(info.pid);
        let header = format!(
            "## window \"{}\" process={} pid={} epoch={} ({} window{} of this process) refs={}\n",
            info.title,
            info.process,
            info.pid,
            epoch,
            windows_total.max(1),
            if windows_total == 1 { "" } else { "s" },
            fmt.element_count
        );
        let (body, truncated) = cap_lines(&fmt.formatted, max_chars.saturating_sub(header.len()));
        let text = format!("{header}{body}");
        let data = json!({
            "text": text, "epoch": epoch, "window": info, "windows_total": windows_total, "refs": fmt.element_count,
            "chars": text.len(), "max_chars": max_chars, "truncated": truncated, "mode": if full { "full" } else { "interactive" }
        });
        Ok(("snapshot ok".to_string(), data))
    }

    async fn apps(&self, args: &Value) -> AResult {
        let include_windows = arg_bool(args, "include_windows", true);
        let focused_hwnd = self.focused_window().ok().and_then(|w| w.get_native_window_handle().ok());
        let apps = self.desktop.applications()?;
        let mut out = vec![];
        for app in apps.iter().take(80) {
            let name = app.name().unwrap_or_default();
            let process = app.process_name().unwrap_or_default();
            let pid = app.process_id().unwrap_or(0);
            let mut windows = vec![];
            if include_windows {
                let mut found = false;
                if let Ok(children) = app.children() {
                    for c in children {
                        if c.role().eq_ignore_ascii_case("window") {
                            found = true;
                            let hwnd = c.get_native_window_handle().ok();
                            windows.push(json!({ "title": c.name().unwrap_or_default(), "hwnd": hwnd, "focused": hwnd.is_some() && hwnd == focused_hwnd }));
                        }
                    }
                }
                if !found && app.role().eq_ignore_ascii_case("window") {
                    let hwnd = app.get_native_window_handle().ok();
                    windows.push(json!({ "title": name, "hwnd": hwnd, "focused": hwnd.is_some() && hwnd == focused_hwnd }));
                }
            }
            out.push(json!({ "name": name, "process": process, "pid": pid, "windows": windows }));
        }
        Ok((format!("{} applications", out.len()), json!({ "apps": out })))
    }

    async fn open(&self, args: &Value) -> AResult {
        if let Some(url) = arg_str(args, "url") {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ActionError::new("EXECUTION_ERROR", "url must start with http:// or https://"));
            }
            self.desktop.open_url(&url, Some(Browser::Default))?;
            self.presence.note_agent_input();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let info = self.focused_window().ok().map(|w| window_info(&w));
            return Ok((format!("opened {url}"), json!({ "window": info, "opened": url })));
        }
        let app = arg_str(args, "app").ok_or_else(|| ActionError::missing("app or url"))?;
        if !self.policy.allow_open || !self.policy.app_allowed(&app) {
            return Err(ActionError::with(
                "APP_NOT_ALLOWED",
                format!("This computer's local policy does not allow agents to open \"{app}\" (shells, consoles and system tools are refused by default). Tell the user; do not retry."),
                json!({ "app": app }),
            ));
        }
        let el = self.desktop.open_application(&app).map_err(|e| match e {
            AutomationError::ElementNotFound(m) => ActionError::with("APP_NOT_FOUND", format!("Could not start \"{app}\": {m}"), json!({ "app": app })),
            AutomationError::PlatformError(m) if m.to_lowercase().contains("not found") || m.to_lowercase().contains("cannot find") => {
                ActionError::with("APP_NOT_FOUND", format!("Could not start \"{app}\": {m}"), json!({ "app": app }))
            }
            other => ActionError::from(other),
        })?;
        self.presence.note_agent_input();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut info = match el.window() {
            Ok(Some(w)) => window_info(&w),
            _ => window_info(&el),
        };
        if info.title.is_empty() {
            if let Ok(w) = self.focused_window() {
                info = window_info(&w);
            }
        }
        Ok((format!("opened {app}"), json!({ "window": info, "opened": app })))
    }

    async fn focus(&self, lane: &str, args: &Value) -> AResult {
        let el: UIElement = if let Some(r) = arg_str(args, "ref") {
            let entry = self.resolve_ref(lane, &r)?;
            self.element_for_entry(lane, &entry).await?
        } else if let Some(p) = arg_str(args, "process") {
            self.desktop.activate_application(&p).map_err(|e| match e {
                AutomationError::ElementNotFound(m) => ActionError::with("APP_NOT_FOUND", format!("No running application \"{p}\": {m}"), json!({ "process": p })),
                other => ActionError::from(other),
            })?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            self.focused_window()?
        } else if let Some(w) = arg_str(args, "window") {
            self.find_window_by_title(&w)?
        } else {
            return Err(ActionError::missing("ref, process or window"));
        };
        let target_window = match el.window() {
            Ok(Some(w)) => w,
            _ => el,
        };
        target_window.activate_window()?;
        self.presence.note_agent_input();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let info = window_info(&target_window);
        Ok(("focus ok".to_string(), json!({ "window": info, "focused": true })))
    }

    async fn read(&self, lane: &str, args: &Value) -> AResult {
        let max_chars = clamp_usize(arg_num(args, "max_chars").unwrap_or(12000.0), 500, 60000);
        let t = self.resolve_target(lane, args, false).await?;
        let text = t.element.text(25)?;
        let total = text.chars().count();
        let out = truncate_chars(&text, max_chars);
        let window = t.element.window().ok().flatten().map(|w| window_info(&w));
        Ok((
            format!("read {total} chars from {} \"{}\"", t.role, t.name),
            json!({ "text": out, "chars": total, "truncated": total > max_chars, "target": t.json(), "window": window }),
        ))
    }

    async fn screenshot(&self, lane: &str, args: &Value) -> AResult {
        let default_px = self.settings.lock().unwrap().screenshot_max_px as f64;
        let max_px = clamp_usize(arg_num(args, "max_px").unwrap_or(default_px), 320, 4096) as u32;
        let full = arg_bool(args, "full_screen", false);
        let (shot, scope, info): (ScreenshotResult, &str, Option<WindowInfo>) = if full {
            let m = self.desktop.get_active_monitor().await?;
            (self.desktop.capture_monitor(&m).await?, "screen", None)
        } else if let Some(r) = arg_str(args, "ref") {
            let entry = self.resolve_ref(lane, &r)?;
            let el = self.element_for_entry(lane, &entry).await?;
            let info = el.window().ok().flatten().map(|w| window_info(&w));
            (el.capture()?, "element", info)
        } else if let Some(w) = arg_str(args, "window") {
            let win = self.find_window_by_title(&w)?;
            let info = window_info(&win);
            (win.capture()?, "window", Some(info))
        } else if let Some(p) = arg_str(args, "process") {
            let shot = self.desktop.capture_window_by_process(&p).map_err(|e| {
                ActionError::with("APP_NOT_FOUND", format!("No window of process \"{p}\" to capture: {e}"), json!({ "process": p }))
            })?;
            (shot, "window", None)
        } else {
            let w = self.focused_window()?;
            let info = window_info(&w);
            (w.capture()?, "window", Some(info))
        };
        let bytes = shot
            .to_jpeg_resized(Some(max_px), Some(80))
            .map_err(|e| ActionError::new("EXECUTION_ERROR", format!("screenshot encode failed: {e}")))?;
        let (w, h) = shot.resized_dimensions(max_px);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok((
            "screenshot ok".to_string(),
            json!({ "base64": b64, "media_type": "image/jpeg", "width": w, "height": h, "scope": scope, "window": info, "bytes": bytes.len() }),
        ))
    }

    async fn scroll(&self, lane: &str, args: &Value) -> AResult {
        let direction = arg_str(args, "direction").unwrap_or_else(|| "down".to_string()).to_lowercase();
        if !["up", "down", "left", "right"].contains(&direction.as_str()) {
            return Err(ActionError::new("EXECUTION_ERROR", "direction must be up, down, left or right"));
        }
        let amount = arg_num(args, "amount").unwrap_or(3.0).clamp(1.0, 50.0);
        let t = self.resolve_target(lane, args, false).await?;
        t.element.scroll(&direction, amount)?;
        self.presence.note_agent_input();
        Ok(("scroll ok".to_string(), json!({ "scrolled": true, "direction": direction, "amount": amount, "target": t.json() })))
    }

    async fn wait(&self, lane: &str, args: &Value, budget_ms: u64) -> AResult {
        let state = arg_str(args, "state").unwrap_or_else(|| "visible".to_string());
        if !["visible", "enabled", "focused", "exists", "gone"].contains(&state.as_str()) {
            return Err(ActionError::new("EXECUTION_ERROR", "state must be visible, enabled, focused, exists or gone"));
        }
        if arg_str(args, "ref").is_none() && arg_str(args, "selector").is_none() {
            return Err(ActionError::missing("ref or selector"));
        }
        let wanted = clamp_usize(arg_num(args, "timeout_ms").unwrap_or(10000.0), 1000, 60000) as u64;
        let timeout = Duration::from_millis(wanted.min(budget_ms.saturating_sub(500).max(1000)));
        let start = Instant::now();
        loop {
            let found = match self.resolve_target(lane, args, true).await {
                Ok(t) => Some(t),
                Err(e) if e.code == "NOT_FOUND" || e.code == "STALE_REF" => None,
                Err(e) => return Err(e),
            };
            let ok = match (state.as_str(), &found) {
                ("gone", None) => true,
                ("gone", Some(_)) => false,
                (_, None) => false,
                ("exists", Some(_)) => true,
                ("visible", Some(t)) => t.element.is_visible().unwrap_or(false),
                ("enabled", Some(t)) => t.element.is_enabled().unwrap_or(false),
                ("focused", Some(t)) => t.element.is_focused().unwrap_or(false),
                _ => false,
            };
            let waited = start.elapsed().as_millis() as u64;
            if ok {
                return Ok((
                    format!("wait ok: {state} after {waited} ms"),
                    json!({ "state": state, "matched": found.map(|t| t.json()), "waited_ms": waited }),
                ));
            }
            if start.elapsed() >= timeout {
                return Err(ActionError::with(
                    "TIMEOUT",
                    format!("Waited {waited} ms for the element to be {state}; it did not happen. Snapshot to see the current state."),
                    json!({ "waited_ms": waited, "state": state, "started": true }),
                ));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn click(&self, lane: &str, args: &Value) -> AResult {
        let t = self.resolve_target(lane, args, true).await?;
        let button = arg_str(args, "button").unwrap_or_else(|| "left".to_string()).to_lowercase();
        if !["left", "double", "right"].contains(&button.as_str()) {
            return Err(ActionError::new("EXECUTION_ERROR", "button must be left, double or right"));
        }
        let pid = t.pid;
        let tinfo = t.json();
        let element = t.element;
        let btn = button.clone();
        let (mut data, settle) = self
            .with_settle(pid, move || {
                let (transport, res): (&str, Result<(), AutomationError>) = match btn.as_str() {
                    "double" => ("double_click", element.double_click().map(|_| ())),
                    "right" => ("right_click", element.right_click()),
                    _ => ("click", element.click().map(|_| ())),
                };
                let (transport, res) = match res {
                    Err(AutomationError::ElementNotVisible(_))
                    | Err(AutomationError::ElementObscured(_))
                    | Err(AutomationError::ElementNotStable(_))
                        if btn == "left" =>
                    {
                        ("invoke", element.invoke())
                    }
                    other => (transport, other),
                };
                res?;
                Ok(json!({ "button": btn, "transport": transport }))
            })
            .await?;
        data["target"] = tinfo;
        data["settle"] = serde_json::to_value(&settle).unwrap_or(Value::Null);
        Ok(("click ok".to_string(), data))
    }

    async fn fill(&self, lane: &str, args: &Value) -> AResult {
        let value = match args.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => return Err(ActionError::missing("value")),
            Some(other) => other.to_string(),
        };
        let clear = arg_bool(args, "clear", true);
        let t = self.resolve_target(lane, args, true).await?;
        let role = t.role.to_lowercase();
        let control = if role.contains("checkbox") {
            "checkbox"
        } else if role.contains("radio") {
            "radio"
        } else if role.contains("combobox") || role == "list" || role.contains("listbox") {
            "combo"
        } else {
            "text"
        };
        let pid = t.pid;
        let tinfo = t.json();
        let element = t.element;
        let val = value.clone();
        let (mut data, settle) = self
            .with_settle(pid, move || {
                let mut via = "set_value";
                match control {
                    "checkbox" => {
                        let on = parse_bool_value(&val)
                            .ok_or_else(|| ActionError::new("EXECUTION_ERROR", "a checkbox takes value true or false"))?;
                        element.set_toggled(on)?;
                        via = "set_toggled";
                    }
                    "radio" => {
                        element.set_selected(parse_bool_value(&val).unwrap_or(true))?;
                        via = "set_selected";
                    }
                    "combo" => {
                        element.select_option(&val)?;
                        via = "select_option";
                    }
                    _ => {
                        let use_clipboard = val.chars().count() > 200;
                        if clear {
                            if element.set_value(&val).is_err() {
                                let _ = element.focus();
                                let _ = element.press_key("{Ctrl}a");
                                element.type_text(&val, use_clipboard)?;
                                via = "type_text";
                            }
                        } else {
                            element.type_text(&val, use_clipboard)?;
                            via = "type_text";
                        }
                    }
                }
                let after = element.get_value().ok().flatten().map(|v| truncate_chars(&v, 200));
                Ok(json!({ "control": control, "via": via, "value_length": val.chars().count(), "value_after": after }))
            })
            .await?;
        data["target"] = tinfo;
        data["settle"] = serde_json::to_value(&settle).unwrap_or(Value::Null);
        Ok(("fill ok".to_string(), data))
    }

    async fn press(&self, lane: &str, args: &Value) -> AResult {
        let keys = arg_str(args, "keys").ok_or_else(|| ActionError::missing("keys"))?;
        let has_target = arg_str(args, "ref").is_some() || arg_str(args, "selector").is_some();
        let (pid, tinfo, element) = if has_target {
            let t = self.resolve_target(lane, args, true).await?;
            (t.pid, t.json(), Some(t.element))
        } else {
            let w = self.focused_window()?;
            (w.process_id().unwrap_or(0), Value::Null, None)
        };
        let before = self.capture_state(pid);
        if let Some(el) = &element {
            let _ = el.focus();
        }
        self.desktop.press_key(&keys).await?;
        self.presence.note_agent_input();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let after = self.capture_state(pid);
        let settle = Self::settle_from(&before, &after);
        Ok((
            "press ok".to_string(),
            json!({ "keys": keys, "target": tinfo, "settle": serde_json::to_value(&settle).unwrap_or(Value::Null) }),
        ))
    }

    fn lane_close(&self, lane: &str, args: &Value) -> AResult {
        let n = self.refbook.drop_lane(lane);
        Ok(("lane closed".to_string(), json!({ "closed": true, "refs_dropped": n, "reason": arg_str(args, "reason") })))
    }

    async fn run_steps(&self, lane: &str, args: &Value, budget_ms: u64) -> AResult {
        let steps = args.get("steps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if steps.is_empty() {
            return Err(ActionError::missing("steps (a non-empty array of { action, args, expect? })"));
        }
        if steps.len() > RUN_STEPS_MAX {
            return Err(ActionError::new("EXECUTION_ERROR", format!("at most {RUN_STEPS_MAX} steps per batch (got {})", steps.len())));
        }
        let stop_on_error = arg_bool(args, "stop_on_error", true);
        let start = Instant::now();
        let total_budget = Duration::from_millis(budget_ms);
        let mut out: Vec<Value> = vec![];
        let mut completed = 0usize;
        let mut stopped_at: Option<usize> = None;
        for (i, step) in steps.iter().enumerate() {
            let n = i + 1;
            let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let sargs = step.get("args").cloned().unwrap_or_else(|| json!({}));
            if !RUN_STEPS_ALLOWED.contains(&action.as_str()) {
                out.push(json!({ "i": n, "action": action, "status": "failed", "code": "UNSUPPORTED_ACTION", "message": format!("a step's action must be one of {}", RUN_STEPS_ALLOWED.join(", ")) }));
                stopped_at = Some(n);
                break;
            }
            let remaining = total_budget.saturating_sub(start.elapsed());
            if remaining < Duration::from_millis(1500) {
                out.push(json!({ "i": n, "action": action, "status": "failed", "code": "TIMEOUT", "message": "the batch budget was exhausted before this step" }));
                stopped_at = Some(n);
                break;
            }
            let step_budget = envelope::budget_for(&action, None).min(remaining.as_millis() as u64);
            let t0 = Instant::now();
            let res: AResult = if self.presence.is_paused() {
                Err(ActionError::with("PAUSED_BY_USER", "the user paused agents", json!({ "retry_after_ms": 60000 })))
            } else if let (false, Some(h)) = (envelope::is_human_safe(&action), self.presence.human_active()) {
                Err(ActionError::with("HUMAN_ACTIVE", format!("the person is using the computer ({} ms ago)", h.idle_ms), json!({ "retry_after_ms": h.retry_after_ms })))
            } else {
                match tokio::time::timeout(Duration::from_millis(step_budget), self.run_single(lane, &action, &sargs, step_budget)).await {
                    Ok(r) => r,
                    Err(_) => Err(ActionError::with("TIMEOUT", format!("step {n} ({action}) overran its budget of {step_budget} ms"), json!({ "budget_ms": step_budget, "started": true }))),
                }
            };
            let ms = t0.elapsed().as_millis() as u64;
            match res {
                Ok((msg, data)) => {
                    out.push(json!({ "i": n, "action": action, "status": "success", "message": msg, "data": compact_step_data(&action, data), "ms": ms }));
                    completed += 1;
                    if let Some(exp) = step.get("expect").filter(|e| e.is_object()) {
                        let mut wargs = exp.clone();
                        if wargs.get("timeout_ms").is_none() {
                            wargs["timeout_ms"] = json!(3000);
                        }
                        let left = total_budget.saturating_sub(start.elapsed()).as_millis() as u64;
                        if let Err(e) = self.wait(lane, &wargs, left.max(1500)).await {
                            out.push(json!({ "i": n, "action": "expect", "status": "failed", "code": e.code, "message": e.message, "details": e.details }));
                            if stop_on_error {
                                stopped_at = Some(n);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    out.push(json!({ "i": n, "action": action, "status": "failed", "code": e.code, "message": e.message, "details": e.details, "ms": ms }));
                    if stop_on_error {
                        stopped_at = Some(n);
                        break;
                    }
                }
            }
        }
        let epoch = self.refbook.lane(lane).map(|b| b.epoch);
        Ok((
            format!("{completed}/{} steps completed", steps.len()),
            json!({ "total": steps.len(), "completed": completed, "stopped_at": stopped_at, "steps": out, "epoch": epoch }),
        ))
    }
}

fn tree_title(t: &UINode) -> String {
    t.attributes.name.clone().unwrap_or_default()
}

/// Keep batch results small: a snapshot keeps a short head of its text, a
/// screenshot drops its bytes (take it as its own command when you need it).
fn compact_step_data(action: &str, mut data: Value) -> Value {
    match action {
        "snapshot" => {
            if let Some(Value::String(t)) = data.get("text").cloned() {
                data["text"] = Value::String(truncate_chars(&t, 4000));
            }
            data
        }
        "screenshot" => {
            if data.get("base64").is_some() {
                data["base64"] = Value::Null;
                data["note"] = Value::String("image bytes are not returned inside a batch; call desktop_screenshot on its own".to_string());
            }
            data
        }
        _ => data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        let a = json!({ "ref": " e3 ", "clear": "false", "max_chars": "5000", "n": 7 });
        assert_eq!(arg_str(&a, "ref").as_deref(), Some("e3"));
        assert_eq!(arg_str(&a, "n").as_deref(), Some("7"));
        assert!(!arg_bool(&a, "clear", true));
        assert_eq!(arg_num(&a, "max_chars"), Some(5000.0));
        assert_eq!(clamp_usize(99999.0, 2000, 60000), 60000);
        assert_eq!(normalize_selector("text=Save"), "name:Save");
        assert_eq!(normalize_selector(" role:Button && name:Save "), "role:Button && name:Save");
        assert_eq!(parse_bool_value("Yes"), Some(true));
        assert_eq!(parse_bool_value("maybe"), None);
        let (t, cut) = cap_lines("a\nb\nc\nd\n", 4);
        assert!(cut && t.contains("lines omitted"));
        assert_eq!(cap_lines("short", 100).1, false);
        let d = compact_step_data("screenshot", json!({ "base64": "xx", "width": 1 }));
        assert!(d["base64"].is_null() && d["note"].is_string());
    }

    #[test]
    fn error_mapping() {
        let e: ActionError = AutomationError::ElementNotEnabled("x".into()).into();
        assert_eq!(e.code, "DISABLED");
        let e: ActionError = AutomationError::ElementDetached("x".into()).into();
        assert_eq!(e.code, "STALE_REF");
        let e: ActionError = AutomationError::PlatformError("boom".into()).into();
        assert_eq!(e.code, "EXECUTION_ERROR");
    }
}
