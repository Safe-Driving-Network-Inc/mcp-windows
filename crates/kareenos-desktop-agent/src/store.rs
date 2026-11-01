//! Where this computer keeps its sign-in and its small state.
//!
//! The token record lives in the Windows Credential Manager (per user,
//! encrypted by Windows) — the `chrome.storage.local` of the extension. The
//! agent is its sole writer. Everything else (install id, logs, the pause flag)
//! lives under `%LOCALAPPDATA%\Kareenos\DesktopAgent`.

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
