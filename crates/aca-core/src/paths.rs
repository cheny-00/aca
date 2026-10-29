//! Where aca keeps its things, and a few small tools used everywhere (time, random ids, atomic writes).
//!
//! - Configuration: `~/.config/aca/` (config.toml, plugins.json)
//! - Registry: `~/.local/share/aca/aca.db`
//! - Runtime state: `~/.local/state/aca/` (run/<id>.json, events.jsonl, inbox/, hooks/, plugin-logs/)
//!
//! With `ACA_HOME` set, everything goes into that one directory (config / data / state subdirectories): for tests and benchmarks,
//! so the real ones can't be touched.

use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Res<T> = Result<T, String>;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn root(sub: &str, xdg: &str) -> PathBuf {
    match std::env::var_os("ACA_HOME").filter(|v| !v.is_empty()) {
        Some(h) => PathBuf::from(h).join(sub),
        None => home().join(xdg).join("aca"),
    }
}

pub fn config_dir() -> PathBuf {
    root("config", ".config")
}

pub fn data_dir() -> PathBuf {
    root("data", ".local/share")
}

pub fn state_dir() -> PathBuf {
    root("state", ".local/state")
}

pub fn db() -> PathBuf {
    data_dir().join("aca.db")
}

/// A session's runtime state (hooks write it, the list reads it)
pub fn run_file(id: &str) -> PathBuf {
    state_dir().join("run").join(format!("{id}.json"))
}

pub fn events_log() -> PathBuf {
    state_dir().join("events.jsonl")
}

/// Inbox: `inbox/<id>.jsonl` (messages), `.state` (which ones were delivered), `.bell` (the doorbell)
pub fn inbox_dir() -> PathBuf {
    state_dir().join("inbox")
}

/// Paths in the configuration may contain ~
pub fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest).to_string_lossy().into_owned(),
        None if p == "~" => home().to_string_lossy().into_owned(),
        None => p.to_string(),
    }
}

/// For comparing whether two directories are the same: expand ~, resolve to the real path when possible (/tmp → /private/tmp),
/// drop a trailing /
pub fn real(p: &str) -> String {
    let p = expand(p);
    let p = Path::new(&p).canonicalize().map(|x| x.to_string_lossy().into_owned()).unwrap_or(p);
    if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn random(buf: &mut [u8]) {
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).is_err() {
        // If it can't be read, make do with the time and pid: only used to make ids, not passwords
        let seed = now_ms() as u128 * 6364136223846793005 + std::process::id() as u128;
        buf.iter_mut().enumerate().for_each(|(i, b)| *b = (seed >> (i % 16 * 8)) as u8);
    }
}

/// Session id: 8 hex digits
pub fn short_id() -> String {
    let mut b = [0u8; 4];
    random(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Claude's `--session-id` wants a v4 UUID
pub fn uuid4() -> String {
    let mut b = [0u8; 16];
    random(&mut b);
    b[6] = b[6] & 0x0f | 0x40;
    b[8] = b[8] & 0x3f | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Write a temporary file first, then rename: several processes (hooks, the interface, the CLI) read and write at once, and none
/// reads half a file. The original file's permissions are kept: if you made it readable only by you, a rewrite doesn't reset it
/// to the default
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    write_as(path, text, std::fs::metadata(path).ok().map(|m| m.permissions()))
}

/// Same as above, but always readable and writable only by you (0600): for files with secrets (plugin settings in plugins.toml)
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    write_as(path, text, Some(std::fs::Permissions::from_mode(0o600)))
}

fn write_as(path: &Path, text: &str, perm: Option<std::fs::Permissions>) -> std::io::Result<()> {
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", path.file_name().unwrap().to_string_lossy(), std::process::id()));
    match perm {
        None => std::fs::write(&tmp, text)?,
        Some(perm) => {
            // The temporary file is readable only by you from the moment it's created, and gets the wanted permissions after writing:
            // it may hold secrets, so there's no moment when anyone can read it
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.set_permissions(perm)?;
        }
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_the_shape_tools_expect() {
        let u = uuid4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_eq!(short_id().len(), 8);
        assert_ne!(short_id(), short_id());
    }

    #[test]
    fn tilde_expands_only_at_the_start() {
        let h = home().to_string_lossy().into_owned();
        assert_eq!(expand("~/a/b"), format!("{h}/a/b"));
        assert_eq!(expand("~"), h);
        assert_eq!(expand("/x/~/y"), "/x/~/y");
    }

    #[test]
    fn rewriting_keeps_a_files_mode_and_private_files_are_only_yours() {
        let dir = std::env::temp_dir().join(format!("aca-paths-{}", short_id()));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // One you tightened: a rewrite doesn't loosen it
        let kept = dir.join("kept.toml");
        write_atomic(&kept, "a = 1\n").unwrap();
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&kept, "a = 2\n").unwrap();
        assert_eq!((std::fs::read_to_string(&kept).unwrap().as_str(), mode(&kept)), ("a = 2\n", 0o600));
        // With secrets: new ones, and ones anyone could read before, are readable only by you after writing
        let secret = dir.join("secret.toml");
        write_private(&secret, "t = 1\n").unwrap();
        assert_eq!(mode(&secret), 0o600);
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&secret, "t = 2\n").unwrap();
        assert_eq!((std::fs::read_to_string(&secret).unwrap().as_str(), mode(&secret)), ("t = 2\n", 0o600));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
