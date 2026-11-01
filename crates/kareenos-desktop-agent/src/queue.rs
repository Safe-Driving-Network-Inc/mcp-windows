//! One queue per computer. A desktop is one keyboard, one mouse and one focus,
//! so the commands of EVERY lane run one at a time, in arrival order, with the
//! deadline semantics of extension 1.3.6:
//!   * `received` ack at once (with the queue position), `started` when it runs;
//!   * a command whose window has closed while it waited is NOT started
//!     (TIMEOUT, not_started: true — safe to retry);
//!   * the budget is what is left of the window from now, never more than the
//!     action's own budget;
//!   * an action that overruns is answered TIMEOUT (started: true) and the
//!     queue waits only a short grace for it — it runs on its own thread, so a
//!     stuck UIA call never freezes the socket or the pings.

use crate::adapter::Adapter;
use crate::envelope::{self, now_ms, CommandEnvelope, ResultEnvelope, Timing, OVERRUN_WAIT_CAP_MS, STALE_MARGIN_MS, TIMEOUT_MARGIN_MS};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Where frames go: the live socket when bound, else the client's outbox.
pub type Sink = Arc<dyn Fn(Value) + Send + Sync>;

pub struct Queued {
    pub cmd: CommandEnvelope,
    pub received_at: u64,
    pub deadline_at: u64,
    pub queue_position: usize,
}

#[derive(Clone)]
pub struct Queue {
    tx: mpsc::UnboundedSender<Queued>,
    depth: Arc<AtomicUsize>,
    pub in_flight: Arc<AtomicUsize>,
    pub last_done_at: Arc<AtomicU64>,
}

impl Queue {
    pub fn spawn(adapter: Arc<Adapter>, sink: Sink) -> Queue {
        let (tx, rx) = mpsc::unbounded_channel::<Queued>();
        let depth = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let last_done_at = Arc::new(AtomicU64::new(0));
        tokio::spawn(consume(rx, adapter, sink, depth.clone(), in_flight.clone(), last_done_at.clone()));
        Queue { tx, depth, in_flight, last_done_at }
    }

    /// Enqueue; returns (queue position, received_at) for the `received` ack.
    pub fn push(&self, cmd: CommandEnvelope) -> (usize, u64) {
        let received_at = now_ms();
        let window = match cmd.timeout_ms {
            Some(t) if t > 0 => t,
            _ => envelope::default_ms(&cmd.action),
        };
        let position = self.depth.fetch_add(1, Ordering::SeqCst);
        let q = Queued { cmd, received_at, deadline_at: received_at + window, queue_position: position };
        if self.tx.send(q).is_err() {
            self.depth.fetch_sub(1, Ordering::SeqCst);
        }
        (position, received_at)
    }

    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::SeqCst)
    }
    pub fn idle_for_ms(&self) -> u64 {
        if self.in_flight.load(Ordering::SeqCst) > 0 || self.depth() > 0 {
            return 0;
        }
        now_ms().saturating_sub(self.last_done_at.load(Ordering::SeqCst))
    }
}

fn ack(sink: &Sink, command_id: &str, phase: &str, at: u64, queue_position: usize) {
    sink(json!({ "type": "ack", "command_id": command_id, "phase": phase, "at": at, "queue_position": queue_position }));
}

async fn consume(
    mut rx: mpsc::UnboundedReceiver<Queued>,
    adapter: Arc<Adapter>,
    sink: Sink,
    depth: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    last_done_at: Arc<AtomicU64>,
) {
    while let Some(q) = rx.recv().await {
        in_flight.fetch_add(1, Ordering::SeqCst);
        let started = now_ms();
        let action = q.cmd.action.clone();
        let mut timing = Timing {
            received_at: q.received_at,
            started_at: started,
            queued_ms: started.saturating_sub(q.received_at),
            queue_position: q.queue_position,
            ..Default::default()
        };
        if started + STALE_MARGIN_MS > q.deadline_at {
            tracing::warn!(command = %q.cmd.command_id, action = %action, queued_ms = timing.queued_ms, "stale: window closed in the queue — not executed");
            let env = ResultEnvelope::failure(
                &q.cmd,
                "TIMEOUT",
                format!(
                    "The {action} action waited {} ms in the queue behind earlier commands and its window had passed before it could start; it did NOT run. Retry it now that the queue is free.",
                    timing.queued_ms
                ),
                Some(json!({ "not_started": true, "action": action, "queued_ms": timing.queued_ms })),
            )
            .with_timing(timing);
            sink(serde_json::to_value(&env).unwrap_or(Value::Null));
        } else {
            ack(&sink, &q.cmd.command_id, "started", started, 0);
            let remaining = q.deadline_at.saturating_sub(started).saturating_sub(TIMEOUT_MARGIN_MS);
            let budget = envelope::budget_for(&action, q.cmd.timeout_ms).min(remaining).max(1000);
            timing.budget_ms = Some(budget);
            tracing::info!(command = %q.cmd.command_id, action = %action, lane = %q.cmd.lane(), timeout_ms = ?q.cmd.timeout_ms, budget_ms = budget, queued_ms = timing.queued_ms, "run");
            let adapter2 = adapter.clone();
            let cmd2 = q.cmd.clone();
            // Its own thread + its own small runtime: UIA calls block, and a stuck
            // one must never block the socket runtime (pings, acks, other frames).
            let mut handle = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("action runtime");
                rt.block_on(adapter2.execute(&cmd2, budget))
            });
            match tokio::time::timeout(Duration::from_millis(budget + 500), &mut handle).await {
                Ok(Ok(env)) => sink(serde_json::to_value(&env.with_timing(timing)).unwrap_or(Value::Null)),
                Ok(Err(join)) => {
                    tracing::error!(command = %q.cmd.command_id, error = %join, "action task failed");
                    let env = ResultEnvelope::failure(&q.cmd, "EXECUTION_ERROR", format!("The {action} action crashed: {join}"), None).with_timing(timing);
                    sink(serde_json::to_value(&env).unwrap_or(Value::Null));
                }
                Err(_) => {
                    tracing::warn!(command = %q.cmd.command_id, action = %action, budget_ms = budget, "timeout: action overran its budget");
                    let env = ResultEnvelope::failure(
                        &q.cmd,
                        "TIMEOUT",
                        format!("The {action} action did not finish within {budget} ms on the computer. It was dispatched and may have taken effect; snapshot the window before repeating it."),
                        Some(json!({ "budget_ms": budget, "action": action, "started": true })),
                    )
                    .with_timing(timing);
                    sink(serde_json::to_value(&env).unwrap_or(Value::Null));
                    let _ = tokio::time::timeout(Duration::from_millis(OVERRUN_WAIT_CAP_MS), &mut handle).await;
                }
            }
        }
        depth.fetch_sub(1, Ordering::SeqCst);
        in_flight.fetch_sub(1, Ordering::SeqCst);
        last_done_at.store(now_ms(), Ordering::SeqCst);
    }
}
