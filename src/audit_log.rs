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
//! 2026-09-28 10:21:03 | CONNECT_IN | user=Hshjxlb | peer=491230743 | ip=1.2.3.4 | type=remote
//! 2026-09-28 10:35:41 | DISCONNECT_IN | user=Hshjxlb | peer=491230743 | ip=1.2.3.4 | type=remote | duration=00:14:38 | at=2026-09-28 10:35:41
//! 2026-09-28 11:02:10 | LOGOUT | user=admin | device_id=123456789
//! ```
//!
//! On CONNECT_IN / DISCONNECT_IN lines `user=` is the CONTROLLING side's
//! name (what the accept dialog shows as "X 请求访问你的设备"), so the line
//! answers "who connected in" (v9). Every admitted session is recorded: the
//! old v7 rule of skipping machines where nobody signed into an API account
//! was dropped in v13, because on machines that never sign in (the common
//! case) it kept the log empty forever. LOGIN / LOGOUT remain the local
//! account's own events.
//!
//! Storage: the switch, the path and the mirrored account name live in the
//! **machine-level** `Config` store, never in `LocalConfig`. On Windows the
//! inbound-connection hooks run inside the SYSTEM service, whose config dir
//! is remapped by hbb_common's `patch()` to `ServiceProfiles\LocalService` —
//! a `LocalConfig` flag ticked in the UI process would be invisible there,
//! and a `user_info` sign-in would be unknown there. `Config` + the
//! `ipc::set_options` round trip (the same channel upstream uses for
//! `direct-server`, `access-mode`, ...) keeps both processes in agreement.

use hbb_common::{config::Config, log};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// Opt-in switch. Deliberately uses the `allow-` prefix: `bool2option()` /
/// `option2bool()` treat `allow-*` as FALSE unless the stored value is exactly
/// "Y", so an unset option means "logging off". An `enable-*` key would default
/// to ON, which is the opposite of what an audit log should do.
///
/// Stored in the machine-level `Config` store (not `LocalConfig`): the flag is
/// read by the SYSTEM service, which has a separate per-process local store.
pub const OPTION_ALLOW_AUDIT_LOG: &str = "allow-audit-log";

/// Where to write. Empty means "use the default location".
///
/// Machine-level `Config` store, for the same reason as the switch: both the
/// UI process and the SYSTEM service resolve the file, and they must agree.
pub const OPTION_AUDIT_LOG_PATH: &str = "audit-log-path";

/// Mirror of the API account signed in on this machine, for stamping
/// connection lines written by the service. Maintained by `sync_account()`
/// from the UI process; empty means "nobody is signed in".
pub const OPTION_AUDIT_ACCOUNT: &str = "audit-account";

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
    let enabled = Config::get_bool_option(OPTION_ALLOW_AUDIT_LOG);
    log::info!(
        "audit_log: is_enabled={} (option value='{}')",
        enabled,
        Config::get_option(OPTION_ALLOW_AUDIT_LOG)
    );
    enabled
}

/// Resolve the file to append to.
///
/// A configured value is used as-is; if it looks like a directory (trailing
/// separator, or it already exists as one) the default file name is appended, so
/// both "D:\logs" and "D:\logs\audit.log" do what the user meant. An empty
/// configured value falls back to the default location (see `default_path`).
///
/// A leading `~` is expanded to the user's home (see `expand_tilde`) — Rust's
/// `PathBuf` does not do this on its own, so a typed "~/RustDesk/audit.log"
/// would otherwise create a literal `~` directory and "mysteriously" not
/// produce a file where the user is looking for one.
pub fn resolve_path() -> PathBuf {
    let configured = Config::get_option(OPTION_AUDIT_LOG_PATH);
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return default_path();
    }
    let path = expand_tilde(trimmed);
    let looks_like_dir = trimmed.ends_with('/')
        || trimmed.ends_with('\\')
        || path.is_dir();
    if looks_like_dir {
        path.join(DEFAULT_FILE_NAME)
    } else {
        path
    }
}

