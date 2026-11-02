#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Kareenos Desktop Agent — the tray application (Kareenos Desktop Channel §5.134, Phase 2).
//!
//! A thin shell around the `kareenos_desktop_agent` library: the tray icon and
//! its menu, three small windows (welcome / sign-in, diagnostics, the activity
//! overlay), the `kareenos-desktop://connect` deep link, start-with-Windows and,
//! in release builds, the updater. Everything that talks to the platform or to
//! Windows UI Automation lives in the library; this crate never opens a port.

mod agent;
mod trail;

use agent::{Agent, Status};
use kareenos_desktop_agent::record::ConnectionRecord;
use kareenos_desktop_agent::{store, URL_SCHEME};
use std::sync::{Arc, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_clipboard_manager::ClipboardExt as _;
use tauri_plugin_deep_link::DeepLinkExt as _;
use tauri_plugin_opener::OpenerExt as _;

const TRAY_ID: &str = "kareenos-main";
const DOCS_URL: &str = "https://kareenos.com/en/docs/kareenos/desktop-agent";
const OVERLAY_W: f64 = 380.0;
const OVERLAY_H: f64 = 48.0;

/// The tray menu items the status keeps current.
struct TrayItems {
    status: MenuItem<Wry>,
    sign_in: MenuItem<Wry>,
    take_over: MenuItem<Wry>,
    pause: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// Brand v5 (decision 21): the mark with a corner state dot. One PNG per state,
/// rendered from the tokens by scripts/brand-assets.cjs.
fn tray_icon_for(s: &Status) -> tauri::image::Image<'static> {
    if s.busy.is_some() {
        return tauri::include_image!("icons/tray-working.png");
    }
    match s.state.as_str() {
        "connected" if s.locked || s.paused => tauri::include_image!("icons/tray-paused.png"),
        "connected" => tauri::include_image!("icons/tray-idle.png"),
        "error" | "signed_out" => tauri::include_image!("icons/tray-error.png"),
        "superseded" => tauri::include_image!("icons/tray-paused.png"),
        _ => tauri::include_image!("icons/tray-offline.png"),
    }
}

/// Which view the main window should open with (read once by the page).
struct UiState {
    mode: Mutex<String>,
}

fn set_mode(app: &AppHandle, mode: &str) {
    if let Some(ui) = app.try_state::<UiState>() {
        *ui.mode.lock().unwrap() = mode.to_string();
    }
}

/// Push a status to the tray, every window and the overlay.
fn apply_status(app: &AppHandle, s: &Status) {
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.status.set_text(s.tray_line());
        let _ = items.sign_in.set_text(if s.signed_in { "Sign in again…" } else { "Sign in…" });
        let _ = items.sign_in.set_enabled(!s.managed);
        let _ = items.take_over.set_enabled(s.state == "superseded");
        let _ = items.pause.set_checked(s.paused);
        let _ = items.quit.set_enabled(!s.managed);
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(format!("Kareenos Desktop Agent — {}", s.tray_line())));
        let _ = tray.set_icon(Some(tray_icon_for(s)));
    }
    let _ = app.emit("status", s);
    if let Some(overlay) = app.get_webview_window("overlay") {
        if s.busy.is_some() {
            if let Ok(Some(m)) = app.primary_monitor() {
                let sf = m.scale_factor();
                let size = m.size();
                let x = size.width as i32 - ((OVERLAY_W + 24.0) * sf) as i32;
                let y = (24.0 * sf) as i32;
                let _ = overlay.set_position(tauri::PhysicalPosition::new(x.max(0), y));
            }
            let _ = overlay.show();
        } else {
            let _ = overlay.hide();
        }
    }
}

fn show_main(app: &AppHandle, mode: &str) {
    set_mode(app, mode);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.emit("mode", mode);
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    if let Err(e) = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Kareenos Desktop Agent")
        .inner_size(560.0, 660.0)
        .resizable(false)
        .center()
        .build()
    {
        tracing::warn!(error = %e, "could not open the main window");
    }
}

fn show_diagnostics(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("diagnostics") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    if let Err(e) = WebviewWindowBuilder::new(app, "diagnostics", WebviewUrl::App("diagnostics.html".into()))
        .title("Kareenos Desktop Agent — Diagnostics")
        .inner_size(760.0, 640.0)
        .center()
        .build()
    {
        tracing::warn!(error = %e, "could not open the diagnostics window");
    }
}

/// A link or a pasted code became a sign-in.
fn handle_link(app: &AppHandle, link: &str) {
    match ConnectionRecord::from_any(link.trim()) {
        Ok(rec) => {
            let agent = app.state::<Arc<Agent>>().inner().clone();
            agent.sign_in(rec);
            let _ = app.autolaunch().enable();
            show_main(app, "connected");
        }
        Err(e) => {
            tracing::warn!(error = %e, "ignored a link that is not a connection record");
            show_main(app, "signin");
        }
    }
}

