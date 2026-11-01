//! The outbound wss client — the twin of the extension's browser-channel-client.
//!
//! One socket at a time to `/desktop-channel`; `bind` with the stored token;
//! the server re-issues the token in `bind_ack` and from its heartbeat
//! (`token` frame), and answers `renew`. A JSON `ping` every 20 s with a 10 s
//! pong deadline; a silent socket is dropped and replaced. Reconnect with
//! exponential backoff (1 s → 30 s). Terminal bind reasons (MISSING_TOKEN,
//! INVALID_TOKEN, EXPIRED, INCOMPLETE_TENANT, REVOKED, WRONG_CHANNEL) and close
//! codes 4000 / 4003 clear the token and stop; 4001 (another computer took the
//! slot) parks; DISABLED (the platform switch) retries slowly. Results produced
//! while the socket is down wait in an outbox and are replayed after the next
//! bind, marked `replayed`.

use crate::adapter::Adapter;
use crate::envelope::{now_ms, CommandEnvelope};
use crate::presence::Presence;
use crate::queue::{ActivityHook, Queue, Sink};
use crate::record::ConnectionRecord;
use crate::store::{self, TokenStore};
use crate::AGENT_VERSION;
use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

const PING_INTERVAL_MS: u64 = 20_000;
const PONG_TIMEOUT_MS: u64 = 10_000;
const CONNECT_TIMEOUT_MS: u64 = 15_000;
const BACKOFF_MIN_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 30_000;
const DISABLED_RETRY_MS: u64 = 30_000;
const SUPERSEDED_RETRY_MS: u64 = 60_000;
const RENEW_MARGIN_SEC: i64 = 7 * 86_400;
const OUTBOX_MAX: usize = 20;
const OUTBOX_TTL_MS: u64 = 90_000;
const TERMINAL_REASONS: &[&str] = &["MISSING_TOKEN", "INVALID_TOKEN", "EXPIRED", "INCOMPLETE_TENANT", "REVOKED", "WRONG_CHANNEL"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    SignedOut,
    Disconnected,
    Connecting,
    Bound,
    Superseded,
    /// The platform switch (desktop_channel_enabled) is off; retrying slowly.
    PlatformOff,
}

#[derive(Debug)]
pub enum Exit {
    SignedOut(String),
    Superseded,
    Shutdown,
}

enum Ended {
    Reconnect(String),
    RetryAfter(u64, String),
    Superseded,
    SignedOut(String),
    Shutdown,
}

/// Observer for a UI (the tray app): called on every connection state change.
pub type StateHook = Arc<dyn Fn(ConnState) + Send + Sync>;

pub struct ClientConfig {
    pub ws_url: String,
    pub install_id: String,
    /// Reconnect after another computer took the slot (the tray's "Use this computer").
    pub take_over: bool,
    /// Connection state changes, for a UI. None in the console binary.
    pub on_state: Option<StateHook>,
    /// Command started / finished, for a UI overlay. None in the console binary.
    pub on_activity: Option<ActivityHook>,
}

/// What the queue and the pause watcher share with the live socket.
pub struct Shared {
    writer: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    outbox: Mutex<Vec<(u64, Value)>>,
    state: Mutex<ConnState>,
    bound: Mutex<bool>,
    on_state: Option<StateHook>,
}

