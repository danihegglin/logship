//! Logs that are copied into a temporary file while a tab shows them: a
//! container's `logs --follow`, or a file on a remote host read with
//! `tail -F` over SSH.
//!
//! The process's stdout and stderr go line by line into a private temporary
//! file, and the tab opens that file like any other log, so indexing, search
//! and live tail work unchanged. Dropping the [`LogStream`] stops the process
//! and deletes the file.

use std::{
    ffi::OsString,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
};

use crate::{containers::Container, ssh};

/// What a stream shows.
#[derive(Clone, Debug)]
pub enum Source {
    Container(Container),
    /// A file on a remote host, by its absolute path there.
    RemoteFile {
        host: String,
        path: String,
    },
}

impl Source {
    /// Tab title, e.g. "docker: web" or "web1: syslog".
    pub fn label(&self) -> String {
        match self {
            Source::Container(c) => c.label(),
            Source::RemoteFile { host, path } => {
                let name = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
                format!("{host}: {name}")
            }
        }
    }

    /// Shown when the pointer rests on the tab.
    pub fn description(&self) -> String {
        match self {
            Source::Container(c) => match &c.host {
                Some(host) => format!(
                    "{} container {} on {host} ({})",
                    c.engine.name(),
                    c.name,
                    c.image
                ),
                None => format!("{} container {} ({})", c.engine.name(), c.name, c.image),
            },
            Source::RemoteFile { host, path } => format!("{host}:{path}"),
        }
    }

    /// True when both show the same logs, whatever their status says now.
    pub fn same(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::Container(a), Source::Container(b)) => {
                a.engine == b.engine && a.id == b.id && a.host == b.host
            }
            (
                Source::RemoteFile { host: h1, path: p1 },
                Source::RemoteFile { host: h2, path: p2 },
            ) => h1 == h2 && p1 == p2,
            _ => false,
        }
    }

    /// The remote host, if any.
    pub fn host(&self) -> Option<&str> {
        match self {
            Source::Container(c) => c.host.as_deref(),
            Source::RemoteFile { host, .. } => Some(host),
        }
    }

    fn file_stem(&self) -> String {
        match self {
            Source::Container(c) => c.name.clone(),
            // "web1: app.log" becomes "web1_app", and the stream file "web1_app.log".
            Source::RemoteFile { .. } => {
                let label = self.label();
                label
                    .strip_suffix(".log")
                    .unwrap_or(&label)
                    .replace(": ", "_")
            }
        }
    }

    fn command(&self) -> Command {
        match self {
            Source::Container(c) => c.logs_command(),
            // `-c +1` copies the whole file as bytes, then follows it; `-F`
            // reopens it when it is rotated.
            Source::RemoteFile { host, path } => {
                ssh::streaming_command(host, &["tail", "-c", "+1", "-F", "--", path])
            }
        }
    }
}

/// Logs being copied into a temporary file.
pub struct LogStream {
    pub source: Source,
    path: PathBuf,
    child: Child,
    /// Kept open for an SSH stream: the remote side stops its command when
    /// this closes (see [`ssh::streaming_command`]).
    _stdin: Option<ChildStdin>,
}

impl LogStream {
    /// Starts the source's process. Stdout and stderr are copied line by line
    /// into the same file, in the order they arrive. Logs of a stopped
    /// container are copied once and the process exits.
    pub fn start(source: &Source) -> std::io::Result<LogStream> {
        let path = new_log_file(&source.file_stem())?;
        let file = Arc::new(Mutex::new(create_private(&path)?));
        let spawned = source
            .command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                return Err(err);
            }
        };
        if let Some(out) = child.stdout.take() {
            copy_lines(out, file.clone());
        }
        if let Some(err) = child.stderr.take() {
            copy_lines(err, file);
        }
        Ok(LogStream {
            source: source.clone(),
            path,
            _stdin: child.stdin.take(),
            child,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Copies whole lines, so lines from stdout and stderr don't get mixed up.
/// The copy ends when the process exits or is killed. If logship itself goes
/// away, the pipe closes and the logs process stops on its next write.
fn copy_lines(from: impl Read + Send + 'static, to: Arc<Mutex<File>>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::with_capacity(64 << 10, from);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let Ok(mut file) = to.lock() else { break };
                    if file.write_all(&line).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// True for files written by a [`LogStream`] of this process. They are
/// temporary, so they are kept out of the recent files and the saved session.
pub fn is_stream_file(path: &Path) -> bool {
    LOG_DIR
        .get()
        .and_then(|dir| dir.as_ref().ok())
        .is_some_and(|dir| path.starts_with(dir))
}

/// Removes this process's (by then empty) log directory, at exit.
pub fn remove_log_dir() {
    if let Some(Ok(dir)) = LOG_DIR.get() {
        let _ = std::fs::remove_dir(dir);
    }
}

static LOG_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// A fresh directory, readable by the user only, for this process's streams.
fn log_dir() -> std::io::Result<&'static Path> {
    LOG_DIR
        .get_or_init(|| {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let mut last_err = String::new();
            for attempt in 0..100 {
                let dir = base.join(format!("logship-{pid}-{attempt}"));
                match create_private_dir(&dir) {
                    Ok(()) => return Ok(std::fs::canonicalize(&dir).unwrap_or(dir)),
                    Err(err) => last_err = err.to_string(),
                }
            }
            Err(last_err)
        })
        .as_deref()
        .map_err(|err| std::io::Error::other(format!("no temporary directory: {err}")))
}

/// A new, unused file name in the log directory, named after the container.
fn new_log_file(name: &str) -> std::io::Result<PathBuf> {
    let dir = log_dir()?;
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let stem = stem.trim_start_matches('.');
    let stem = if stem.is_empty() { "container" } else { stem };
    (1..)
        .map(|n| {
            let mut file = OsString::from(stem);
            if n > 1 {
                file.push(format!("-{n}"));
            }
            file.push(".log");
            dir.join(file)
        })
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("no free file name"))
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // Not recursive: fails if the directory, or anything planted in its place,
    // already exists.
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir)
}

fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_files_are_private_and_unique() {
        let a = new_log_file("../../etc/passwd").unwrap();
        assert!(is_stream_file(&a));
        assert_eq!(a.parent(), Some(log_dir().unwrap()));
        create_private(&a).unwrap();
        let b = new_log_file("../../etc/passwd").unwrap();
        assert_ne!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&a), 0o600);
            assert_eq!(mode(log_dir().unwrap()), 0o700);
        }
        std::fs::remove_file(&a).unwrap();
        assert!(!is_stream_file(Path::new("/var/log/syslog")));
    }

    /// Streams the output of a short shell command the way `logs --follow` is.
    #[cfg(unix)]
    #[test]
    fn copies_stdout_and_stderr_and_cleans_up() {
        let path = new_log_file("sh").unwrap();
        let file = Arc::new(Mutex::new(create_private(&path).unwrap()));
        let mut child = Command::new("sh")
            .args(["-c", "echo out; echo err >&2; echo more"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        copy_lines(child.stdout.take().unwrap(), file.clone());
        copy_lines(child.stderr.take().unwrap(), file);
        child.wait().unwrap();
        let source = Source::RemoteFile {
            host: "web1".into(),
            path: "/var/log/sh.log".into(),
        };
        assert_eq!(source.label(), "web1: sh.log");
        let stream = LogStream {
            source,
            path: path.clone(),
            child,
            _stdin: None,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            text = std::fs::read_to_string(&path).unwrap();
            if text.lines().count() == 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort();
        assert_eq!(lines, ["err", "more", "out"]);
        drop(stream);
        assert!(!path.exists());
    }
}

/// Against a real host: `LOGSHIP_SSH_TEST_HOST=<host> cargo test -- --ignored`.
/// The host must be this machine (or share its /tmp), reachable by ssh in
/// batch mode.
#[cfg(all(test, unix))]
mod ssh_tests {
    use super::*;
    use crate::{containers, ssh};
    use std::time::{Duration, Instant};

    fn host() -> String {
        std::env::var("LOGSHIP_SSH_TEST_HOST").expect("set LOGSHIP_SSH_TEST_HOST")
    }

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    #[ignore]
    fn follows_a_remote_file_and_stops_tail_on_drop() {
        let host = host();
        let dir = std::env::temp_dir().join(format!("logship ssh {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("it's.log");
        std::fs::write(&file, "first\n").unwrap();

        let Ok(ssh::Listing::Dir { entries, .. }) = ssh::list(&host, dir.to_str().unwrap()) else {
            panic!("listing failed")
        };
        assert_eq!(entries[0].name, "it's.log");
        let path = file.to_str().unwrap().to_string();
        assert_eq!(
            ssh::list(&host, &path),
            Ok(ssh::Listing::File { path: path.clone() })
        );
        assert!(ssh::list(&host, "/no/such/dir").is_err());

        let stream = LogStream::start(&Source::RemoteFile {
            host: host.clone(),
            path: path.clone(),
        })
        .unwrap();
        let local = stream.path().to_path_buf();
        let read = || std::fs::read_to_string(&local).unwrap_or_default();
        wait_for("first line", || read() == "first\n");
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        wait_for("appended line", || read() == "first\nsecond\n");

        let tails = || {
            Command::new("pgrep")
                .args(["-f", &format!("tail -c \\+1 -F -- {path}")])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        };
        assert!(tails(), "remote tail is running");
        drop(stream);
        assert!(!local.exists());
        wait_for("remote tail to stop", || !tails());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore]
    fn reports_unreachable_hosts_and_missing_engines() {
        let (found, errors) = containers::list_all(Some("nobody@127.0.0.1:1"));
        assert!(found.is_empty());
        assert_eq!(errors.len(), 1, "{errors:?}");
        eprintln!("unreachable: {errors:?}");
        let (found, errors) = containers::list_all(Some(&host()));
        eprintln!("containers: {found:?}\nerrors: {errors:?}");
    }

    /// Also needs `LOGSHIP_SSH_TEST_CONTAINER`, a running container on the host
    /// that logs regularly.
    #[test]
    #[ignore]
    fn streams_a_remote_container() {
        let host = host();
        let name =
            std::env::var("LOGSHIP_SSH_TEST_CONTAINER").expect("set LOGSHIP_SSH_TEST_CONTAINER");
        let (found, _) = containers::list_all(Some(&host));
        let c = found
            .into_iter()
            .find(|c| c.name == name)
            .expect("container is listed");
        assert!(c.running);
        assert_eq!(c.host.as_deref(), Some(host.as_str()));
        let stream = LogStream::start(&Source::Container(c.clone())).unwrap();
        let local = stream.path().to_path_buf();
        let lines = || {
            std::fs::read_to_string(&local)
                .unwrap_or_default()
                .lines()
                .count()
        };
        wait_for("some lines", || lines() >= 2);
        let n = lines();
        wait_for("more lines", || lines() > n);
        let follows = || {
            Command::new("pgrep")
                .args(["-f", &format!("logs --follow -- {}", c.id)])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        };
        assert!(follows());
        drop(stream);
        wait_for("remote logs to stop", || !follows());
    }
}
