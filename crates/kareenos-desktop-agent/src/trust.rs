//! What the agent trusts (0.2.2, 2026-11-02): the servers a connection record may
//! name and the keys it must be signed with.
//!
//! A connection record (the hand-off page's link or code, a computer's connection
//! code from the Computers table, the MSI's `CONNECT_CODE`) tells the agent where
//! to dial. Anyone who got a person to open a crafted link or paste a crafted code
//! could otherwise point the agent at a server of their own — with a perfectly
//! valid certificate for that other name — and hold the keyboard. So a NEW record
//! is accepted only when
//!   * every URL in it (`ws_url`, `api_url`, `app_url`) names a Kareenos host — the
//!     apex or a subdomain of [`ALLOWED_HOST_SUFFIXES`], or a host the local
//!     policy's `extra_hosts` adds (that file belongs to the machine's
//!     administrator) — over `wss` / `https`, and
//!   * it carries a signature by one of [`RECORD_KEYS`] (Ed25519; the platform's
//!     `desktop_record_signer.js`) over [`ConnectionRecord::canonical`], dated
//!     within the last 30 days.
//! The stored sign-in (the agent's own Credential Manager entry) is trusted as
//! stored; only its hosts are re-checked before dialing (`check_hosts`).
//!
//! Rotation: add the new public key to [`RECORD_KEYS`] first, release, then swap
//! the platform's private key; the key id (`kid`) is the first 8 hex of
//! sha256(raw public key) on both sides.

