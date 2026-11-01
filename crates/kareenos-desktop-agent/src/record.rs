//! The connection record the Kareenos sign-in page hands over, either as the
//! `kareenos-desktop://connect?p=<base64url JSON>` link or as the same code
//! pasted by hand. It becomes the stored sign-in (token record) of this computer.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionRecord {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub channel: String,
    pub token: String,
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
    /// false once the absolute max age has passed (the server stops re-issuing).
    #[serde(default)]
    pub renewable: Option<bool>,
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

    /// A link or a bare code — whatever the person gave us.
    pub fn from_any(input: &str) -> Result<Self> {
        let t = input.trim();
        if t.starts_with(&format!("{}://", crate::URL_SCHEME)) {
            Self::from_deep_link(t)
        } else {
            Self::from_code(t)
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.token.trim().is_empty() {
            return Err(anyhow!("the connection record has no token"));
        }
        if !self.channel.is_empty() && self.channel != "desktop" {
            return Err(anyhow!(
                "this connection record is for the '{}' channel, not the Desktop Agent (did you copy a browser-extension sign-in?)",
                self.channel
            ));
        }
        Ok(())
    }

    /// The code form (what the sign-in page shows), for round trips and tests.
    pub fn to_code(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    /// A human label for logs and the tray: never the token.
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.project_name.clone().unwrap_or_else(|| self.projectid.clone().unwrap_or_default()),
            self.account_name.clone().unwrap_or_else(|| self.accountid.clone().unwrap_or_default()),
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
            renewable: None,
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
}
