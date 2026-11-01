//! The frozen contract — a JSON-only mirror of the backend's
//! `global/kareenos/messaging/desktop_channel_envelope.js`. The backend's names
//! are authoritative; `messaging_engine/dev/desktop_envelope_selfcheck.js`
//! compares the action table below with the backend's.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const CHANNEL: &str = "desktop";

pub const ACTIONS: &[&str] = &[
    "snapshot", "apps", "open", "focus", "read", "screenshot", "scroll", "wait", "click", "fill",
    "press", "run_steps", "lane_close",
];
pub const READ_CLASS_ACTIONS: &[&str] =
    &["snapshot", "apps", "open", "focus", "read", "screenshot", "scroll", "wait", "lane_close"];
pub const SIDE_EFFECTING_ACTIONS: &[&str] = &["click", "press"];
/// Actions that never fight the person for the mouse or keyboard: they run even
/// while the person is active. Everything else answers HUMAN_ACTIVE first.
pub const HUMAN_SAFE_ACTIONS: &[&str] = &["snapshot", "apps", "read", "screenshot", "wait", "lane_close"];

/// Per-action await windows (ms) — mirrors ACTION_TIMEOUT_MS in the backend.
pub const ACTION_DEFAULT_MS: &[(&str, u64)] = &[
    ("snapshot", 20000),
    ("apps", 15000),
    ("open", 30000),
    ("focus", 10000),
    ("read", 15000),
    ("screenshot", 20000),
    ("scroll", 11000),
    ("wait", 20000),
    ("click", 17000),
    ("fill", 17000),
    ("press", 17000),
    ("run_steps", 290000),
    ("lane_close", 8000),
];
/// The agent answers TIMEOUT itself this long before the server stops listening.
pub const TIMEOUT_MARGIN_MS: u64 = 3000;
/// A command whose window closes within this margin while it waits in the queue is not started.
pub const STALE_MARGIN_MS: u64 = 1500;
/// After answering TIMEOUT, the queue waits at most this long for the overrunning action.
pub const OVERRUN_WAIT_CAP_MS: u64 = 3000;
pub const MAX_RESULT_BYTES: usize = 600 * 1024;
pub const RUN_STEPS_MAX: usize = 25;
pub const RUN_STEPS_ALLOWED: &[&str] =
    &["snapshot", "focus", "read", "screenshot", "scroll", "wait", "click", "fill", "press"];

pub fn is_supported(action: &str) -> bool {
    ACTIONS.contains(&action)
}
pub fn is_read_class(action: &str) -> bool {
    READ_CLASS_ACTIONS.contains(&action)
}
pub fn is_side_effecting(action: &str) -> bool {
    SIDE_EFFECTING_ACTIONS.contains(&action)
}
pub fn is_human_safe(action: &str) -> bool {
    HUMAN_SAFE_ACTIONS.contains(&action)
}
pub fn default_ms(action: &str) -> u64 {
    ACTION_DEFAULT_MS
        .iter()
        .find(|(a, _)| *a == action)
        .map(|(_, ms)| *ms)
        .unwrap_or(15000)
}
/// The budget a tool gets: answer before the server stops waiting.
pub fn budget_for(action: &str, timeout_ms: Option<u64>) -> u64 {
    let base = match timeout_ms {
        Some(t) if t > 0 => t,
        _ => default_ms(action),
    };
    base.saturating_sub(TIMEOUT_MARGIN_MS).max(1000)
}

/// What arrives on the socket as `{ type: "command", ... }`.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandEnvelope {
    pub command_id: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub lane_id: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    pub action: String,
    #[serde(default)]
    pub args: Value,
}

