//! The supervisor: owns the library's channel client for the life of the app.
//!
//! One task. It loads the stored sign-in, creates the UI Automation desktop
//! once, runs `client::run` and reacts to the tray / windows (sign in, sign
//! out, use this computer, quit). Every change of state is pushed to the UI
//! through `on_change`; the UI never polls.

use kareenos_desktop_agent::adapter::Adapter;
use kareenos_desktop_agent::client::{self, ClientConfig, ConnState, Exit};
use kareenos_desktop_agent::presence::Presence;
use kareenos_desktop_agent::queue::Activity;
use kareenos_desktop_agent::record::ConnectionRecord;
use kareenos_desktop_agent::store::{self, TokenStore};
use kareenos_desktop_agent::{policy, AGENT_VERSION, DEFAULT_WS_URL};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The SPA host: the connect page lives there. The API host comes from the sign-in record.
pub const DEFAULT_APP_URL: &str = "https://kareenos.com";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Everything the tray, the windows and "Copy diagnostics" show.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// signed_out | connecting | connected | offline | superseded | platform_off | error
    pub state: String,
    pub signed_in: bool,
    pub label: String,
    pub project: String,
    pub account: String,
    pub user: String,
    pub server: String,
    pub app_url: String,
    pub version: String,
    pub token_expires: Option<String>,
    pub renewable: Option<bool>,
    pub paused: bool,
    /// The overlay line while a command runs.
    pub busy: Option<String>,
    pub last_error: Option<String>,
    pub last_result_code: Option<String>,
    pub commands_done: u64,
    pub last_command_at: Option<i64>,
    pub install_id: String,
    pub hostname: String,
    pub data_dir: String,
    pub logs_dir: String,
    pub policy_path: String,
    pub updated_at: i64,
}

impl Status {
    fn blank() -> Status {
        Status {
            state: "signed_out".into(),
            signed_in: false,
            label: String::new(),
            project: String::new(),
            account: String::new(),
            user: String::new(),
            server: DEFAULT_WS_URL.to_string(),
            app_url: DEFAULT_APP_URL.to_string(),
            version: AGENT_VERSION.to_string(),
            token_expires: None,
            renewable: None,
            paused: false,
            busy: None,
            last_error: None,
            last_result_code: None,
            commands_done: 0,
            last_command_at: None,
            install_id: store::install_id(),
            hostname: hostname::get().ok().and_then(|h| h.into_string().ok()).unwrap_or_default(),
            data_dir: store::data_dir().display().to_string(),
            logs_dir: store::logs_dir().display().to_string(),
            policy_path: policy::path().display().to_string(),
            updated_at: now_ms(),
        }
    }

    fn set_signed_out(&mut self) {
        self.state = "signed_out".into();
        self.signed_in = false;
        self.label.clear();
        self.project.clear();
        self.account.clear();
        self.user.clear();
        self.token_expires = None;
        self.renewable = None;
        self.busy = None;
    }

    fn apply_record(&mut self, r: &ConnectionRecord) {
        self.signed_in = true;
        self.label = r.label();
        self.project = r.project_name.clone().or_else(|| r.projectid.clone()).unwrap_or_default();
        self.account = r.account_name.clone().or_else(|| r.accountid.clone()).unwrap_or_default();
        self.user = r.user_name.clone().or_else(|| r.userid.clone()).unwrap_or_default();
        self.token_expires = r.exp.and_then(|e| chrono::DateTime::from_timestamp(e, 0)).map(|d| d.to_rfc3339());
        self.renewable = r.renewable;
        self.app_url = r
            .api_url
            .as_deref()
            .map(|u| u.trim_end_matches('/').trim_end_matches("/api").to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_APP_URL.to_string());
    }

    /// The one line the tray shows.
    pub fn tray_line(&self) -> String {
        match self.state.as_str() {
            "signed_out" => "Signed out".to_string(),
            "connecting" => "Connecting…".to_string(),
            "connected" => {
                if self.paused {
                    format!("Paused · {}", self.project)
                } else {
                    format!("Connected · {}", self.project)
                }
            }
            "offline" => "Offline — reconnecting".to_string(),
            "superseded" => "Another computer is connected".to_string(),
            "platform_off" => "Off on the platform — waiting".to_string(),
            "error" => format!("Error: {}", self.last_error.clone().unwrap_or_default()),
            other => other.to_string(),
        }
    }
}

