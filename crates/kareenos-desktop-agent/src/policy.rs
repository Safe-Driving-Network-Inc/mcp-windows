//! The computer's local policy — `%ProgramData%\Kareenos\DesktopAgent\policy.json`.
//! Absent = the defaults below. Local wins over whatever the platform asks
//! (the CLI client's rule): an application on the deny list is refused with
//! APP_NOT_ALLOWED, and the deny list ships with the shells and system consoles
//! because shell work belongs to the Kareenos CLI Client, never to this agent.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPolicy {
    /// Process / application names (case-insensitive, `.exe` optional) agents may never open.
    #[serde(default = "default_deny")]
    pub deny_processes: Vec<String>,
    /// When non-empty, the ONLY applications agents may open.
    #[serde(default)]
    pub allow_processes: Vec<String>,
    /// false = desktop_open is refused for every application (URLs still open).
    #[serde(default = "yes")]
    pub allow_open: bool,
    /// Overrides the platform's human-idle window (ms); 0 = off.
    #[serde(default)]
    pub human_idle_ms: Option<u64>,
    /// Overrides the wss URL (white-label / self-hosted platforms).
    #[serde(default)]
    pub ws_url: Option<String>,
}

fn yes() -> bool {
    true
}

pub fn default_deny() -> Vec<String> {
    ["cmd", "powershell", "pwsh", "powershell_ise", "windowsterminal", "wt", "regedit", "mmc", "taskmgr"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for LocalPolicy {
    fn default() -> Self {
        LocalPolicy { deny_processes: default_deny(), allow_processes: vec![], allow_open: true, human_idle_ms: None, ws_url: None }
    }
}

fn norm(name: &str) -> String {
    let n = name.trim().to_lowercase();
    let n = n.rsplit(['\\', '/']).next().unwrap_or(&n).to_string();
    n.strip_suffix(".exe").map(|s| s.to_string()).unwrap_or(n)
}

impl LocalPolicy {
    pub fn app_allowed(&self, name: &str) -> bool {
        let n = norm(name);
        if n.is_empty() {
            return false;
        }
        if self.deny_processes.iter().any(|d| norm(d) == n || n.contains(&norm(d))) {
            return false;
        }
        if !self.allow_processes.is_empty() && !self.allow_processes.iter().any(|a| norm(a) == n) {
            return false;
        }
        true
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("PROGRAMDATA").map(PathBuf::from).unwrap_or_else(crate::store::data_dir);
    base.join("Kareenos").join("DesktopAgent").join("policy.json")
}

/// Defaults when the file is absent; a broken file is reported and ignored.
pub fn load() -> LocalPolicy {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(s) => match serde_json::from_str::<LocalPolicy>(&s) {
            Ok(pol) => {
                tracing::info!(path = %p.display(), "local policy loaded");
                pol
            }
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "local policy unreadable — using the defaults");
                LocalPolicy::default()
            }
        },
        Err(_) => LocalPolicy::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_list_and_allow_list() {
        let p = LocalPolicy::default();
        assert!(p.app_allowed("notepad"));
        assert!(p.app_allowed("EXCEL.EXE"));
        assert!(!p.app_allowed("cmd"));
        assert!(!p.app_allowed("C:\\Windows\\System32\\cmd.exe"));
        assert!(!p.app_allowed("PowerShell"));
        assert!(!p.app_allowed(""));
        let only = LocalPolicy { allow_processes: vec!["notepad".into()], ..LocalPolicy::default() };
        assert!(only.app_allowed("Notepad.exe"));
        assert!(!only.app_allowed("excel"));
    }
}