impl CommandEnvelope {
    pub fn correlation(&self) -> String {
        self.correlation_id.clone().unwrap_or_else(|| self.command_id.clone())
    }
    pub fn lane(&self) -> String {
        self.lane_id.clone().unwrap_or_else(|| "default".to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultError {
    pub code: String,
    pub message: String,
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Extension-side timings every result carries (data.timing).
#[derive(Debug, Clone, Serialize, Default)]
pub struct Timing {
    pub received_at: u64,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    pub queued_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_ms: Option<u64>,
    pub queue_position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_ms: Option<u64>,
}

/// The result frame: the platform-wide tool envelope plus the correlation ids.
#[derive(Debug, Clone, Serialize)]
pub struct ResultEnvelope {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub command_id: String,
    pub correlation_id: String,
    pub lane_id: Option<String>,
    pub status: &'static str,
    pub message: String,
    pub data: Value,
    pub errors: Vec<ResultError>,
}

impl ResultEnvelope {
    pub fn success(cmd: &CommandEnvelope, message: impl Into<String>, data: Value) -> Self {
        ResultEnvelope {
            frame_type: "result",
            command_id: cmd.command_id.clone(),
            correlation_id: cmd.correlation(),
            lane_id: cmd.lane_id.clone(),
            status: "success",
            message: message.into(),
            data: if data.is_object() { data } else { json!({}) },
            errors: vec![],
        }
    }

    pub fn failure(cmd: &CommandEnvelope, code: &str, message: impl Into<String>, details: Option<Value>) -> Self {
        let message = message.into();
        let mut data = json!({});
        if let Some(d) = &details {
            data["error_details"] = d.clone();
            if let Some(c) = d.get("candidates") {
                data["candidates"] = c.clone();
            }
            if let Some(r) = d.get("retry_after_ms") {
                data["retry_after_ms"] = r.clone();
            }
        }
        ResultEnvelope {
            frame_type: "result",
            command_id: cmd.command_id.clone(),
            correlation_id: cmd.correlation(),
            lane_id: cmd.lane_id.clone(),
            status: "failed",
            message: format!("[{code}] {message}"),
            data,
            errors: vec![ResultError { code: code.to_string(), message, severity: "error".to_string(), details }],
        }
    }

    pub fn with_timing(mut self, mut timing: Timing) -> Self {
        let now = now_ms();
        if timing.finished_at.is_none() {
            timing.finished_at = Some(now);
        }
        if timing.run_ms.is_none() && timing.started_at > 0 {
            timing.run_ms = Some(now.saturating_sub(timing.started_at));
        }
        if !self.data.is_object() {
            self.data = json!({});
        }
        self.data["timing"] = serde_json::to_value(&timing).unwrap_or(Value::Null);
        self
    }

    pub fn is_success(&self) -> bool {
        self.status == "success"
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_table_is_complete_and_classified() {
        assert_eq!(ACTIONS.len(), 13);
        for a in ACTIONS {
            assert!(default_ms(a) > 0, "{a}");
            let classes = [is_read_class(a), is_side_effecting(a), *a == "fill", *a == "run_steps"]
                .iter()
                .filter(|b| **b)
                .count();
            assert_eq!(classes, 1, "{a} must belong to exactly one class");
        }
        for a in RUN_STEPS_ALLOWED {
            assert!(is_supported(a) && *a != "run_steps" && *a != "open" && *a != "apps" && *a != "lane_close");
        }
        assert!(!is_supported("run_command"));
    }

    #[test]
    fn budgets_and_envelopes() {
        assert_eq!(budget_for("click", None), 14000);
        assert_eq!(budget_for("click", Some(5000)), 2000);
        assert_eq!(budget_for("click", Some(100)), 1000);
        let cmd: CommandEnvelope =
            serde_json::from_str(r#"{"command_id":"c1","action":"click","args":{"ref":"e3"}}"#).unwrap();
        assert_eq!(cmd.correlation(), "c1");
        assert_eq!(cmd.lane(), "default");
        let f = ResultEnvelope::failure(&cmd, "AMBIGUOUS", "two", Some(json!({"candidates":[{"ref":"e1"}]})));
        assert_eq!(f.status, "failed");
        assert!(f.message.starts_with("[AMBIGUOUS]"));
        assert_eq!(f.data["candidates"][0]["ref"], "e1");
        let s = ResultEnvelope::success(&cmd, "ok", json!({"a":1})).with_timing(Timing::default());
        assert!(s.data["timing"]["finished_at"].as_u64().is_some());
    }
}
