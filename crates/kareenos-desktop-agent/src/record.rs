//! The connection record the Kareenos sign-in page hands over, either as the
//! `kareenos-desktop://connect?p=<base64url JSON>` link or as the same code
//! pasted by hand. It becomes the stored sign-in (token record) of this computer.
//!
//! Two shapes (contract 1.1):
//! * `v: 1` — a TOKEN record: the Browsers card's "Connect this computer" minted a
//!   session and its token; the agent binds with the token.
//! * `v: 2` — a CODE record: a registered computer's one-time connection code
//!   (the Computers table, or the MSI's `CONNECT_CODE` of a silent install); the
//!   agent binds with the code once and receives its token in a `token` frame,
//!   which replaces the code in the stored record.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};

/// The computer this sign-in belongs to (from a v2 record or the tier's bind_ack).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ComputerRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

impl ComputerRef {
    /// "Lab PC (LAB-01)", "LAB-01", "Lab PC", or empty.
    pub fn label(&self) -> String {
        match (self.name.as_deref().filter(|s| !s.is_empty()), self.code.as_deref().filter(|s| !s.is_empty())) {
            (Some(n), Some(c)) => format!("{n} ({c})"),
            (Some(n), None) => n.to_string(),
            (None, Some(c)) => c.to_string(),
            (None, None) => String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionRecord {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub channel: String,
    /// Empty on a v2 code record until the first bind answers with a token.
    #[serde(default)]
    pub token: String,
    /// v2: the one-time connection code of a registered computer.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub computer: Option<ComputerRef>,
    #[serde(default)]
    pub exp: Option<i64>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub projectid: Option<String>,
    #[serde(default)]
    pub accountid: Option<String>,
    #[serde(default)]
    pub userid: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub account_name: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub ws_url: Option<String>,
    #[serde(default)]
    pub api_url: Option<String>,
    /// The console (SPA) the person signs in on — where "Open Kareenos" goes. The
    /// API host is NOT it (the API answers ResourceNotFound for console routes).
    #[serde(default)]
    pub app_url: Option<String>,
    /// false once the absolute max age has passed (the server stops re-issuing).
    #[serde(default)]
    pub renewable: Option<bool>,
    /// Agent 0.2.2: when the platform signed this record (epoch seconds) and the
    /// signature `<kid>.<base64url Ed25519>` over [`ConnectionRecord::canonical`];
    /// checked by `trust.rs` before a new record is accepted.
    #[serde(default)]
    pub iat: Option<i64>,
    #[serde(default)]
    pub sig: Option<String>,
}

/// What the confirmation screen shows before a record is accepted: names and
/// hosts only, never the token or the code.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RecordPreview {
    /// "code" (a registered computer's one-time code) or "token" (a Browsers card sign-in).
    pub kind: String,
    pub host: String,
    pub api_host: String,
    pub app_host: String,
    pub account: String,
    pub user: String,
    pub project: String,
    pub computer_name: String,
    pub computer_code: String,
    pub mode: String,
    /// The key id that signed it ("" when the signature was not required).
    pub signed_by: String,
}

fn one() -> u32 {
    1
}

impl ConnectionRecord {
    /// Parse the base64url JSON code shown on the sign-in page.
    pub fn from_code(code: &str) -> Result<Self> {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("empty connection code"));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(trimmed)
            .or_else(|_| URL_SAFE.decode(trimmed))
            .context("the connection code is not valid base64url")?;
        let rec: ConnectionRecord =
            serde_json::from_slice(&bytes).context("the connection code does not hold a connection record")?;
        rec.validate()?;
        Ok(rec)
    }

    /// Parse `kareenos-desktop://connect?p=<code>`.
    pub fn from_deep_link(link: &str) -> Result<Self> {
        let u = url::Url::parse(link.trim()).context("not a URL")?;
        if u.scheme() != crate::URL_SCHEME {
            return Err(anyhow!("not a {}:// link", crate::URL_SCHEME));
        }
        let code = u
            .query_pairs()
            .find(|(k, _)| k == "p")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| anyhow!("the link carries no connection code (p=)"))?;
        Self::from_code(&code)
    }

    /// A link or a bare code — whatever the person gave us. Shape only: a NEW
    /// record is accepted through [`ConnectionRecord::from_any_trusted`].
    pub fn from_any(input: &str) -> Result<Self> {
        let t = input.trim();
        if t.starts_with(&format!("{}://", crate::URL_SCHEME)) {
            Self::from_deep_link(t)
        } else {
            Self::from_code(t)
        }
    }

    /// Parse AND check a new record against what this agent trusts (0.2.2): the
    /// Kareenos signature and the hosts. Returns the record and the signing key id.
    pub fn from_any_trusted(input: &str, trust: &crate::trust::Trust, now_secs: i64) -> Result<(Self, String)> {
        let rec = Self::from_any(input)?;
        let kid = trust.check_record(&rec, now_secs)?;
        Ok((rec, kid))
    }

    /// The signed text: fixed fields in a fixed order, one per line, missing = empty.
    /// Byte for byte what the platform's desktop_record_signer.js signs.
    pub fn canonical(&self) -> String {
        let c = self.computer.clone().unwrap_or_default();
        let s = |o: &Option<String>| o.clone().unwrap_or_default();
        [
            crate::trust::CANONICAL_PREFIX.to_string(),
            format!("v={}", self.v),
            format!("channel={}", self.channel),
            format!("token={}", self.token),
            format!("code={}", s(&self.code)),
            format!("computer_id={}", s(&c.id)),
            format!("computer_code={}", s(&c.code)),
            format!("ws_url={}", s(&self.ws_url)),
            format!("api_url={}", s(&self.api_url)),
            format!("app_url={}", s(&self.app_url)),
            format!("iat={}", self.iat.unwrap_or(0)),
        ]
        .join("\n")
    }

