//! Kareenos Desktop Agent — console binary (the tray app of Phase 2 wraps the
//! same library). Sign in once with the link or code from Kareenos → Browsers →
//! "Connect this computer"; afterwards it runs with no arguments.

use clap::Parser;
use kareenos_desktop_agent::adapter::Adapter;
use kareenos_desktop_agent::client::{self, ClientConfig, Exit};
use kareenos_desktop_agent::presence::Presence;
use kareenos_desktop_agent::record::ConnectionRecord;
use kareenos_desktop_agent::store::{self, TokenStore};
use kareenos_desktop_agent::{envelope, policy, AGENT_VERSION, DEFAULT_WS_URL};
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "kareenos-desktop-agent", version, about = "Kareenos Desktop Agent — lets your Kareenos agents work in the programs on this Windows computer while you are present.")]
struct Cli {
    /// A kareenos-desktop://connect?p=... link or the connection code from the sign-in page. Saves it as this computer's sign-in and connects.
    connect: Option<String>,
    /// Override the wss URL (default: the sign-in record's ws_url, else the build default).
    #[arg(long)]
    server: Option<String>,
    /// Start with agents paused (the tray's "Pause agents").
    #[arg(long)]
    paused: bool,
    /// Reconnect after another computer took this project's slot ("Use this computer").
    #[arg(long)]
    take_over: bool,
    /// Offline self-test of the agent's own pieces; no network.
    #[arg(long)]
    selfcheck: bool,
    /// Forget this computer's sign-in (the platform-side sign-out is Kareenos → Browsers).
    #[arg(long)]
    sign_out: bool,
    /// Print the stored sign-in (never the token) and exit.
    #[arg(long)]
    status: bool,
    /// Exit after this many minutes (spikes, CI).
    #[arg(long)]
    minutes: Option<u64>,
    /// Log filter (RUST_LOG syntax).
    #[arg(long, default_value = "info")]
    log: String,
}

fn init_logging(filter: &str) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = tracing_appender::rolling::daily(store::logs_dir(), "agent.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file);
    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_target(false))
        .with(tracing_subscriber::fmt::layer().with_writer(file_writer).with_ansi(false).with_target(false));
    if subscriber.try_init().is_err() {
        return None;
    }
    Some(guard)
}

