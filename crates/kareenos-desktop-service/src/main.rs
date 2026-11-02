//! Kareenos Desktop Service — the small Windows service of wave 2 (plan
//! kareenos-desktop-fleet-plan-2026-11-02 §4.6).
//!
//! Windows UI Automation drives the INTERACTIVE desktop only: a service in
//! session 0 sees no windows. So this service never drives anything itself. It
//! runs as LocalSystem, watches which session owns the console, and keeps ONE
//! `kareenos-desktop-agent-app.exe --service-child` alive inside that session
//! (WTSQueryUserToken → CreateEnvironmentBlock → CreateProcessAsUserW on
//! winsta0\default), restarting it when it exits, replacing it when the console
//! session changes, and stopping it when the service stops. No network, no port,
//! no command of its own. Logs go to %ProgramData%\Kareenos\DesktopAgent\logs\service.log.
//!
//!   kareenos-desktop-service.exe              run as the service (the SCM starts it)
//!   kareenos-desktop-service.exe --install    register + start (an administrator; the MSI does this)
//!   kareenos-desktop-service.exe --uninstall  stop + remove
//!   kareenos-desktop-service.exe --run        supervise in the foreground (tests), Ctrl+C stops

use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SERVICE_NAME: &str = "KareenosDesktopService";
pub const SERVICE_DISPLAY: &str = "Kareenos Desktop Service";
const CHILD_EXE: &str = "kareenos-desktop-agent-app.exe";
const POLL: Duration = Duration::from_secs(3);
const RESTART_DELAY: Duration = Duration::from_secs(5);
const NO_SESSION: u32 = 0xFFFF_FFFF;