/// Expand a leading `~` / `~/` / `~\` to the user's home directory.
///
/// Every other position of a tilde (inside the path, escaped-looking "~user"
/// home shorthand) is left untouched: this is not a shell, and guessing more
/// than the common "path starts with ~" case invites surprises. When no home
/// can be determined the input is returned as-is.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_for_expansion().unwrap_or_else(|| PathBuf::from(path));
    }
    let rest = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"));
    match rest {
        Some(rest) => match home_for_expansion() {
            Some(home) if rest.is_empty() => home,
            Some(home) => home.join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

/// The home directory used by both `default_path` and `expand_tilde`.
///
/// Same rule on Linux as in `default_path`: prefer the *active desktop user's*
/// home (the inbound hooks run in the root service, where `$HOME` is `/root`)
/// so the service and the UI process expand a configured path to the same
/// file. Windows has no tilde tradition but `$USERPROFILE` still makes
/// `~/logs/audit.log` work if someone types it.
fn home_for_expansion() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::get_active_user_home()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// The fallback file. On Windows this deliberately does NOT use
/// `Config::path()`: that applies hbb_common's `patch()`, which would resolve
/// to `ServiceProfiles\LocalService\...` inside the service but to the user's
/// `%APPDATA%` in the UI process — two different files for the same log. A
/// fixed machine-wide location is writable by both (SYSTEM always is; the UI
/// pre-creates the file via `ensure_file()`, which makes it the CREATOR OWNER
/// with full rights, and SYSTEM keeps full access through inherited ACEs).
#[inline]
fn default_path() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
            .join("RustDesk")
            .join(DEFAULT_FILE_NAME)
    }
    #[cfg(target_os = "linux")]
    {
        // Linux: `~/RustDesk/audit.log` under the home of the *active desktop
        // user*, not `$HOME`. The inbound-connection hooks run inside the
        // root service, where `$HOME` is `/root`; resolving from the active
        // user instead keeps the service and the UI process writing to the
        // same file (otherwise the log would silently split in two). The UI
        // process, which is the active user itself, resolves identically.
        let home = crate::platform::linux::get_active_user_home()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
        // info, not debug: this is the first place to look when the file does
        // not appear where the user expects it, and the default level filters
        // debug out.
        log::info!(
            "audit_log: linux default_path: get_active_user_home={:?}, HOME env={:?}",
            crate::platform::linux::get_active_user_home(),
            std::env::var_os("HOME")
        );
        match home {
            Some(home) => home.join("RustDesk").join(DEFAULT_FILE_NAME),
            None => PathBuf::from("RustDesk").join(DEFAULT_FILE_NAME),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Config::path(DEFAULT_FILE_NAME)
    }
}

/// `2026-09-28 10:09:25`, local time. `chrono` is already a dependency of
/// `hbb_common` (re-exported at its crate root) so this needs no new dependency.
fn now_string() -> String {
    hbb_common::chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Append one already-formatted record if logging is on. Every failure is
/// downgraded to a warning: a full disk, a revoked directory or a path the
/// user typo'd must not be able to break the connection whose event we are
/// trying to record.
pub fn write_line(event: &str) {
    if !is_enabled() {
        return;
    }
    write_line_unchecked(event);
}

/// Append without consulting the stored flag. Used by `note_switch`, which
/// runs right after the new flag value was persisted but must also record
/// the switch being turned OFF — after which the gate above would block
/// every write.
fn write_line_unchecked(event: &str) {
    let path = resolve_path();
    log::info!("audit_log: attempting write to {:?}: {}", path, event);
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

    // Windows Notepad (including the one shipped with Windows Server and
    // older Win10 builds) does not treat a bare LF as a line break — an
    // LF-only file shows up as one glued line. CRLF is the only ending it
    // renders reliably, so Windows gets CRLF; other platforms keep LF.
    #[cfg(windows)]
    let line = format!("{} | {}\r\n", now_string(), event);
    #[cfg(not(windows))]
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

/// Called from the FFI layer when the user changes the "Enable logging"
/// switch, AFTER the new value has been persisted. Turning it ON pre-creates
/// the directory and an empty file right away: the user sees the file appear
/// the moment they tick the box (instead of only after the first event),
/// and — on Windows — the UI process becomes the file's CREATOR OWNER, so
/// both the user account and the SYSTEM service can append to it regardless
/// of which process writes first.
///
/// v7: the switch event itself is no longer written to the log — the file
/// records account activity only (LOGIN / LOGOUT / authenticated
/// CONNECT_IN / DISCONNECT_IN). The visible feedback for enabling is the
/// file simply appearing.
pub fn note_switch(raw_value: &str) {
    let on = hbb_common::config::option2bool(OPTION_ALLOW_AUDIT_LOG, raw_value);
    if on {
        ensure_file();
    }
}

/// Best-effort pre-creation of the log file. Never fatal.
pub fn ensure_file() {
    let path = resolve_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::warn!("audit_log: cannot create directory {:?}: {}", parent, e);
                return;
            }
        }
    }
    if fs::metadata(&path).is_err() {
        if let Err(e) = OpenOptions::new().create(true).append(true).open(&path) {
            log::warn!("audit_log: cannot pre-create {:?}: {}", path, e);
        }
    }
    // On Linux the file is shared between the root service and the desktop
    // user: whoever creates it first must leave it appendable by the other,
    // or half the events would fail with EACCES (and only show up as a
    // warning in the service log).
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o666));
        if let Some(parent) = path.parent() {
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o777));
        }
    }
}

