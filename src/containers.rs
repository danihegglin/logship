//! Logs of Docker and Podman containers.
//!
//! Containers are listed with `<engine> ps -a`, and a container's logs are
//! streamed with `<engine> logs --follow` into a private temporary file. The tab
//! then opens that file like any other log, so indexing, search and live tail
//! work unchanged. Dropping the [`LogStream`] stops the process and deletes the
//! file.

use std::{
    ffi::OsString,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    Docker,
    Podman,
}

impl Engine {
    pub const ALL: [Engine; 2] = [Engine::Docker, Engine::Podman];

    pub fn name(self) -> &'static str {
        match self {
            Engine::Docker => "docker",
            Engine::Podman => "podman",
        }
    }

    /// The engine's command line tool. Apps started from the macOS Finder or a
    /// desktop launcher get a short `PATH`, so the usual install locations are
    /// searched too.
    fn program(self) -> PathBuf {
        let exe = format!("{}{}", self.name(), std::env::consts::EXE_SUFFIX);
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut dirs: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        #[cfg(unix)]
        {
            dirs.extend(
                [
                    "/usr/local/bin",
                    "/opt/homebrew/bin",
                    "/opt/podman/bin",
                    "/opt/local/bin",
                    "/Applications/Docker.app/Contents/Resources/bin",
                ]
                .map(PathBuf::from),
            );
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                dirs.extend([".docker/bin", ".rd/bin", ".local/bin"].map(|d| home.join(d)));
            }
        }
        dirs.into_iter()
            .filter(|d| d.is_absolute())
            .map(|d| d.join(&exe))
            .find(|p| p.is_file())
            .unwrap_or_else(|| exe.into())
    }

    fn command(self) -> Command {
        let mut cmd = Command::new(self.program());
        cmd.stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub engine: Engine,
    pub id: String,
    pub name: String,
    pub image: String,
    /// Human readable, e.g. "Up 5 minutes" or "Exited (0) 2 hours ago".
    pub status: String,
    pub running: bool,
}

impl Container {
    /// Tab title, e.g. "docker: web".
    pub fn label(&self) -> String {
        format!("{}: {}", self.engine.name(), self.name)
    }
}

const PS_FORMAT: &str = "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Status}}";

/// Lists the engine's containers, running and stopped. Fails when the tool is
/// missing or its daemon / machine can't be reached.
pub fn list(engine: Engine) -> Result<Vec<Container>, String> {
    let output = engine
        .command()
        .args(["ps", "-a", "--format", PS_FORMAT])
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => format!("{} is not installed", engine.name()),
            _ => format!("{}: {err}", engine.name()),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed");
        return Err(format!("{}: {}", engine.name(), reason.trim()));
    }
    Ok(parse_ps(engine, &String::from_utf8_lossy(&output.stdout)))
}

fn parse_ps(engine: Engine, stdout: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.trim();
            // Docker lists every name of a linked container, comma separated.
            let name = fields.next()?.split(',').next()?.trim();
            let image = fields.next().unwrap_or("").trim();
            // Docker's State is a word like "running"; Podman's reads "Up 5 minutes".
            let state = fields.next().unwrap_or("").trim();
            let status = fields.next().unwrap_or(state).trim();
            if id.is_empty() {
                return None;
            }
            Some(Container {
                engine,
                id: id.to_string(),
                name: if name.is_empty() { id } else { name }.to_string(),
                image: image.to_string(),
                status: status.to_string(),
                running: state.eq_ignore_ascii_case("running") || state.starts_with("Up "),
            })
        })
        .collect()
}

/// Every engine's containers, running ones first. A container listed by two
/// engines (Podman's `docker` alias) is shown once. Errors are returned only for
/// engines that could not list anything.
pub fn list_all() -> (Vec<Container>, Vec<String>) {
    let mut containers: Vec<Container> = Vec::new();
    let mut errors = Vec::new();
    for engine in Engine::ALL {
        match list(engine) {
            Ok(found) => {
                for c in found {
                    if !containers.iter().any(|o| o.id == c.id) {
                        containers.push(c);
                    }
                }
            }
            Err(err) => errors.push(err),
        }
    }
    containers.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    (containers, errors)
}

/// A container's logs being copied into a temporary file.
pub struct LogStream {
    pub container: Container,
    path: PathBuf,
    child: Child,
}

impl LogStream {
    /// Starts `<engine> logs --follow`. Stdout and stderr are copied line by line
    /// into the same file, in the order they arrive. Logs of a stopped container
    /// are copied once and the process exits.
    pub fn start(container: &Container) -> std::io::Result<LogStream> {
        let path = new_log_file(&container.name)?;
        let file = Arc::new(Mutex::new(create_private(&path)?));
        let spawned = container
            .engine
            .command()
            .args(["logs", "--follow", &container.id])
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
            container: container.clone(),
            path,
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
    fn parses_docker_ps() {
        let out = "a1b2c3d4e5f6\tweb,proxy/web\tnginx:1.27\trunning\tUp 5 minutes (healthy)\n\
                   0f9e8d7c6b5a\tjob\talpine\texited\tExited (0) 2 hours ago\n\n";
        let list = parse_ps(Engine::Docker, out);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "a1b2c3d4e5f6");
        assert_eq!(list[0].name, "web");
        assert_eq!(list[0].image, "nginx:1.27");
        assert_eq!(list[0].status, "Up 5 minutes (healthy)");
        assert!(list[0].running);
        assert!(!list[1].running);
        assert_eq!(list[1].label(), "docker: job");
    }

    #[test]
    fn parses_podman_ps() {
        let out = "abc\tdb\tdocker.io/library/postgres:16\tUp 3 hours\tUp 3 hours\n\
                   def\t\tbusybox\tExited (1) 5 seconds ago\tExited (1) 5 seconds ago\n";
        let list = parse_ps(Engine::Podman, out);
        assert!(list[0].running);
        assert!(!list[1].running);
        // A container without a name is shown by its id.
        assert_eq!(list[1].name, "def");
    }

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
        let container = Container {
            engine: Engine::Docker,
            id: "x".into(),
            name: "sh".into(),
            image: String::new(),
            status: String::new(),
            running: false,
        };
        let stream = LogStream {
            container,
            path: path.clone(),
            child,
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