fn check_updates_now(app: AppHandle) {
    #[cfg(feature = "updater")]
    {
        use tauri_plugin_updater::UpdaterExt as _;
        tauri::async_runtime::spawn(async move {
            let agent = app.state::<Arc<Agent>>().inner().clone();
            let outcome: Result<String, String> = async {
                let updater = app.updater().map_err(|e| e.to_string())?;
                match updater.check().await.map_err(|e| e.to_string())? {
                    None => Ok("You have the latest version.".to_string()),
                    Some(update) => {
                        let _ = app.emit("update", format!("Version {} is available; it installs when the agent is idle.", update.version));
                        // The 1.3.3 rule: never while a command runs, and only after 45 s of quiet.
                        let mut waited = 0u64;
                        while !agent.idle_for(45_000) && waited < 30 * 60_000 {
                            tokio::time::sleep(std::time::Duration::from_millis(5_000)).await;
                            waited += 5_000;
                        }
                        update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
                        app.restart()
                    }
                }
            }
            .await;
            let msg = match outcome {
                Ok(m) => m,
                Err(e) => format!("Update check failed: {e}"),
            };
            tracing::info!("{msg}");
            let _ = app.emit("update", msg);
        });
    }
    #[cfg(not(feature = "updater"))]
    {
        let _ = app.emit("update", "This build has no automatic updates. Download the latest installer from kareenos.com → Downloads.");
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    let agent = app.state::<Arc<Agent>>().inner().clone();
    match id {
        "status" => show_main(app, "status"),
        "sign_in" => show_main(app, "signin"),
        "take_over" => agent.take_over(),
        "pause" => {
            let on = !agent.status().paused;
            agent.set_paused(on);
        }
        "diagnostics" => show_diagnostics(app),
        "updates" => {
            show_diagnostics(app);
            check_updates_now(app.clone());
        }
        "help" => {
            let _ = app.opener().open_url(DOCS_URL, None::<&str>);
        }
        "quit" => {
            if agent.status().managed {
                return;   // an IT-managed computer: the service keeps the agent alive
            }
            agent.quit();
            app.exit(0);
        }
        _ => {}
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Starting…", true, None::<&str>)?;
    let sign_in = MenuItem::with_id(app, "sign_in", "Sign in…", true, None::<&str>)?;
    let take_over = MenuItem::with_id(app, "take_over", "Use this computer", false, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause agents", true, store::is_paused_flag_set(), None::<&str>)?;
    let diagnostics = MenuItem::with_id(app, "diagnostics", "Diagnostics…", true, None::<&str>)?;
    let updates = MenuItem::with_id(app, "updates", "Check for updates", true, None::<&str>)?;
    let help = MenuItem::with_id(app, "help", "Help", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let quit_ref = quit.clone();
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &sign_in,
            &take_over,
            &pause,
            &PredefinedMenuItem::separator(app)?,
            &diagnostics,
            &updates,
            &help,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::include_image!("icons/tray-offline.png"))
        .tooltip("Kareenos Desktop Agent")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .build(app)?;
    app.manage(TrayItems { status, sign_in, take_over, pause, quit: quit_ref });
    Ok(())
}

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = tracing_appender::rolling::daily(store::logs_dir(), "app.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file);
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(file_writer).with_ansi(false).with_target(false))
        .with(trail::TrailLayer);
    if subscriber.try_init().is_err() {
        return None;
    }
    Some(guard)
}

// ---- commands the pages call -------------------------------------------------

#[tauri::command]
fn get_status(agent: State<'_, Arc<Agent>>) -> Status {
    agent.status()
}

#[tauri::command]
fn take_mode(ui: State<'_, UiState>) -> String {
    std::mem::take(&mut *ui.mode.lock().unwrap())
}

#[tauri::command]
fn get_trail() -> Vec<trail::Entry> {
    trail::snapshot()
}

#[tauri::command]
fn sign_in(app: AppHandle, agent: State<'_, Arc<Agent>>, input: String) -> Result<Status, String> {
    let rec = ConnectionRecord::from_any(input.trim()).map_err(|e| format!("That is not a Kareenos Desktop Agent connection code or link ({e})."))?;
    agent.sign_in(rec);
    let _ = app.autolaunch().enable();
    Ok(agent.status())
}

#[tauri::command]
fn sign_out(app: AppHandle, agent: State<'_, Arc<Agent>>) -> Status {
    agent.sign_out();
    let _ = app.autolaunch().disable();
    agent.status()
}

#[tauri::command]
fn set_paused(agent: State<'_, Arc<Agent>>, on: bool) -> Status {
    agent.set_paused(on);
    agent.status()
}

#[tauri::command]
fn take_over(agent: State<'_, Arc<Agent>>) -> Status {
    agent.take_over();
    agent.status()
}

#[tauri::command]
fn open_link(app: AppHandle, agent: State<'_, Arc<Agent>>, which: String) -> Result<(), String> {
    let s = agent.status();
    let r = match which.as_str() {
        "connect" => app.opener().open_url(format!("{}/kareenos/connectextension?desktop=1", s.app_url), None::<&str>),
        "docs" => app.opener().open_url(DOCS_URL, None::<&str>),
        "logs" => app.opener().open_path(s.logs_dir.clone(), None::<&str>),
        "data" => app.opener().open_path(s.data_dir.clone(), None::<&str>),
        _ => return Err("unknown link".to_string()),
    };
    r.map_err(|e| e.to_string())
}

#[tauri::command]
fn read_clipboard(app: AppHandle) -> Result<String, String> {
    app.clipboard().read_text().map_err(|e| e.to_string())
}

#[tauri::command]
fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

#[tauri::command]
fn check_updates(app: AppHandle) {
    check_updates_now(app);
}

// async on purpose: a window built from inside a SYNCHRONOUS command blocks the
// main thread on Windows (the build waits for the event loop the command is
// holding) — the 0.2.0 "blank Diagnostics window and the app is stuck". An async
// command runs off the main thread and the build is dispatched to it.
#[tauri::command]
async fn open_diagnostics(app: AppHandle) {
    show_diagnostics(&app);
}

#[tauri::command]
fn close_window(window: tauri::WebviewWindow) {
    let _ = window.close();
}

/// Brand review (`--brand-shots`, CI): the four surfaces at fixed places, the
/// overlay with a sample line; the runner screenshots the desktop after 10 s.
fn brand_shots(app: &AppHandle) {
    let agent = app.state::<Arc<Agent>>().inner().clone();
    show_main(app, "welcome");
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_position(tauri::PhysicalPosition::new(40, 40));
    }
    show_diagnostics(app);
    if let Some(w) = app.get_webview_window("diagnostics") {
        let _ = w.set_position(tauri::PhysicalPosition::new(640, 40));
    }
    agent.demo_busy("Sara's Invoice Clerk · Typing into Name");
}

fn main() {
    let _log_guard = init_logging();
    let autostarted = std::env::args().any(|a| a == "--autostart");
    // Wave 2: started by the Kareenos Desktop Service inside the signed-in session.
    let service_child = std::env::args().any(|a| a == "--service-child");
    let shots = std::env::args().any(|a| a == "--brand-shots");
    let positional = std::env::args().nth(1).filter(|a| !a.starts_with("--"));

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A second launch lands here. A deep link in argv is forwarded to the
            // deep-link plugin by its feature; anything else just shows the window.
            if !argv.iter().skip(1).any(|a| a.starts_with(URL_SCHEME)) {
                show_main(app, "status");
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--autostart"])));
    #[cfg(feature = "updater")]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    let app = builder
        .manage(UiState { mode: Mutex::new(String::new()) })
        .invoke_handler(tauri::generate_handler![
            get_status,
            take_mode,
            get_trail,
            sign_in,
            sign_out,
            set_paused,
            take_over,
            open_link,
            read_clipboard,
            copy_text,
            check_updates,
            open_diagnostics,
            close_window
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            build_tray(&handle)?;
            // The overlay exists from the start, hidden and click-through; the
            // status callback shows it while a command runs.
            match WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
                .title("Kareenos")
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .focused(false)
                .visible(false)
                .inner_size(OVERLAY_W, OVERLAY_H)
                .build()
            {
                Ok(overlay) => {
                    let _ = overlay.set_ignore_cursor_events(true);
                }
                Err(e) => tracing::warn!(error = %e, "no overlay window"),
            }
            let cb = handle.clone();
            let agent = Agent::start(move |s: Status| apply_status(&cb, &s));
            app.manage(agent.clone());

            // kareenos-desktop://connect?p=… — registered by the installer and, for
            // a portable or development run, here as well (per user, no admin).
            #[cfg(any(windows, target_os = "linux"))]
            {
                if let Err(e) = app.deep_link().register_all() {
                    tracing::warn!(error = %e, "deep link registration failed (the installer registers it too)");
                }
            }
            let link_handle = handle.clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    handle_link(&link_handle, url.as_str());
                }
            });
            let mut opened = false;
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    handle_link(&handle, url.as_str());
                    opened = true;
                }
            }
            if !opened {
                if let Some(p) = positional.clone() {
                    handle_link(&handle, &p);
                    opened = true;
                }
            }
            if service_child {
                agent.set_managed(true);
            }
            // Wave 2: a silent install left a one-time connection code (HKLM); with no
            // stored sign-in, bind with it once. The library marks it used when the token arrives.
            if !opened && !agent.status().signed_in {
                if let Some(code) = store::pending_connect_code() {
                    match ConnectionRecord::from_any(&code) {
                        Ok(rec) => {
                            tracing::info!("signing in with the installer's connection code ({})", rec.label());
                            agent.sign_in(rec);
                            opened = true;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "the installer's connection code is not a connection record");
                            store::mark_connect_code_used(&code);
                        }
                    }
                }
            }
            if !opened && !autostarted && !service_child && !agent.status().signed_in {
                show_main(&handle, "welcome");
            }
            if shots {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    brand_shots(&h);
                });
            }
            tracing::info!(version = kareenos_desktop_agent::AGENT_VERSION, autostarted, service_child, "Kareenos Desktop Agent app started");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Kareenos Desktop Agent");

    app.run(|_app, event| {
        // Closing a window never quits the app; only the tray's Quit does.
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}