impl Shared {
    fn new(on_state: Option<StateHook>) -> Self {
        Shared { writer: Mutex::new(None), outbox: Mutex::new(vec![]), state: Mutex::new(ConnState::Disconnected), bound: Mutex::new(false), on_state }
    }
    pub fn state(&self) -> ConnState {
        *self.state.lock().unwrap()
    }
    fn set_state(&self, s: ConnState) {
        let changed = {
            let mut st = self.state.lock().unwrap();
            let changed = *st != s;
            if changed {
                tracing::info!(from = ?*st, to = ?s, "state");
            }
            *st = s;
            changed
        };
        // The hook runs outside the lock: a UI may read state() from it.
        if changed {
            if let Some(h) = &self.on_state {
                h(s);
            }
        }
    }
    fn set_writer(&self, w: Option<mpsc::UnboundedSender<Message>>) {
        let none = w.is_none();
        *self.writer.lock().unwrap() = w;
        if none {
            *self.bound.lock().unwrap() = false;
        }
    }
    fn set_bound(&self, b: bool) {
        *self.bound.lock().unwrap() = b;
    }
    /// Send on the live socket; false when there is none.
    pub fn send(&self, v: &Value) -> bool {
        let guard = self.writer.lock().unwrap();
        match guard.as_ref() {
            Some(tx) => tx.send(Message::Text(v.to_string())).is_ok(),
            None => false,
        }
    }
    /// Results that cannot leave wait in the outbox; acks are dropped.
    pub fn send_or_outbox(&self, v: Value) {
        let is_result = v.get("type").and_then(|t| t.as_str()) == Some("result");
        let bound = *self.bound.lock().unwrap();
        if bound && self.send(&v) {
            return;
        }
        if !is_result {
            return;
        }
        let mut ob = self.outbox.lock().unwrap();
        ob.push((now_ms(), v));
        while ob.len() > OUTBOX_MAX {
            ob.remove(0);
        }
        tracing::info!(queued = ob.len(), "result buffered (socket down)");
    }
    fn flush_outbox(&self) {
        let now = now_ms();
        let frames: Vec<Value> = {
            let mut ob = self.outbox.lock().unwrap();
            let all = std::mem::take(&mut *ob);
            all.into_iter().filter(|(at, _)| now.saturating_sub(*at) <= OUTBOX_TTL_MS).map(|(at, mut v)| {
                v["replayed"] = json!(true);
                v["replayed_after_ms"] = json!(now.saturating_sub(at));
                v
            }).collect()
        };
        if !frames.is_empty() {
            tracing::info!(n = frames.len(), "results replayed after reconnect");
            for f in frames {
                self.send(&f);
            }
        }
    }
}

pub fn sink_for(shared: Arc<Shared>) -> Sink {
    Arc::new(move |frame: Value| shared.send_or_outbox(frame))
}

fn client_info(install_id: &str) -> Value {
    json!({
        "version": AGENT_VERSION,
        "os": format!("{}/{}", std::env::consts::OS, std::env::consts::FAMILY),
        "arch": std::env::consts::ARCH,
        "hostname": hostname::get().ok().and_then(|h| h.into_string().ok()).unwrap_or_default(),
        "install_id": install_id,
    })
}

/// Run until signed out, superseded (without take_over) or shut down.
pub async fn run(
    store: Arc<TokenStore>,
    mut record: ConnectionRecord,
    cfg: ClientConfig,
    adapter: Arc<Adapter>,
    presence: Arc<Presence>,
    shutdown: CancellationToken,
) -> Exit {
    let shared = Arc::new(Shared::new(cfg.on_state.clone()));
    let queue = Queue::spawn_with_hook(adapter.clone(), sink_for(shared.clone()), cfg.on_activity.clone());

    // The pause flag (tray / file) → state frames.
    {
        let shared = shared.clone();
        let presence = presence.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                let on = store::is_paused_flag_set();
                if on != presence.is_paused() {
                    presence.set_paused(on);
                    tracing::info!(paused = on, "pause flag changed");
                    shared.send(&json!({ "type": "state", "paused": on, "human_active": presence.human_active().is_some() }));
                }
                tokio::time::sleep(Duration::from_millis(2000)).await;
            }
        });
    }

    let mut backoff = BACKOFF_MIN_MS;
    loop {
        if shutdown.is_cancelled() {
            return Exit::Shutdown;
        }
        shared.set_state(ConnState::Connecting);
        let ended = connect_once(&shared, &store, &mut record, &cfg, &adapter, &presence, &queue, &shutdown).await;
        shared.set_writer(None);
        // A connection that bound resets the backoff: the next failure starts the ladder again.
        if let Ok((_, true)) = &ended {
            backoff = BACKOFF_MIN_MS;
        }
        let ended = ended.map(|(e, _)| e);
        match ended {
            Ok(Ended::Shutdown) => return Exit::Shutdown,
            Ok(Ended::SignedOut(reason)) => {
                shared.set_state(ConnState::SignedOut);
                if let Err(e) = store.clear() {
                    tracing::warn!(error = %e, "could not clear the stored sign-in");
                }
                return Exit::SignedOut(reason);
            }
            Ok(Ended::Superseded) => {
                shared.set_state(ConnState::Superseded);
                if !cfg.take_over {
                    return Exit::Superseded;
                }
                tracing::warn!("superseded — retrying in {} s (take-over mode)", SUPERSEDED_RETRY_MS / 1000);
                sleep_or_shutdown(SUPERSEDED_RETRY_MS, &shutdown).await;
                backoff = BACKOFF_MIN_MS;
            }
            Ok(Ended::RetryAfter(ms, why)) => {
                if shared.state() != ConnState::PlatformOff {
                    shared.set_state(ConnState::Disconnected);
                }
                tracing::warn!(retry_in_ms = ms, "{}", why);
                sleep_or_shutdown(ms, &shutdown).await;
            }
            Ok(Ended::Reconnect(why)) => {
                shared.set_state(ConnState::Disconnected);
                let jitter = (now_ms() % (backoff / 3 + 1)) as u64;
                tracing::warn!(reconnect_in_ms = backoff + jitter, "{}", why);
                sleep_or_shutdown(backoff + jitter, &shutdown).await;
                backoff = (backoff * 2).min(BACKOFF_MAX_MS);
            }
            Err(e) => {
                shared.set_state(ConnState::Disconnected);
                let jitter = (now_ms() % (backoff / 3 + 1)) as u64;
                tracing::warn!(error = %e, reconnect_in_ms = backoff + jitter, "connect failed");
                sleep_or_shutdown(backoff + jitter, &shutdown).await;
                backoff = (backoff * 2).min(BACKOFF_MAX_MS);
            }
        }
    }
}

