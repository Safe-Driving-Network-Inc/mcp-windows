//! The diagnostics trail: the last 80 log lines kept in memory for the
//! Diagnostics window and for "Copy diagnostics". Never a token: the library
//! never logs one, and this layer records only what was logged.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};

const CAP: usize = 80;

static TRAIL: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Unix milliseconds.
    pub at: i64,
    pub level: String,
    pub text: String,
}

pub fn snapshot() -> Vec<Entry> {
    TRAIL.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default()
}

pub fn push(level: &str, text: String) {
    if let Ok(mut t) = TRAIL.lock() {
        t.push_back(Entry { at: chrono::Utc::now().timestamp_millis(), level: level.to_string(), text });
        while t.len() > CAP {
            t.pop_front();
        }
    }
}

/// A `tracing` layer that mirrors every event into the ring.
pub struct TrailLayer;

struct Collect {
    message: String,
    fields: Vec<String>,
}

impl Visit for Collect {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for TrailLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut c = Collect { message: String::new(), fields: vec![] };
        event.record(&mut c);
        let mut text = c.message;
        if !c.fields.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&c.fields.join(" "));
        }
        push(&event.metadata().level().to_string(), text);
    }
}