enum Cmd {
    SignIn(ConnectionRecord),
    SignOut,
    TakeOver,
    Quit,
}

pub struct Agent {
    store: Option<Arc<TokenStore>>,
    presence: Arc<Presence>,
    status: Mutex<Status>,
    tx: mpsc::UnboundedSender<Cmd>,
    on_change: Box<dyn Fn(Status) + Send + Sync>,
    take_over: AtomicBool,
}

impl Agent {
    /// Create the agent and start its supervisor on the app's async runtime.
    pub fn start(on_change: impl Fn(Status) + Send + Sync + 'static) -> Arc<Agent> {
        let (tx, rx) = mpsc::unbounded_channel::<Cmd>();
        let pol = policy::load();
        let presence = Arc::new(Presence::new(pol.human_idle_ms.unwrap_or(3000)));
        presence.set_paused(store::is_paused_flag_set());
        let mut status = Status::blank();
        status.paused = presence.is_paused();
        let store = match TokenStore::new() {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                status.state = "error".into();
                status.last_error = Some(format!("credential store unavailable: {e}"));
                None
            }
        };
        let agent = Arc::new(Agent { store, presence, status: Mutex::new(status), tx, on_change: Box::new(on_change), take_over: AtomicBool::new(false) });
        // Its own thread and runtime: the channel client, the queue and the UIA
        // calls never share a thread with the app's event loop or Tauri's runtime.
        let me = agent.clone();
        std::thread::Builder::new()
            .name("kareenos-agent".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("kareenos-agent-rt").build().expect("agent runtime");
                rt.block_on(me.supervise(rx));
            })
            .expect("agent thread");
        (agent.on_change)(agent.status());
        agent
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        let s = {
            let mut g = self.status.lock().unwrap();
            f(&mut g);
            g.updated_at = now_ms();
            g.clone()
        };
        (self.on_change)(s);
    }

    pub fn sign_in(&self, rec: ConnectionRecord) {
        let _ = self.tx.send(Cmd::SignIn(rec));
    }
    pub fn sign_out(&self) {
        let _ = self.tx.send(Cmd::SignOut);
    }
    pub fn take_over(&self) {
        self.take_over.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::TakeOver);
    }
    pub fn quit(&self) {
        let _ = self.tx.send(Cmd::Quit);
    }

    /// The tray's "Pause agents". Only the flag file is written: the client's
    /// watcher flips the presence and tells the server (a `state` frame) within 2 s.
    pub fn set_paused(&self, on: bool) {
        if let Err(e) = store::set_paused_flag(on) {
            tracing::warn!(error = %e, "could not write the pause flag");
        }
        self.update(|s| s.paused = on);
    }

    /// No command in flight and none finished for `ms` (the updater's gate).
    pub fn idle_for(&self, ms: u64) -> bool {
        let s = self.status();
        s.busy.is_none() && s.last_command_at.map(|t| now_ms() - t >= ms as i64).unwrap_or(true)
    }

    fn on_conn_state(&self, cs: ConnState) {
        let state = match cs {
            ConnState::SignedOut => "signed_out",
            ConnState::Disconnected => "offline",
            ConnState::Connecting => "connecting",
            ConnState::Bound => "connected",
            ConnState::Superseded => "superseded",
            ConnState::PlatformOff => "platform_off",
        };
        self.update(|s| {
            s.state = state.to_string();
            if cs == ConnState::Bound {
                s.last_error = None;
            }
        });
    }

    fn on_activity(&self, a: Activity) {
        self.update(|s| {
            if a.started {
                s.busy = Some(a.label.clone());
            } else {
                s.busy = None;
                s.commands_done += 1;
                s.last_command_at = Some(now_ms());
                s.last_result_code = a.code.clone();
            }
        });
    }

    fn save(&self, store: &TokenStore, rec: ConnectionRecord) {
        match store.save(&rec) {
            Ok(()) => tracing::info!("signed in: {}", rec.label()),
            Err(e) => self.update(|s| {
                s.state = "error".into();
                s.last_error = Some(format!("could not save the sign-in: {e}"));
            }),
        }
    }

    fn clear(&self, store: &TokenStore) {
        if let Err(e) = store.clear() {
            tracing::warn!(error = %e, "could not clear the stored sign-in");
        }
        self.update(|s| {
            s.set_signed_out();
            s.last_error = None;
        });
    }

    async fn supervise(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let Some(store) = self.store.clone() else {
            // Without a credential store there is nothing to run; keep the tray alive.
            while let Some(cmd) = rx.recv().await {
                if matches!(cmd, Cmd::Quit) {
                    break;
                }
            }
            return;
        };
        let mut desktop: Option<Arc<terminator::Desktop>> = None;
        loop {
            // 1. A stored sign-in?
            let record = match store.load() {
                Ok(r) => r,
                Err(e) => {
                    self.update(|s| {
                        s.state = "error".into();
                        s.last_error = Some(e.to_string());
                    });
                    None
                }
            };
            let Some(record) = record else {
                self.update(|s| s.set_signed_out());
                match rx.recv().await {
                    Some(Cmd::SignIn(rec)) => {
                        self.save(&store, rec);
                        continue;
                    }
                    Some(Cmd::Quit) | None => return,
                    Some(_) => continue,
                }
            };
            self.update(|s| {
                s.apply_record(&record);
                s.state = "connecting".into();
            });

            // 2. Windows UI Automation, once.
            if desktop.is_none() {
                match terminator::Desktop::new(false, false) {
                    Ok(d) => desktop = Some(Arc::new(d)),
                    Err(e) => {
                        self.update(|s| {
                            s.state = "error".into();
                            s.last_error = Some(format!("Windows UI Automation is not available: {e}"));
                        });
                        tokio::select! {
                            c = rx.recv() => match c {
                                Some(Cmd::SignIn(rec)) => self.save(&store, rec),
                                Some(Cmd::SignOut) => self.clear(&store),
                                Some(Cmd::Quit) | None => return,
                                Some(Cmd::TakeOver) => {}
                            },
                            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                        }
                        continue;
                    }
                }
            }

            // 3. The client, until it ends or the UI asks for something else.
            let pol = policy::load();
            self.presence.set_idle_window(pol.human_idle_ms.unwrap_or(3000));
            let adapter = Arc::new(Adapter::new(desktop.clone().expect("desktop"), pol.clone(), self.presence.clone()));
            let ws_url = pol
                .ws_url
                .clone()
                .or_else(|| record.ws_url.clone().filter(|u| !u.is_empty()))
                .unwrap_or_else(|| DEFAULT_WS_URL.to_string());
            let me_state = self.clone();
            let me_act = self.clone();
            let cfg = ClientConfig {
                ws_url: ws_url.clone(),
                install_id: store::install_id(),
                take_over: self.take_over.swap(false, Ordering::SeqCst),
                on_state: Some(Arc::new(move |cs: ConnState| me_state.on_conn_state(cs))),
                on_activity: Some(Arc::new(move |a: Activity| me_act.on_activity(a))),
            };
            self.update(|s| {
                s.server = ws_url;
                s.last_error = None;
            });
            tracing::info!(version = AGENT_VERSION, "starting the channel client for {}", record.label());
            let shutdown = CancellationToken::new();
            let run = client::run(store.clone(), record, cfg, adapter, self.presence.clone(), shutdown.clone());
            tokio::pin!(run);
            let mut finished = false;
            let next: Option<Cmd> = loop {
                tokio::select! {
                    exit = &mut run => {
                        finished = true;
                        match exit {
                            Exit::Shutdown => break None,
                            Exit::SignedOut(reason) => {
                                tracing::warn!(reason = %reason, "signed out by the platform");
                                self.update(|s| {
                                    s.set_signed_out();
                                    s.last_error = Some(format!("Signed out ({reason}). Connect this computer again from Kareenos → Browsers & Desktops."));
                                });
                                break None;
                            }
                            Exit::Superseded => {
                                self.update(|s| s.state = "superseded".into());
                                // Parked until "Use this computer", a new sign-in, sign-out or quit.
                                match rx.recv().await {
                                    Some(c) => break Some(c),
                                    None => return,
                                }
                            }
                        }
                    }
                    cmd = rx.recv() => { break Some(cmd.unwrap_or(Cmd::Quit)); }
                }
            };
            if !finished {
                shutdown.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(3), &mut run).await;
            }
            match next {
                None => continue,
                Some(Cmd::SignIn(rec)) => self.save(&store, rec),
                Some(Cmd::SignOut) => self.clear(&store),
                Some(Cmd::TakeOver) => {}
                Some(Cmd::Quit) => return,
            }
        }
    }
}