async fn sleep_or_shutdown(ms: u64, shutdown: &CancellationToken) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms)) => {},
        _ = shutdown.cancelled() => {},
    }
}

fn persist_renewal(store: &TokenStore, record: &mut ConnectionRecord, frame: &Value) {
    let mut changed = false;
    if let Some(t) = frame.get("token").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
        record.token = t.to_string();
        changed = true;
    }
    if let Some(e) = frame.get("exp").and_then(|e| e.as_i64()) {
        record.exp = Some(e);
        changed = true;
    }
    if let Some(r) = frame.get("renewable").and_then(|r| r.as_bool()) {
        record.renewable = Some(r);
        changed = true;
    }
    if changed {
        if let Err(e) = store.save(record) {
            tracing::warn!(error = %e, "could not persist the renewed token (keeping it in memory)");
        } else {
            tracing::info!(exp = ?record.exp, renewable = ?record.renewable, "token record saved");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_once(
    shared: &Arc<Shared>,
    store: &Arc<TokenStore>,
    record: &mut ConnectionRecord,
    cfg: &ClientConfig,
    adapter: &Arc<Adapter>,
    presence: &Arc<Presence>,
    queue: &Queue,
    shutdown: &CancellationToken,
) -> Result<(Ended, bool)> {
    tracing::info!(url = %cfg.ws_url, "connecting");
    let (ws, _resp) = tokio::time::timeout(Duration::from_millis(CONNECT_TIMEOUT_MS), connect_async(cfg.ws_url.as_str()))
        .await
        .map_err(|_| anyhow!("connect timed out after {} s", CONNECT_TIMEOUT_MS / 1000))?
        .context("websocket connect failed")?;
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    shared.set_writer(Some(tx.clone()));

    let bind = json!({ "type": "bind", "token": record.token, "client": client_info(&cfg.install_id) });
    let _ = tx.send(Message::Text(bind.to_string()));

    let mut bound = false;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(PING_INTERVAL_MS), Duration::from_millis(PING_INTERVAL_MS));
    let mut pong_deadline: Option<tokio::time::Instant> = None;

    let outcome: Ended = loop {
        // By value (Instant is Copy): the ping arm below re-arms the deadline.
        let pong_wait = async move {
            match pong_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = shutdown.cancelled() => { break Ended::Shutdown; }
            _ = ping.tick() => {
                if bound {
                    let _ = tx.send(Message::Text(json!({ "type": "ping" }).to_string()));
                    if pong_deadline.is_none() {
                        pong_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(PONG_TIMEOUT_MS));
                    }
                }
            }
            _ = pong_wait => { break Ended::Reconnect(format!("no pong within {} s — socket presumed dead", PONG_TIMEOUT_MS / 1000)); }
            msg = stream.next() => {
                match msg {
                    None => break Ended::Reconnect("socket ended".to_string()),
                    Some(Err(e)) => break Ended::Reconnect(format!("socket error: {e}")),
                    Some(Ok(Message::Text(text))) => {
                        let frame: Value = match serde_json::from_str(&text) { Ok(v) => v, Err(_) => continue };
                        match frame.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                            "bind_ack" => {
                                if frame.get("ok").and_then(|o| o.as_bool()) == Some(true) {
                                    persist_renewal(store, record, &frame);
                                    if let Some(settings) = frame.get("settings") {
                                        adapter.apply_settings(settings);
                                    }
                                    bound = true;
                                    shared.set_bound(true);
                                    shared.set_state(ConnState::Bound);
                                    tracing::info!(instance = ?frame.get("instance_id"), renewed = frame.get("token").is_some(), exp = ?record.exp, "bound — waiting for commands ({})", record.label());
                                    shared.flush_outbox();
                                    // Tell the server about a pause that was already on.
                                    if presence.is_paused() {
                                        shared.send(&json!({ "type": "state", "paused": true, "human_active": false }));
                                    }
                                    let now_secs = chrono::Utc::now().timestamp();
                                    if record.renewable != Some(false) && record.seconds_to_expiry(now_secs).map(|s| s < RENEW_MARGIN_SEC).unwrap_or(true) {
                                        let _ = tx.send(Message::Text(json!({ "type": "renew" }).to_string()));
                                    }
                                } else {
                                    let reason = frame.get("reason").and_then(|r| r.as_str()).unwrap_or("UNKNOWN").to_string();
                                    tracing::warn!(reason = %reason, "bind rejected");
                                    if TERMINAL_REASONS.contains(&reason.as_str()) {
                                        break Ended::SignedOut(reason);
                                    }
                                    if reason == "DISABLED" {
                                        shared.set_state(ConnState::PlatformOff);
                                        break Ended::RetryAfter(DISABLED_RETRY_MS, "the Desktop Channel is off on the platform (desktop_channel_enabled); waiting".to_string());
                                    }
                                    break Ended::RetryAfter(5_000, format!("bind refused ({reason}) — retrying"));
                                }
                            }
                            "command" => {
                                match serde_json::from_value::<CommandEnvelope>(frame.clone()) {
                                    Ok(cmd) => {
                                        let id = cmd.command_id.clone();
                                        let (pos, received_at) = queue.push(cmd);
                                        let _ = tx.send(Message::Text(json!({ "type": "ack", "command_id": id, "phase": "received", "at": received_at, "queue_position": pos }).to_string()));
                                    }
                                    Err(e) => tracing::warn!(error = %e, "malformed command frame ignored"),
                                }
                            }
                            "token" => persist_renewal(store, record, &frame),
                            "pong" => { pong_deadline = None; }
                            "superseded" => {
                                tracing::warn!(by = ?frame.get("by"), "another computer took this project's slot");
                                break Ended::Superseded;
                            }
                            "revoked" => break Ended::SignedOut("REVOKED".to_string()),
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        let reason = frame.as_ref().map(|f| f.reason.to_string()).unwrap_or_default();
                        tracing::info!(code = ?code, reason = %reason, "socket closed by the server");
                        break match code {
                            Some(4000) => Ended::SignedOut("signed_out".to_string()),
                            Some(4003) => Ended::SignedOut("revoked".to_string()),
                            Some(4001) => Ended::Superseded,
                            Some(c) => Ended::Reconnect(format!("closed {c} {reason}")),
                            None => Ended::Reconnect("closed".to_string()),
                        };
                    }
                    Some(Ok(Message::Ping(p))) => { let _ = tx.send(Message::Pong(p)); }
                    Some(Ok(_)) => {}
                }
            }
        }
    };

    shared.set_bound(false);
    shared.set_writer(None);
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    Ok((outcome, bound))
}
