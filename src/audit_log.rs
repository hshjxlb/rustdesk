//! Local, opt-in, human-readable audit log. One line per event.
//!
//! Design rules, in order of importance:
//!   1. Never panic. Never abort a connection or a login because logging failed.
//!   2. Never block the caller for longer than a `thread::spawn` costs.
//!   3. Write nothing at all unless the user ticked "Enable logging".
//!
//! Records:
//!   LOGIN            account name + this device's id
//!   LOGOUT           account name + this device's id
//!   CONNECT_IN       a peer connected *into* this machine (we are the controlled side)
//!   DISCONNECT_IN    that peer left; includes how long it stayed connected
//!
//! The file is plain text so it can be read in Notepad and grepped:
//!
//! ```text
//! 2026-09-28 10:09:25 | LOGIN | user=admin | device_id=123456789
//! 2026-09-28 10:21:03 | CONNECT_IN | peer=987654321 | type=remote
//! 2026-09-28 10:35:41 | DISCONNECT_IN | peer=987654321 | type=remote | duration=00:14:38
//! 2026-09-28 11:02:10 | LOGOUT | user=admin | device_id=123456789
//! ```

use hbb_common::{
    config::{Config, LocalConfig},
    log,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// Opt-in switch. Deliberately uses the `allow-` prefix: `bool2option()` /
/// `option2bool()` treat `allow-*` as FALSE unless the stored value is exactly
/// "Y", so an unset option means "logging off". An `enable-*` key would default
/// to ON, which is the opposite of what an audit log should do.
pub const OPTION_ALLOW_AUDIT_LOG: &str = "allow-audit-log";

/// Where to write. Empty means "use the default location".
pub const OPTION_AUDIT_LOG_PATH: &str = "audit-log-path";

/// Default file name when no path is configured, and when the configured value
/// points at a directory.
const DEFAULT_FILE_NAME: &str = "audit.log";

/// Roll the file over once it passes this size, keeping one previous generation
/// as `audit.log.1`. Without this a chatty machine could grow the file without
/// bound, since audit traffic is not covered by the main logger's rotation.
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Cheapest possible check, and the one that keeps the feature honest: when the
/// user has not ticked the box, every entry point below returns before it does
/// any I/O at all.
#[inline]
pub fn is_enabled() -> bool {
    LocalConfig::get_bool_option(OPTION_ALLOW_AUDIT_LOG)
}

/// Resolve the file to append to.
///
/// A configured value is used as-is; if it looks like a directory (trailing
/// separator, or it already exists as one) the default file name is appended, so
/// both "D:\logs" and "D:\logs\audit.log" do what the user meant. An empty
/// configured value falls back to the per-user data directory.
pub fn resolve_path() -> PathBuf {
    let configured = LocalConfig::get_option(OPTION_AUDIT_LOG_PATH);
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return Config::path(DEFAULT_FILE_NAME);
    }
    let path = PathBuf::from(trimmed);
    let looks_like_dir = trimmed.ends_with('/')
        || trimmed.ends_with('\\')
        || path.is_dir();
    if looks_like_dir {
        path.join(DEFAULT_FILE_NAME)
    } else {
        path
    }
}

/// `2026-09-28 10:09:25`, local time. `chrono` is already a dependency of
/// `hbb_common` (re-exported at its crate root) so this needs no new dependency.
fn now_string() -> String {
    hbb_common::chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Append one already-formatted record. Every failure is downgraded to a
/// warning: a full disk, a revoked directory or a path the user typo'd must not
/// be able to break the connection whose event we are trying to record.
pub fn write_line(event: &str) {
    if !is_enabled() {
        return;
    }

    let path = resolve_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::warn!(
                    "audit_log: cannot create directory {:?}: {}",
                    parent,
                    e
                );
                return;
            }
        }
    }

    rotate_if_needed(&path);

    let line = format!("{} | {}\n", now_string(), event);
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(line.as_bytes()) {
                log::warn!("audit_log: cannot write to {:?}: {}", path, e);
            }
        }
        Err(e) => log::warn!("audit_log: cannot open {:?}: {}", path, e),
    }
}

/// One generation of history: `audit.log` -> `audit.log.1`. Best effort; a
/// failure here only means the size cap is not enforced this time.
fn rotate_if_needed(path: &Path) {
    let too_big = fs::metadata(path).map(|m| m.len() > MAX_FILE_SIZE).unwrap_or(false);
    if !too_big {
        return;
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(".1");
    let backup = PathBuf::from(backup);
    // Ignore the error if the previous generation cannot be replaced; we would
    // rather keep growing the current file than lose both.
    if let Err(e) = fs::rename(path, &backup) {
        log::warn!("audit_log: cannot rotate {:?}: {}", path, e);
    }
}

/// Write from a detached thread so neither a slow disk nor a network share can
/// stall the session loop or the login path that triggered us.
fn spawn_line(event: String) {
    if !is_enabled() {
        return;
    }
    std::thread::spawn(move || write_line(&event));
}

/// Account name of the signed-in API user, or `unknown`.
///
/// Rust never sees the `/api/login` response (that request is made by the
/// Flutter side), but the Dart code persists the user payload as the
/// `user_info` local option, so we read it back from there.
fn account_name() -> String {
    let raw = LocalConfig::get_option("user_info");
    if raw.trim().is_empty() {
        return "unknown".to_owned();
    }
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Record that a user signed in to the API server.
pub fn log_login() {
    let event = format!(
        "LOGIN | user={} | device_id={}",
        account_name(),
        Config::get_id()
    );
    spawn_line(event);
}

/// Record that a user signed out of the API server.
pub fn log_logout() {
    let event = format!(
        "LOGOUT | user={} | device_id={}",
        account_name(),
        Config::get_id()
    );
    spawn_line(event);
}

/// Record an inbound connection to this machine (we are the controlled side).
/// `peer_id` is the controlling machine's id.
pub fn log_incoming_connect(peer_id: &str, conn_type: &str) {
    let event = format!(
        "CONNECT_IN | peer={} | type={}",
        peer_id, conn_type
    );
    spawn_line(event);
}

/// Record that an inbound connection ended, including how long it lasted.
pub fn log_incoming_disconnect(peer_id: &str, conn_type: &str, duration: &str) {
    let event = format!(
        "DISCONNECT_IN | peer={} | type={} | duration={} | at={}",
        peer_id,
        conn_type,
        duration,
        now_string()
    );
    spawn_line(event);
}

/// `00:14:38` — hours are not capped at 24, so a session left open for days
/// reads as `49:12:05` rather than wrapping around.
pub fn format_duration(d: std::time::Duration) -> String {
    let total = d.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Human-readable connection type for the log line. The controlled side already
/// derives a numeric code for its audit upload; this mirrors the same set.
pub fn conn_type_label(kind: &str) -> &'static str {
    match kind {
        "file_transfer" => "file_transfer",
        "port_forward" => "port_forward",
        "view_camera" => "view_camera",
        "terminal" => "terminal",
        _ => "remote",
    }
}
