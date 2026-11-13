//! Blocking client for the Fruity Bridge HTTP API (see docs/api.md).
//!
//! Calls block; GTK apps run them on a worker thread and send results back to the main loop.

mod config;
mod events;
pub mod notes;
pub mod reminders;

use std::io::Write;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

pub use config::Config;
pub use events::{Event, Subscription, subscribe};

/// Budget for a whole read request.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for a whole write; the bridge's AppleScript alone may take 45 s.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Downloads have no total budget, only this much silence between reads.
const DOWNLOAD_IDLE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The bridge couldn't be reached (offline, Mac asleep, Tailscale down).
    #[error("Can't reach the Mac bridge: {0}")]
    Unreachable(String),
    /// No bridge address has been set yet. Not "offline": retrying can't help.
    #[error("No Mac bridge address is set")]
    NotConfigured,
    /// The bridge answered with an API error.
    #[error("{message}")]
    Api {
        status: u16,
        code: String,
        message: String,
        /// For `conflict`: the current server copy.
        current: Option<serde_json::Value>,
    },
    #[error("Unexpected response from the Mac bridge: {0}")]
    Decode(String),
}

impl Error {
    fn code(&self) -> Option<&str> {
        match self {
            Error::Api { code, .. } => Some(code),
            _ => None,
        }
    }
    pub fn is_conflict(&self) -> bool {
        self.code() == Some("conflict")
    }
    pub fn is_offline(&self) -> bool {
        matches!(self, Error::Unreachable(_))
    }
    pub fn is_not_found(&self) -> bool {
        self.code() == Some("not_found")
    }
    pub fn is_not_editable(&self) -> bool {
        self.code() == Some("not_editable")
    }

    /// A short, plain sentence for a toast or status line.
    pub fn user_message(&self) -> String {
        match self {
            Error::Unreachable(_) => {
                "Can't reach your Mac. Check it's awake and Tailscale is connected.".into()
            }
            Error::NotConfigured => "Add your Mac's bridge address in Preferences.".into(),
            Error::Api { status: 401 | 403, .. } => {
                "Your Mac turned this computer away. Check the bridge's allowed logins.".into()
            }
            Error::Api { code, message, .. } if code == "permission_missing" && !message.trim().is_empty() => {
                message.clone()
            }
            Error::Api { code, .. } if code == "conflict" => "Changed on another device.".into(),
            Error::Api { code, .. } if code == "not_found" => "That item no longer exists.".into(),
            Error::Api { message, .. } if !message.trim().is_empty() => message.clone(),
            Error::Api { status, .. } => format!("Your Mac couldn't do that (error {status})."),
            Error::Decode(_) => {
                "Your Mac sent something this app doesn't understand. Is the bridge up to date?".into()
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    base: String,
    agent: ureq::Agent,
    write_timeout: Duration,
}

#[derive(serde::Deserialize)]
struct ApiErrorBody {
    error: ApiError,
}

#[derive(serde::Deserialize)]
struct ApiError {
    code: String,
    message: String,
    #[serde(default)]
    current: Option<serde_json::Value>,
}

/// Percent-encodes an opaque id (or query value) for use in a URL.
pub fn encode_segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

type Response = std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>;

fn unreachable(e: impl std::fmt::Display) -> Error {
    Error::Unreachable(e.to_string())
}

fn api_error(status: u16, body: &str) -> Error {
    match serde_json::from_str::<ApiErrorBody>(body) {
        Ok(b) => Error::Api { status, code: b.error.code, message: b.error.message, current: b.error.current },
        Err(_) => Error::Api {
            status,
            code: "internal".into(),
            message: format!("The Mac bridge returned HTTP {status}."),
            current: None,
        },
    }
}

impl Client {
    pub fn new(config: &Config) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(READ_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Client { base: config.url.trim_end_matches('/').to_string(), agent, write_timeout: WRITE_TIMEOUT }
    }

    /// The same client with a different budget for writes (default 60 s), e.g. a short one for
    /// work that mustn't hold up quitting.
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> Result<String> {
        if self.base.trim().is_empty() {
            return Err(Error::NotConfigured);
        }
        Ok(format!("{}{}", self.base, path))
    }

    fn finish<T: DeserializeOwned>(resp: Response) -> Result<T> {
        let mut resp = resp.map_err(unreachable)?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().map_err(unreachable)?;
        if (200..300).contains(&status) {
            let text = if body.trim().is_empty() { "null" } else { &body };
            serde_json::from_str(text).map_err(|e| Error::Decode(e.to_string()))
        } else {
            Err(api_error(status, &body))
        }
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Self::finish(self.agent.get(&self.url(path)?).call())
    }

    pub fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let req = self.agent.post(&self.url(path)?).config().timeout_global(Some(self.write_timeout)).build();
        Self::finish(req.send_json(body))
    }

    pub fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let req = self.agent.put(&self.url(path)?).config().timeout_global(Some(self.write_timeout)).build();
        Self::finish(req.send_json(body))
    }

    pub fn patch<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let req = self.agent.patch(&self.url(path)?).config().timeout_global(Some(self.write_timeout)).build();
        Self::finish(req.send_json(body))
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        let req = self.agent.delete(&self.url(path)?).config().timeout_global(Some(self.write_timeout)).build();
        Self::finish::<serde_json::Value>(req.call()).map(|_| ())
    }