    /// Names and hosts for the confirmation screen.
    pub fn preview(&self, signed_by: &str) -> RecordPreview {
        let host_of = |u: &Option<String>, default: &str| {
            let u = u.clone().filter(|x| !x.trim().is_empty()).unwrap_or_else(|| default.to_string());
            crate::trust::split_url(&u).map(|(_, h)| h).unwrap_or(u)
        };
        let c = self.computer.clone().unwrap_or_default();
        RecordPreview {
            kind: if self.is_code_only() { "code".into() } else { "token".into() },
            host: host_of(&self.ws_url, crate::DEFAULT_WS_URL),
            api_host: host_of(&self.api_url, ""),
            app_host: host_of(&self.app_url, "https://kareenos.com"),
            account: self.account_name.clone().unwrap_or_default(),
            user: self.user_name.clone().unwrap_or_default(),
            project: self.project_name.clone().unwrap_or_default(),
            computer_name: c.name.clone().unwrap_or_default(),
            computer_code: c.code.clone().unwrap_or_default(),
            mode: c.mode.clone().unwrap_or_default(),
            signed_by: signed_by.to_string(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.token.trim().is_empty() && !self.is_code_only() {
            return Err(anyhow!("the connection record has neither a token nor a connection code"));
        }
        if !self.channel.is_empty() && self.channel != "desktop" {
            return Err(anyhow!(
                "this connection record is for the '{}' channel, not the Desktop Agent (did you copy a browser-extension sign-in?)",
                self.channel
            ));
        }
        Ok(())
    }

    /// A v2 record that still has to exchange its code for a token.
    pub fn is_code_only(&self) -> bool {
        self.token.trim().is_empty() && self.code.as_deref().map(str::trim).map(|c| !c.is_empty()).unwrap_or(false)
    }

    /// The code form (what the sign-in page shows), for round trips and tests.
    pub fn to_code(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    /// A human label for logs and the tray: never the token, never the code.
    pub fn label(&self) -> String {
        let account = self.account_name.clone().unwrap_or_else(|| self.accountid.clone().unwrap_or_default());
        if let Some(c) = self.computer.as_ref().map(|c| c.label()).filter(|l| !l.is_empty()) {
            return if account.is_empty() { c } else { format!("{c} · {account}") };
        }
        format!(
            "{} · {} · {}",
            self.project_name.clone().unwrap_or_else(|| self.projectid.clone().unwrap_or_default()),
            account,
            self.user_name.clone().unwrap_or_else(|| self.userid.clone().unwrap_or_default())
        )
    }

    pub fn seconds_to_expiry(&self, now_secs: i64) -> Option<i64> {
        self.exp.map(|e| e - now_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ConnectionRecord {
        ConnectionRecord {
            v: 1,
            channel: "desktop".into(),
            token: "tok".into(),
            code: None,
            computer: None,
            exp: Some(1_800_000_000),
            session_id: Some("s1".into()),
            projectid: Some("pr".into()),
            accountid: Some("a".into()),
            userid: Some("u".into()),
            project_name: Some("Sales".into()),
            account_name: Some("Acme".into()),
            user_name: Some("Noura".into()),
            ws_url: Some("wss://example.test:8092/desktop-channel".into()),
            api_url: Some("https://example.test/api".into()),
            app_url: Some("https://console.example.test".into()),
            renewable: None,
            iat: None,
            sig: None,
        }
    }

    #[test]
    fn code_and_link_round_trip() {
        let r = sample();
        let code = r.to_code();
        assert_eq!(ConnectionRecord::from_code(&code).unwrap(), r);
        let link = format!("kareenos-desktop://connect?p={code}");
        assert_eq!(ConnectionRecord::from_any(&link).unwrap(), r);
        assert_eq!(ConnectionRecord::from_any(&format!("  {code} \n")).unwrap(), r);
        assert!(ConnectionRecord::from_any("https://example.test/x?p=abc").is_err());
    }

    #[test]
    fn refuses_a_browser_record() {
        let mut r = sample();
        r.channel = "browser".into();
        assert!(ConnectionRecord::from_code(&r.to_code()).is_err());
    }

    #[test]
    fn label_never_holds_the_token() {
        let r = sample();
        assert!(!r.label().contains("tok"));
        assert!(r.label().contains("Sales"));
    }

    #[test]
    fn a_v2_code_record_parses_and_labels_by_computer() {
        let json = r#"{"v":2,"channel":"desktop","code":"s3cr3t","computer":{"id":"C1","name":"Lab PC","code":"LAB-01","mode":"attended"},"account_name":"Acme","ws_url":"wss://x/desktop-channel","api_url":"https://x/api"}"#;
        let b64 = URL_SAFE_NO_PAD.encode(json.as_bytes());
        let r = ConnectionRecord::from_any(&b64).unwrap();
        assert!(r.is_code_only());
        assert_eq!(r.v, 2);
        assert_eq!(r.label(), "Lab PC (LAB-01) · Acme");
        assert!(!r.label().contains("s3cr3t"));
        // An empty record (no token, no code) is refused.
        let empty = URL_SAFE_NO_PAD.encode(br#"{"v":2,"channel":"desktop"}"#);
        assert!(ConnectionRecord::from_any(&empty).is_err());
        // Once the token arrives the record is a normal one.
        let mut got = r.clone();
        got.token = "tok".into();
        got.code = None;
        assert!(!got.is_code_only());
        assert!(got.validate().is_ok());
    }
}