/// `ensure_file` but only when the switch is on. Called by the service
/// whenever a fresh options map arrives over IPC: the UI process pre-creates
/// the file when the switch is ticked, but if that run failed (odd home
/// resolution, sandboxing, ...) the service retries with its own resolution,
/// so "ticked the box, no file appeared" has a second chance to self-heal.
pub fn ensure_file_if_enabled() {
    if is_enabled() {
        ensure_file();
    }
}

/// Extract the account name from a raw `user_info` JSON string.
///
/// Public because the FFI sign-out path must parse the *pre-write* snapshot:
/// by the time a LOGOUT event fires the `user_info` option has already been
/// cleared. Empty input or a parse failure falls back to `unknown`.
pub fn account_name_from(raw: &str) -> String {
    if raw.trim().is_empty() {
        return "unknown".to_owned();
    }
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Trim a caller-supplied account name, defaulting to `unknown` when empty.
fn clean_account(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Trim a peer address, defaulting to `unknown` when empty. The `ip` handed in
/// is the controlled side's own view of the peer: the real remote address for
/// direct / LAN sessions, and the address hbbs reported for relayed ones.
fn clean_ip(ip: &str) -> String {
    let trimmed = ip.trim();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The API account signed in on this machine right now, or `none` when no
/// one is logged in. Kept for diagnostics and future per-account stamping;
/// connection lines themselves are written unconditionally since v13.
///
/// Reads the `audit-account` mirror, NOT `user_info`: this runs inside the
/// SYSTEM service on Windows, whose `LocalConfig` is a separate store that
/// never receives the UI's sign-in state (and stays empty for OIDC sign-ins
/// that skip "remember me" anyway).
pub fn current_account() -> String {
    let name = Config::get_option(OPTION_AUDIT_ACCOUNT);
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "none".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Push the signed-in account name into the machine-level `Config` store and
/// the service process. The service stamps CONNECT_IN / DISCONNECT_IN lines
/// with the mirror (see `current_account`); it cannot see this process's
/// `LocalConfig`, so the mirror is the only channel.
///
/// `ipc::set_option` writes our own `Config` copy AND the service's in one
/// call, and degrades gracefully to local-only when no service is installed
/// (portable mode, where the UI process runs the server itself anyway).
/// An empty string clears the mirror (sign-out).
///
/// Fire-and-forget on a detached thread: the IPC round trips must never stall
/// the settings write or the login path that triggered the mirror.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn sync_account(account: &str) {
    let account = account.trim().to_owned();
    std::thread::spawn(move || {
        crate::ipc::set_option(OPTION_AUDIT_ACCOUNT, &account);
    });
}

/// Mobile single-process variant: no service, no IPC — write the mirror directly.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn sync_account(account: &str) {
    Config::set_option(OPTION_AUDIT_ACCOUNT.to_owned(), account.trim().to_owned());
}

/// Record that a user signed in to the API server.
///
/// The caller supplies the name. The FFI sign-in path re-reads the freshly
/// written `user_info` and parses it with `account_name_from`; the OIDC path
/// takes the name straight from the auth response, which also covers the
/// "don't remember me" case where `user_info` is never persisted.
pub fn log_login(account: &str) {
    let event = format!(
        "LOGIN | user={} | device_id={}",
        clean_account(account),
        Config::get_id()
    );
    spawn_line(event);
}

/// Record that a user signed out of the API server.
///
/// The caller supplies the name: for the FFI sign-out path the `user_info`
/// option is already cleared when we run, so the caller passes the name
/// parsed from the pre-write snapshot via `account_name_from(old_raw)`.
pub fn log_logout(account: &str) {
    let event = format!(
        "LOGOUT | user={} | device_id={}",
        clean_account(account),
        Config::get_id()
    );
    spawn_line(event);
}

/// Record an account switch (re-login as someone else without an explicit
/// sign-out first): the previous identity's session ended and a new one
/// began. Both lines are written from ONE detached thread so they cannot
/// interleave with each other or swap their order.
pub fn log_switch(old_account: &str, new_account: &str) {
    let logout = format!(
        "LOGOUT | user={} | device_id={}",
        clean_account(old_account),
        Config::get_id()
    );
    let login = format!(
        "LOGIN | user={} | device_id={}",
        clean_account(new_account),
        Config::get_id()
    );
    std::thread::spawn(move || {
        write_line(&logout);
        write_line(&login);
    });
}

/// Marked by the explicit Dart sign-out (`main_audit_logout`) right after it
/// wrote its LOGOUT line. The `reset()` that always follows clears
/// `user_info`, and the resulting (non-empty -> empty) transition must not
/// log the same sign-out twice. Process-local on purpose: both sides run in
/// the UI process.
static EXPLICIT_LOGOUT_PENDING: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Remember that a LOGOUT line was just written by the explicit sign-out and
/// its `user_info` clear is still pending.
pub fn mark_explicit_logout() {
    if let Ok(mut pending) = EXPLICIT_LOGOUT_PENDING.lock() {
        *pending = true;
    }
}

/// Consume the pending mark. Returns true when the (non-empty -> empty)
/// `user_info` transition being processed is just the echo of the explicit
/// sign-out, which already recorded its LOGOUT line.
pub fn take_explicit_logout() -> bool {
    EXPLICIT_LOGOUT_PENDING
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or(false)
}

/// Record an inbound connection to this machine (we are the controlled side).
/// `peer_name` is the name the CONTROLLING side presents (its LoginRequest
/// `my_name` — the same name the accept dialog shows), stamped as `user=` so
/// the line answers "who connected in"; `peer_id` / `ip` identify and locate
/// that peer.
///
/// v13: the v7 gate (record only when an API account is signed in on this
/// machine) is removed. On machines that never sign in the gate swallowed
/// every event, leaving an empty file behind a ticked switch; the only real
/// gate is now the "Enable logging" switch itself, checked in `spawn_line`.
pub fn log_incoming_connect(peer_name: &str, peer_id: &str, ip: &str, conn_type: &str) {
    let event = format!(
        "CONNECT_IN | user={} | peer={} | ip={} | type={}",
        clean_account(peer_name),
        peer_id,
        clean_ip(ip),
        conn_type
    );
    spawn_line(event);
}

/// Record that an inbound connection ended, including how long it lasted.
/// `peer_name` is the controlling side's name (stamped as `user=` right after
/// the event, matching CONNECT_IN); `ip` comes from the same session-lifetime
/// snapshot. See `log_incoming_connect` for why the v7 sign-in gate is gone.
pub fn log_incoming_disconnect(
    peer_name: &str,
    peer_id: &str,
    ip: &str,
    conn_type: &str,
    duration: &str,
) {
    let event = format!(
        "DISCONNECT_IN | user={} | peer={} | ip={} | type={} | duration={} | at={}",
        clean_account(peer_name),
        peer_id,
        clean_ip(ip),
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