use crate::policy::LocalPolicy;
use crate::record::ConnectionRecord;
use anyhow::{anyhow, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// The first line of the signed text; the platform's signer writes the same.
pub const CANONICAL_PREFIX: &str = "kareenos-desktop-record/1";

/// Hosts a record may name: these apexes and any of their subdomains.
pub const ALLOWED_HOST_SUFFIXES: &[&str] = &["kareenos.com", "sdnvision.services"];

/// The platform's record-signing public keys (raw Ed25519, base64), newest first.
pub const RECORD_KEYS: &[&str] = &["xNjRPfv/2eH4nYw/Uua2qWFutplde4ILzRxqANn8gbY="];

/// The TEST key of the platform selfcheck's vector. Trusted by DEBUG builds only.
pub const TEST_RECORD_KEY: &str = "jrDOnCurLmgpNMFKeRY7b8Trx2ByRUx3slvgHt+3370=";

/// A signed record older than this is refused ("make a new one").
pub const RECORD_MAX_AGE_SECS: i64 = 30 * 86_400;
/// Tolerated clock difference for a record dated in the future.
pub const RECORD_CLOCK_SKEW_SECS: i64 = 15 * 60;

/// The key id: the first 8 hex characters of sha256(raw public key).
pub fn kid_of(raw: &[u8]) -> String {
    Sha256::digest(raw).iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// A raw Ed25519 public key (base64) → (kid, key).
pub fn parse_key(b64: &str) -> Result<(String, VerifyingKey)> {
    let raw = STANDARD.decode(b64.trim()).map_err(|e| anyhow!("record key is not base64: {e}"))?;
    let arr: [u8; 32] = raw.as_slice().try_into().map_err(|_| anyhow!("record key is not 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&arr).map_err(|e| anyhow!("record key is not a valid Ed25519 key: {e}"))?;
    Ok((kid_of(&arr), key))
}

pub fn normalize_host(h: &str) -> String {
    h.trim().trim_end_matches('.').to_lowercase()
}

/// (scheme, host) of a URL, lower-cased.
pub fn split_url(u: &str) -> Result<(String, String)> {
    let p = url::Url::parse(u.trim())?;
    let host = p.host_str().ok_or_else(|| anyhow!("no host"))?;
    Ok((p.scheme().to_lowercase(), normalize_host(host)))
}

#[derive(Clone)]
pub struct Trust {
    keys: Vec<(String, VerifyingKey)>,
    extra_hosts: Vec<String>,
    require_signature: bool,
}

impl Trust {
    /// What a release trusts: the platform keys (plus the test key in a debug build)
    /// and the hosts the local policy adds — `extra_hosts`, and the host of a
    /// policy-set `ws_url` (local wins).
    pub fn production(policy: &LocalPolicy) -> Trust {
        let mut keys: Vec<(String, VerifyingKey)> = RECORD_KEYS.iter().filter_map(|k| parse_key(k).ok()).collect();
        if cfg!(debug_assertions) {
            if let Ok(k) = parse_key(TEST_RECORD_KEY) {
                keys.push(k);
            }
        }
        let mut extra: Vec<String> = policy.extra_hosts.iter().map(|h| normalize_host(h)).filter(|h| !h.is_empty()).collect();
        if let Some(u) = policy.ws_url.as_deref() {
            if let Ok((_, h)) = split_url(u) {
                extra.push(h);
            }
        }
        Trust { keys, extra_hosts: extra, require_signature: true }
    }

    /// Tests and rigs: explicit keys and extra hosts.
    pub fn with_keys(keys: Vec<(String, VerifyingKey)>, extra_hosts: Vec<String>, require_signature: bool) -> Trust {
        Trust { keys, extra_hosts: extra_hosts.iter().map(|h| normalize_host(h)).collect(), require_signature }
    }

    pub fn host_allowed(&self, host: &str) -> bool {
        let h = normalize_host(host);
        if h.is_empty() {
            return false;
        }
        let under = |s: &str| h == s || h.ends_with(&format!(".{s}"));
        ALLOWED_HOST_SUFFIXES.iter().any(|s| under(s)) || self.extra_hosts.iter().any(|s| under(s))
    }

    /// `what` names the address in the message ("server", "API", "Kareenos").
    pub fn check_url(&self, what: &str, url: &str, schemes: &[&str]) -> Result<()> {
        let (scheme, host) = split_url(url).map_err(|e| anyhow!("the {what} address is not a URL ({e})"))?;
        if !schemes.contains(&scheme.as_str()) {
            return Err(anyhow!("the {what} address must use {}://, not {scheme}://", schemes.join(":// or ")));
        }
        if !self.host_allowed(&host) {
            return Err(anyhow!(
                "the {what} address points at {host}, which is not a Kareenos server: this connection code did not come from Kareenos"
            ));
        }
        Ok(())
    }

    /// Every address of a record names a Kareenos host over wss / https (an empty
    /// address means the build default, which is one).
    pub fn check_hosts(&self, rec: &ConnectionRecord) -> Result<()> {
        if let Some(u) = rec.ws_url.as_deref().filter(|u| !u.trim().is_empty()) {
            self.check_url("server", u, &["wss"])?;
        }
        if let Some(u) = rec.api_url.as_deref().filter(|u| !u.trim().is_empty()) {
            self.check_url("API", u, &["https"])?;
        }
        if let Some(u) = rec.app_url.as_deref().filter(|u| !u.trim().is_empty()) {
            self.check_url("Kareenos", u, &["https"])?;
        }
        Ok(())
    }

    /// The signature and its date. Returns the key id that signed.
    pub fn verify_signature(&self, rec: &ConnectionRecord, now_secs: i64) -> Result<String> {
        let sig = rec
            .sig
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("this connection code is not signed by Kareenos (an older platform, or not Kareenos at all)"))?;
        let (kid, b64) = sig.split_once('.').ok_or_else(|| anyhow!("the connection code's signature is malformed"))?;
        let (_, key) = self
            .keys
            .iter()
            .find(|(k, _)| k == kid)
            .ok_or_else(|| anyhow!("the connection code is signed with a key this Desktop Agent does not know ({kid}): update the Desktop Agent"))?;
        let bytes = URL_SAFE_NO_PAD.decode(b64).map_err(|_| anyhow!("the connection code's signature is malformed"))?;
        let signature = Signature::from_slice(&bytes).map_err(|_| anyhow!("the connection code's signature is malformed"))?;
        key.verify_strict(rec.canonical().as_bytes(), &signature)
            .map_err(|_| anyhow!("the connection code was altered or did not come from Kareenos (bad signature)"))?;
        let iat = rec.iat.ok_or_else(|| anyhow!("the connection code carries no date"))?;
        if now_secs - iat > RECORD_MAX_AGE_SECS {
            return Err(anyhow!("this connection code is more than 30 days old: make a new one in Kareenos"));
        }
        if iat - now_secs > RECORD_CLOCK_SKEW_SECS {
            return Err(anyhow!("this connection code is dated in the future: check this computer's clock"));
        }
        Ok(kid.to_string())
    }

    /// Everything a NEW record must pass: shape, signature (when required), hosts.
    /// Returns the signing key id ("" when no signature was required).
    pub fn check_record(&self, rec: &ConnectionRecord, now_secs: i64) -> Result<String> {
        rec.validate()?;
        let kid = if self.require_signature { self.verify_signature(rec, now_secs)? } else { String::new() };
        self.check_hosts(rec)?;
        Ok(kid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::ComputerRef;

    // The platform selfcheck's vector (desktop_envelope_selfcheck.js), TEST key.
    fn vector() -> ConnectionRecord {
        ConnectionRecord {
            v: 2,
            channel: "desktop".into(),
            token: String::new(),
            code: Some("s3cr3t".into()),
            computer: Some(ComputerRef { id: Some("C1".into()), name: Some("Lab PC".into()), code: Some("LAB-01".into()), mode: Some("attended".into()) }),
            exp: None,
            session_id: None,
            projectid: None,
            accountid: None,
            userid: None,
            project_name: None,
            account_name: Some("Acme".into()),
            user_name: None,
            ws_url: Some("wss://x.kareenos.com/desktop-channel".into()),
            api_url: Some("https://x.kareenos.com/api".into()),
            app_url: Some("https://kareenos.com".into()),
            renewable: None,
            iat: Some(1_793_600_000),
            sig: Some("0ecdabbc.Hzhnq8ZfSrvXGfuyuj2O3uLAPfv9FZ8K_J8tBUzwkix89Shiv0v4c5dnZaLl0XsX6K5AJ2CRPxUERH0cfvdhAA".into()),
        }
    }
    fn test_trust() -> Trust {
        Trust::with_keys(vec![parse_key(TEST_RECORD_KEY).unwrap()], vec![], true)
    }

    #[test]
    fn canonical_text_matches_the_platform() {
        let c = vector().canonical();
        assert_eq!(
            c,
            "kareenos-desktop-record/1\nv=2\nchannel=desktop\ntoken=\ncode=s3cr3t\ncomputer_id=C1\ncomputer_code=LAB-01\nws_url=wss://x.kareenos.com/desktop-channel\napi_url=https://x.kareenos.com/api\napp_url=https://kareenos.com\niat=1793600000"
        );
    }

    #[test]
    fn the_platform_vector_verifies_and_a_change_does_not() {
        let t = test_trust();
        assert_eq!(parse_key(TEST_RECORD_KEY).unwrap().0, "0ecdabbc");
        assert_eq!(t.check_record(&vector(), 1_793_600_100).unwrap(), "0ecdabbc");
        let mut tampered = vector();
        tampered.ws_url = Some("wss://evil.example/desktop-channel".into());
        assert!(t.verify_signature(&tampered, 1_793_600_100).unwrap_err().to_string().contains("bad signature"));
        let mut unsigned = vector();
        unsigned.sig = None;
        assert!(t.check_record(&unsigned, 1_793_600_100).unwrap_err().to_string().contains("not signed"));
        assert!(t.check_record(&vector(), 1_793_600_000 + 31 * 86_400).unwrap_err().to_string().contains("30 days"));
        let mut other_key = vector();
        other_key.sig = Some("deadbeef.AAAA".into());
        assert!(t.verify_signature(&other_key, 1_793_600_100).unwrap_err().to_string().contains("does not know"));
    }

    #[test]
    fn only_kareenos_hosts_over_tls() {
        let t = Trust::with_keys(vec![], vec!["lab.example".into()], false);
        for ok in ["kareenos.com", "app.kareenos.com", "ap4.sdnvision.services", "LAB.example", "x.lab.example"] {
            assert!(t.host_allowed(ok), "{ok}");
        }
        for bad in ["kareenos.com.evil.net", "evilkareenos.com", "example.com", "", "sdnvision.services.attacker.io"] {
            assert!(!t.host_allowed(bad), "{bad}");
        }
        assert!(t.check_url("server", "wss://ap4.sdnvision.services:8092/desktop-channel", &["wss"]).is_ok());
        assert!(t.check_url("server", "ws://ap4.sdnvision.services:8092/desktop-channel", &["wss"]).unwrap_err().to_string().contains("wss://"));
        assert!(t.check_url("API", "https://api.evil.example/api", &["https"]).unwrap_err().to_string().contains("not a Kareenos server"));
        let mut rec = vector();
        rec.app_url = Some("https://kareenos.com.evil.net".into());
        assert!(t.check_hosts(&rec).is_err());
    }

    #[test]
    fn a_policy_ws_url_host_is_trusted_locally() {
        let mut pol = LocalPolicy::default();
        pol.ws_url = Some("wss://desktop.partner.example:8092/desktop-channel".into());
        pol.extra_hosts = vec![".internal.example".into()];
        let t = Trust::production(&pol);
        assert!(t.host_allowed("desktop.partner.example"));
        assert!(t.host_allowed("x.internal.example"));
        assert!(!t.host_allowed("partner.example"));
    }
}