fn log_dir() -> PathBuf {
    let base = std::env::var_os("PROGRAMDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Kareenos").join("DesktopAgent").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn log(line: &str) {
    use std::io::Write;
    let stamp = {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        format!("{secs}")
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_dir().join("service.log")) {
        let _ = writeln!(f, "{stamp} {line}");
    }
    eprintln!("{line}");
}

fn child_exe() -> PathBuf {
    let here = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    here.join(CHILD_EXE)
}

// ---------------------------------------------------------------- the supervisor

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
    use windows::Win32::System::Threading::{
        CreateProcessAsUserW, TerminateProcess, WaitForSingleObject, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    pub struct Child {
        pub session: u32,
        pub pid: u32,
        process: HANDLE,
    }

    impl Child {
        pub fn running(&self) -> bool {
            // SAFETY: a wait with a zero timeout on a process handle we own.
            unsafe { WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 }
        }
        pub fn stop(&self) {
            // SAFETY: the handle is ours; TerminateProcess on an exited process just fails.
            unsafe {
                let _ = TerminateProcess(self.process, 0);
                let _ = WaitForSingleObject(self.process, 5_000);
                let _ = CloseHandle(self.process);
            }
        }
    }

    pub fn console_session() -> Option<u32> {
        // SAFETY: a plain query.
        let s = unsafe { WTSGetActiveConsoleSessionId() };
        if s == NO_SESSION {
            None
        } else {
            Some(s)
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Start the agent app inside `session`, as that session's user, on its interactive desktop.
    pub fn launch(session: u32, exe: &Path) -> anyhow::Result<Child> {
        // SAFETY: Win32 process creation with handles we close on every path.
        unsafe {
            let mut token = HANDLE::default();
            WTSQueryUserToken(session, &mut token).map_err(|e| anyhow::anyhow!("WTSQueryUserToken({session}): {e}"))?;
            let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
            if let Err(e) = CreateEnvironmentBlock(&mut env, Some(token), false) {
                let _ = CloseHandle(token);
                return Err(anyhow::anyhow!("CreateEnvironmentBlock: {e}"));
            }
            let mut desktop = wide("winsta0\\default");
            let mut si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
            si.lpDesktop = PWSTR(desktop.as_mut_ptr());
            let mut cmd = wide(&format!("\"{}\" --service-child", exe.display()));
            let dir = wide(&exe.parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".to_string()));
            let mut pi = PROCESS_INFORMATION::default();
            let created = CreateProcessAsUserW(
                Some(token),
                PCWSTR::null(),
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_PROCESS_GROUP,
                Some(env as *const core::ffi::c_void),
                PCWSTR(dir.as_ptr()),
                &si,
                &mut pi,
            );
            let _ = DestroyEnvironmentBlock(env);
            let _ = CloseHandle(token);
            created.map_err(|e| anyhow::anyhow!("CreateProcessAsUserW: {e}"))?;
            let _ = CloseHandle(pi.hThread);
            Ok(Child { session, pid: pi.dwProcessId, process: pi.hProcess })
        }
    }

    /// One supervision tick: keep exactly one child in the console session.
    pub fn tick(child: &mut Option<Child>, last_fail: &mut Option<std::time::Instant>) {
        let exe = child_exe();
        let session = console_session();
        if let Some(c) = child.as_ref() {
            let keep = session == Some(c.session) && c.running();
            if !keep {
                log(&format!("child pid {} (session {}) is gone or the console moved ({:?}); stopping it", c.pid, c.session, session));
                c.stop();
                *child = None;
                *last_fail = Some(std::time::Instant::now());
            }
        }
        if child.is_none() {
            let Some(s) = session else { return };
            if let Some(t) = last_fail {
                if t.elapsed() < RESTART_DELAY {
                    return;
                }
            }
            if !exe.exists() {
                log(&format!("agent app not found at {}", exe.display()));
                *last_fail = Some(std::time::Instant::now());
                return;
            }
            match launch(s, &exe) {
                Ok(c) => {
                    log(&format!("started {} in session {} as pid {}", CHILD_EXE, s, c.pid));
                    *child = Some(c);
                }
                Err(e) => {
                    log(&format!("could not start the agent in session {s}: {e}"));
                    *last_fail = Some(std::time::Instant::now());
                }
            }
        }
    }

    pub fn stop_child(child: &mut Option<Child>) {
        if let Some(c) = child.take() {
            log(&format!("stopping child pid {}", c.pid));
            c.stop();
        }
    }
}

// ---------------------------------------------------------------- the service shell

#[cfg(windows)]
mod svc {
    use super::*;
    use std::ffi::OsString;
    use std::sync::mpsc;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo, ServiceStartType, ServiceState,
        ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            log(&format!("service error: {e}"));
        }
    }

    fn status(state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN | ServiceControlAccept::SESSION_CHANGE,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }
    }

    fn run_service() -> anyhow::Result<()> {
        let (tx, rx) = mpsc::channel::<&'static str>();
        let handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = tx.send("stop");
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::SessionChange(_) => {
                    let _ = tx.send("session");
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let handle = service_control_handler::register(SERVICE_NAME, handler)?;
        handle.set_service_status(status(ServiceState::Running))?;
        log(&format!("{SERVICE_DISPLAY} running (agent app: {})", child_exe().display()));
        let mut child: Option<win::Child> = None;
        let mut last_fail: Option<std::time::Instant> = None;
        loop {
            win::tick(&mut child, &mut last_fail);
            match rx.recv_timeout(POLL) {
                Ok("stop") => break,
                Ok(_) => continue,          // a session change: the next tick re-evaluates
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        win::stop_child(&mut child);
        handle.set_service_status(status(ServiceState::Stopped))?;
        log("service stopped");
        Ok(())
    }

    pub fn dispatch() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    pub fn install() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::env::current_exe()?,
            launch_arguments: vec![],
            dependencies: vec![],
            account_name: None,   // LocalSystem
            account_password: None,
        };
        let service = manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
        service.set_description("Keeps the Kareenos Desktop Agent running in the signed-in Windows session so Kareenos agents can work on this computer unattended.")?;
        service.start::<&str>(&[])?;
        log("installed and started");
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS)?;
        let _ = service.stop();
        for _ in 0..20 {
            if let Ok(s) = service.query_status() {
                if s.current_state == ServiceState::Stopped {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        service.delete()?;
        log("uninstalled");
        Ok(())
    }

    /// Foreground supervision for tests: the same loop, Ctrl+C stops it.
    pub fn run_foreground() -> anyhow::Result<()> {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let stop = stop.clone();
            let _ = ctrlc_like(move || stop.store(true, std::sync::atomic::Ordering::SeqCst));
        }
        log("supervising in the foreground (Ctrl+C stops)");
        let mut child: Option<win::Child> = None;
        let mut last_fail: Option<std::time::Instant> = None;
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            win::tick(&mut child, &mut last_fail);
            std::thread::sleep(POLL);
        }
        win::stop_child(&mut child);
        Ok(())
    }

    fn ctrlc_like(f: impl Fn() + Send + 'static) -> anyhow::Result<()> {
        // A tiny stand-in for the ctrlc crate: read stdin for a line, or rely on the process being killed.
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            f();
        });
        Ok(())
    }
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    #[cfg(windows)]
    {
        let result = match arg.as_str() {
            "--install" => svc::install(),
            "--uninstall" => svc::uninstall(),
            "--run" => svc::run_foreground(),
            "--version" => {
                println!("kareenos-desktop-service {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            _ => svc::dispatch(),
        };
        if let Err(e) = result {
            eprintln!("{SERVICE_DISPLAY}: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("{SERVICE_DISPLAY} runs on Windows only ({arg})");
        std::process::exit(2);
    }
}