    /// Streams the body at `path` into `out` and returns its content type. Has no size limit and
    /// no total time limit, only an idle limit between reads.
    pub fn download(&self, path: &str, out: &mut dyn Write) -> Result<String> {
        let req = self
            .agent
            .get(&self.url(path)?)
            .config()
            .timeout_global(None)
            .timeout_per_read(Some(DOWNLOAD_IDLE))
            .build();
        let mut resp = req.call().map_err(unreachable)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(api_error(status, &body));
        }
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let mut reader = resp.into_body().into_reader();
        std::io::copy(&mut reader, out).map_err(unreachable)?;
        Ok(ctype)
    }

    pub fn health(&self) -> Result<Health> {
        self.get("/v1/health")
    }
}

#[derive(Debug, Clone, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub version: String,
    pub reminders: String,
    pub notes: String,
    pub notes_write: String,
}

/// Files under `~/.cache/fruity-bridge/`: a JSON cache so apps open instantly and show the last
/// copy while offline, plus whatever else an app keeps there (e.g. attachment bytes).
pub mod cache {
    use serde::{Serialize, de::DeserializeOwned};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `~/.cache/fruity-bridge`.
    pub fn dir() -> Option<PathBuf> {
        Some(dirs::cache_dir()?.join("fruity-bridge"))
    }

    /// Stable, filename-safe key for an opaque id (FNV-1a; ids contain `/` and `:`).
    pub fn short_hash(s: &str) -> String {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("{h:016x}")
    }

    /// Cache name for something identified by an opaque id, e.g. `key("notes-note", id)`.
    pub fn key(prefix: &str, id: &str) -> String {
        format!("{prefix}-{}", short_hash(id))
    }

    fn path(name: &str) -> Option<PathBuf> {
        let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        Some(dir()?.join(format!("{safe}.json")))
    }

    pub fn load<T: DeserializeOwned>(name: &str) -> Option<T> {
        let data = std::fs::read(path(name)?).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Writes `value` as JSON. Blocking; GTK apps call it off the main thread.
    pub fn store<T: Serialize + ?Sized>(name: &str, value: &T) {
        let Some(p) = path(name) else { return };
        if let Ok(data) = serde_json::to_vec(value) {
            let _ = write_atomic(&p, &data);
        }
    }

    /// Writes `bytes` to a temporary file next to `path`, then renames it into place, creating
    /// the directory if needed.
    pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // A name per write, so concurrent writers of the same file can't interleave.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn keys_hide_ids() {
            let k = key("notes-note", "x-coredata://STORE/ICNote/p1");
            assert!(k.starts_with("notes-note-"));
            assert!(k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
            assert_ne!(k, key("notes-note", "x-coredata://STORE/ICNote/p2"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_helpers() {
        let api = |status, code: &str| Error::Api { status, code: code.into(), message: String::new(), current: None };
        assert!(api(404, "not_found").is_not_found());
        assert!(api(422, "not_editable").is_not_editable());
        assert!(api(409, "conflict").is_conflict());
        assert!(Error::Unreachable("x".into()).is_offline());
        assert!(!Error::NotConfigured.is_offline());
        assert!(Error::NotConfigured.user_message().contains("Preferences"));
        assert_eq!(api(500, "internal").user_message(), "Your Mac couldn't do that (error 500).");
        assert!(api(403, "forbidden").user_message().contains("allowed logins"));
    }

    #[test]
    fn encodes_ids() {
        assert_eq!(encode_segment("x-coredata://S/ICNote/p1"), "x-coredata%3A%2F%2FS%2FICNote%2Fp1");
    }
}
