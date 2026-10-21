//! What logship remembers between runs: the settings, the open tabs and the
//! recently opened files. Stored as JSON in the user's config directory:
//!
//! - Linux: `$XDG_CONFIG_HOME/logship/session.json` (default `~/.config/...`)
//! - macOS: `~/Library/Application Support/logship/session.json`
//! - Windows: `%APPDATA%\logship\session.json`

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// Recently opened files, most recent first; shown on empty tabs.
pub struct RecentFiles(pub Vec<PathBuf>);

impl gpui::Global for RecentFiles {}

/// How many recently opened files are remembered.
pub const MAX_RECENT: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub settings: Settings,
    /// Files open in tabs, left to right.
    pub tabs: Vec<PathBuf>,
    /// Index into `tabs` of the tab that was in front.
    pub active: usize,
    /// Most recently opened first.
    pub recent: Vec<PathBuf>,
}

impl Session {
    /// Reads the saved session. A missing or unreadable file gives the defaults.
    pub fn load() -> Session {
        let Some(path) = session_path() else {
            return Session::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => Session::from_json(&bytes).unwrap_or_else(|err| {
                eprintln!("logship: ignoring {}: {err}", path.display());
                Session::default()
            }),
            Err(_) => Session::default(),
        }
    }

    fn from_json(bytes: &[u8]) -> serde_json::Result<Session> {
        let mut session: Session = serde_json::from_slice(bytes)?;
        session.settings = session.settings.sanitized();
        session.recent.truncate(MAX_RECENT);
        Ok(session)
    }

    /// Saves the session, replacing the file atomically. Errors are reported on
    /// stderr only: losing the session must never stop the app.
    pub fn save(&self) {
        let Some(path) = session_path() else {
            return;
        };
        if let Err(err) = write_atomically(&path, self) {
            eprintln!("logship: could not save {}: {err}", path.display());
        }
    }
}

/// Moves `path` to the front of `recent`, dropping duplicates and old entries.
pub fn push_recent(recent: &mut Vec<PathBuf>, path: &Path) {
    recent.retain(|p| p != path);
    recent.insert(0, path.to_path_buf());
    recent.truncate(MAX_RECENT);
}

/// Writes the session readable by the user only (it lists the files they
/// opened), through a temporary file named after the process so two running
/// instances can't write into the same one.
fn write_atomically(path: &Path, session: &Session) -> std::io::Result<()> {
    let dir = path.parent().expect("session path has a parent");
    create_private_dir(dir)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let written = write_private(&tmp, &serde_json::to_vec_pretty(session)?)
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    // A leftover file would keep its old permissions, so start from scratch;
    // `create_new` also refuses to follow a symlink planted at `path`.
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

fn session_path() -> Option<PathBuf> {
    Some(config_dir()?.join("logship").join("session.json"))
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library").join("Application Support"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home_dir().map(|h| h.join(".config")))
    }
}

#[cfg(not(target_os = "windows"))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_fields() {
        let session = Session {
            settings: Settings {
                font_family: "Hack".into(),
                font_size: 14.,
                dark: false,
                newest_first: true,
            },
            tabs: vec!["/var/log/a.log".into(), "/var/log/b.log".into()],
            active: 1,
            recent: vec!["/var/log/b.log".into()],
        };
        let json = serde_json::to_vec(&session).unwrap();
        assert_eq!(Session::from_json(&json).unwrap(), session);

        let partial =
            Session::from_json(br#"{"tabs":["/x.log"],"settings":{"dark":false}}"#).unwrap();
        assert_eq!(partial.tabs, vec![PathBuf::from("/x.log")]);
        assert!(!partial.settings.dark);
        assert_eq!(
            partial.settings.font_family,
            Settings::default().font_family
        );

        assert!(Session::from_json(b"not json").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn saves_readable_by_the_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("logship-session-{}", std::process::id()));
        let path = dir.join("logship").join("session.json");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        write_atomically(&path, &Session::default()).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);

        // An existing file shared with others is replaced by a private one.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let session = Session {
            active: 3,
            ..Session::default()
        };
        write_atomically(&path, &session).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(
            Session::from_json(&std::fs::read(&path).unwrap()).unwrap(),
            session
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recent_files_are_most_recent_first_without_duplicates() {
        let mut recent = Vec::new();
        for p in ["/a", "/b", "/a", "/c"] {
            push_recent(&mut recent, Path::new(p));
        }
        assert_eq!(recent, vec![PathBuf::from("/c"), "/a".into(), "/b".into()]);

        for i in 0..30 {
            push_recent(&mut recent, Path::new(&format!("/f{i}")));
        }
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0], PathBuf::from("/f29"));
    }
}
