//! Kareenos Desktop Agent — the Windows half of the Kareenos Desktop Channel (§5.134).
//!
//! The twin of the Kareenos Extension (mcp-chrome fork): one OUTBOUND wss
//! connection to the Kareenos browser tier (`/desktop-channel`), bound with the
//! durable channel token from the sign-in hand-off, a single command queue (one
//! keyboard, one mouse), and an adapter that maps the bounded thirteen actions
//! onto the Terminator core (Windows UI Automation). The server is the sole
//! command source; nothing here listens on any port.
//!
//! Modules
//! - `envelope`  the frozen contract (mirror of `desktop_channel_envelope.js`)
//! - `record`    the connection record the sign-in page hands over
//! - `store`     the token store (Windows Credential Manager) and the data dir
//! - `presence`  "the person is active" (GetLastInputInfo) and the pause flag
//! - `policy`    the computer's local policy file (local wins)
//! - `refbook`   per-lane snapshot refs (`e<n>`) and epochs
//! - `adapter`   action → Terminator
//! - `queue`     one queue, deadlines, acks, overrun handling
//! - `client`    the wss client: bind, ping, token renewal, reconnect, outbox

pub mod adapter;
pub mod client;
pub mod envelope;
pub mod policy;
pub mod presence;
pub mod queue;
pub mod record;
pub mod refbook;
pub mod store;
pub mod trust;

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Build-time default; the sign-in record's `ws_url` and `--server` override it.
pub const DEFAULT_WS_URL: &str = "wss://ap4.sdnvision.services:8092/desktop-channel";
pub const URL_SCHEME: &str = "kareenos-desktop";
