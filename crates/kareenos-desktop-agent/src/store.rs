//! Where this computer keeps its sign-in and its small state.
//!
//! The token record lives in the Windows Credential Manager (per user,
//! encrypted by Windows) — the `chrome.storage.local` of the extension. The
//! agent is its sole writer. Everything else (install id, logs, the pause flag)
//! lives under `%LOCALAPPDATA%\Kareenos\DesktopAgent`.
//!
//! Wave 2: an installer may leave a one-time connection code and the install
//! mode in `HKLM\Software\Kareenos\DesktopAgent` (`ConnectCode`, `Mode`): the
//! silent fleet install (`msiexec … CONNECT_CODE=… ADDLOCAL=Service`). The app
//! reads the code once, when no sign-in is stored, and remembers which code it
//! used so a dead code is never retried. An unattended computer keeps ONE
//! Windows account signed in (the customer's auto-logon policy), so that
//! account's Credential Manager is the right home for the token there too.

use crate::record::ConnectionRecord;
use anyhow::{Context, Result};
use std::path::PathBuf;

const SERVICE: &str = "Kareenos Desktop Agent";
const USER: &str = "channel";

pub struct TokenStore {
    entry: keyring::Entry,
}

impl TokenStore {
    pub fn new() -> Result<Self> {
        let entry = keyring::Entry::new(SERVICE, USER).context("credential store unavailable")?;
        Ok(TokenStore { entry })
    }

    pub fn load(&self) -> Result<Option<ConnectionRecord>> {
        match self.entry.get_password() {
            Ok(json) => {
                let rec: ConnectionRecord = serde_json::from_str(&json).context("stored sign-in is unreadable")?;
                Ok(Some(rec))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("credential store read failed: {e}")),
        }
    }

    pub fn save(&self, rec: &ConnectionRecord) -> Result<()> {
        let json = serde_json::to_string(rec)?;
        self.entry.set_password(&json).context("credential store write failed")?;
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("credential store delete failed: {e}")),
        }
    }
}

/// `%LOCALAPPDATA%\Kareenos\DesktopAgent` (created on first use).
pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Kareenos").join("DesktopAgent");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn logs_dir() -> PathBuf {
    let dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `%ProgramData%\Kareenos\DesktopAgent`: the machine-wide folder (policy, the service's log).
pub fn program_data_dir() -> PathBuf {
    let base = std::env::var_os("PROGRAMDATA").map(PathBuf::from).unwrap_or_else(data_dir);
    base.join("Kareenos").join("DesktopAgent")
}

/// A GUID minted once per install; the session row's channel_identifier.
pub fn install_id() -> String {
    let p = data_dir().join("install_id");
    if let Ok(s) = std::fs::read_to_string(&p) {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::write(&p, &id);
    id
}

/// The tray's "Pause agents" switch, as a file so the console agent and the
/// tray app (Phase 2) share it without any IPC.
pub fn paused_flag_path() -> PathBuf {
    data_dir().join("paused")
}

pub fn is_paused_flag_set() -> bool {
    paused_flag_path().exists()
}

pub fn set_paused_flag(on: bool) -> Result<()> {
    let p = paused_flag_path();
    if on {
        std::fs::write(&p, b"paused\n")?;
    } else if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- the installer's hand-off

const REG_KEY: &str = "Software\\Kareenos\\DesktopAgent";

/// The connection code the installer left for this machine (`HKLM\…\ConnectCode`),
/// unless it is the one this install already used. None when there is none.
pub fn pending_connect_code() -> Option<String> {
    let code = read_machine_value("ConnectCode").or_else(|| std::env::var("KAREENOS_CONNECT_CODE").ok())?;
    let code = code.trim().to_string();
    if code.is_empty() {
        return None;
    }
    let marker = data_dir().join("used_connect_code");
    let fp = fingerprint(&code);
    if std::fs::read_to_string(&marker).map(|s| s.trim() == fp).unwrap_or(false) {
        return None;
    }
    Some(code)
}

/// Remember that this code was consumed (it is one-time on the platform), and
/// try to delete it from the registry (that needs an administrator; failing is fine).
pub fn mark_connect_code_used(code: &str) {
    let _ = std::fs::write(data_dir().join("used_connect_code"), fingerprint(code));
    delete_machine_value("ConnectCode");
}

/// `service` when the installer's "Run unattended (service)" feature is in, else None.
pub fn install_mode() -> Option<String> {
    read_machine_value("Mode").map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty())
}

fn fingerprint(s: &str) -> String {
    // Not a secret hash, a marker: FNV-1a over the code, so the code itself is never written to disk.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(windows)]
fn read_machine_value(name: &str) -> Option<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(REG_KEY, KEY_READ).ok()?;
    key.get_value::<String, _>(name).ok()
}

#[cfg(windows)]
fn delete_machine_value(name: &str) {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_SET_VALUE};
    use winreg::RegKey;
    if let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(REG_KEY, KEY_SET_VALUE) {
        let _ = key.delete_value(name);
    }
}

#[cfg(not(windows))]
fn read_machine_value(_name: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
fn delete_machine_value(_name: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_stable_and_not_the_code() {
        let a = fingerprint("abc");
        assert_eq!(a, fingerprint("abc"));
        assert_ne!(a, fingerprint("abd"));
        assert_eq!(a.len(), 16);
        assert!(!a.contains("abc"));
    }
}