fn selfcheck() -> bool {
    let mut ok = true;
    let mut say = |label: &str, good: bool, detail: String| {
        println!("{} {label}{}", if good { "ok  " } else { "FAIL" }, if detail.is_empty() { String::new() } else { format!(" — {detail}") });
        if !good {
            ok = false;
        }
    };
    say("version", true, AGENT_VERSION.to_string());
    say("envelope", envelope::ACTIONS.len() == 13 && envelope::budget_for("click", None) == 14000, format!("{} actions", envelope::ACTIONS.len()));
    let rec = ConnectionRecord { v: 1, channel: "desktop".into(), token: "t".into(), exp: Some(1), session_id: None, projectid: Some("pr".into()), accountid: Some("a".into()), userid: Some("u".into()), project_name: None, account_name: None, user_name: None, ws_url: None, api_url: None, renewable: None };
    let round = ConnectionRecord::from_any(&format!("kareenos-desktop://connect?p={}", rec.to_code())).map(|r| r == rec).unwrap_or(false);
    say("connection record round trip", round, String::new());
    let dir = store::data_dir();
    say("data dir", dir.exists(), dir.display().to_string());
    say("install id", !store::install_id().is_empty(), String::new());
    let pol = policy::load();
    say("local policy", !pol.app_allowed("cmd") && pol.app_allowed("notepad"), format!("{} (deny {} entries)", policy::path().display(), pol.deny_processes.len()));
    match kareenos_desktop_agent::presence::last_input_ticks() {
        Some((now, last)) => say("last input", true, format!("{} ms ago", now.wrapping_sub(last))),
        None => say("last input", !cfg!(windows), "unavailable on this platform".to_string()),
    }
    match keyring::Entry::new("Kareenos Desktop Agent", "selfcheck") {
        Ok(e) => {
            let probe = format!("probe-{}", envelope::now_ms());
            let wrote = e.set_password(&probe).is_ok();
            let read = e.get_password().map(|p| p == probe).unwrap_or(false);
            let _ = e.delete_credential();
            say("credential store", wrote && read, String::new());
        }
        Err(err) => say("credential store", false, err.to_string()),
    }
    #[cfg(windows)]
    {
        match terminator::Desktop::new(false, false) {
            Ok(d) => {
                let focused = d.focused_element().ok().and_then(|f| f.window().ok().flatten()).and_then(|w| w.name()).unwrap_or_default();
                let apps = d.applications().map(|a| a.len()).unwrap_or(0);
                say("ui automation", true, format!("{apps} applications; focused window \"{focused}\""));
            }
            Err(e) => say("ui automation", false, e.to_string()),
        }
    }
    println!("{}", if ok { "selfcheck OK" } else { "selfcheck FAILED" });
    ok
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let _guard = init_logging(&cli.log);

    if cli.selfcheck {
        return if selfcheck() { ExitCode::SUCCESS } else { ExitCode::from(5) };
    }

    let store = match TokenStore::new() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("credential store unavailable: {e}");
            return ExitCode::from(4);
        }
    };

    if cli.sign_out {
        return match store.clear() {
            Ok(()) => {
                println!("This computer's sign-in was removed. To sign it out on the platform too, open Kareenos → Browsers.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(4)
            }
        };
    }

    let record = if let Some(input) = cli.connect.as_deref() {
        match ConnectionRecord::from_any(input) {
            Ok(r) => {
                if let Err(e) = store.save(&r) {
                    eprintln!("could not save the sign-in: {e}");
                    return ExitCode::from(4);
                }
                println!("Signed in: {}", r.label());
                r
            }
            Err(e) => {
                eprintln!("That is not a Kareenos Desktop Agent connection code or link: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        match store.load() {
            Ok(Some(r)) => r,
            Ok(None) => {
                eprintln!("Not signed in. Open Kareenos → Browsers → \"Connect this computer\", then run this program with the link or the connection code it shows.");
                return ExitCode::from(2);
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(4);
            }
        }
    };

    if cli.status {
        println!("Signed in: {}", record.label());
        println!("project {} · account {} · user {}", record.projectid.clone().unwrap_or_default(), record.accountid.clone().unwrap_or_default(), record.userid.clone().unwrap_or_default());
        println!("token expires: {}", record.exp.map(|e| chrono::DateTime::from_timestamp(e, 0).map(|d| d.to_rfc3339()).unwrap_or_default()).unwrap_or_else(|| "unknown".into()));
        println!("server: {}", record.ws_url.clone().unwrap_or_else(|| DEFAULT_WS_URL.to_string()));
        println!("paused: {}", store::is_paused_flag_set());
        return ExitCode::SUCCESS;
    }

    let pol = policy::load();
    let ws_url = cli
        .server
        .clone()
        .or_else(|| pol.ws_url.clone())
        .or_else(|| record.ws_url.clone().filter(|u| !u.is_empty()))
        .unwrap_or_else(|| DEFAULT_WS_URL.to_string());

    let desktop = match terminator::Desktop::new(false, false) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("Windows UI Automation is not available: {e}");
            return ExitCode::from(4);
        }
    };
    let presence = Arc::new(Presence::new(pol.human_idle_ms.unwrap_or(3000)));
    if cli.paused {
        let _ = store::set_paused_flag(true);
    }
    presence.set_paused(store::is_paused_flag_set());
    let adapter = Arc::new(Adapter::new(desktop, pol, presence.clone()));

    let shutdown = CancellationToken::new();
    {
        let s = shutdown.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("ctrl-c — shutting down");
            s.cancel();
        });
    }
    if let Some(m) = cli.minutes {
        let s = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(m * 60)).await;
            tracing::info!(minutes = m, "time limit reached — shutting down");
            s.cancel();
        });
    }

    tracing::info!(version = AGENT_VERSION, url = %ws_url, "Kareenos Desktop Agent starting for {}", record.label());
    let cfg = ClientConfig { ws_url, install_id: store::install_id(), take_over: cli.take_over, on_state: None, on_activity: None };
    match client::run(store, record, cfg, adapter, presence, shutdown).await {
        Exit::Shutdown => ExitCode::SUCCESS,
        Exit::Superseded => {
            eprintln!("Another computer connected to this project; this one stepped aside. Run again with --take-over to use this computer.");
            ExitCode::from(6)
        }
        Exit::SignedOut(reason) => {
            eprintln!("Signed out ({reason}). Open Kareenos → Browsers → \"Connect this computer\" to sign in again.");
            ExitCode::from(3)
        }
    }
}
