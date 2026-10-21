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

fn write_atomically(path: &Path, session: &Session) -> std::io::Result<()> {
    let dir = path.parent().expect("session path has a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
    std::fs::rename(&tmp, path)
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
