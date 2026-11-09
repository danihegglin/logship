//! Remote hosts over SSH.
//!
//! logship runs the system's `ssh` client, so everything in `~/.ssh/config`
//! (aliases, users, ports, jump hosts, identity files) and the SSH agent apply
//! as they do in a terminal. There is no terminal to type a password or accept
//! a new host key into, so `ssh` runs in batch mode: the host must accept a key
//! from the agent or an unencrypted key file, and must already be in
//! `known_hosts`.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

/// How long to wait for a host to answer before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The `ssh` client, looked up like the container engines are.
fn program() -> PathBuf {
    let exe = format!("ssh{}", std::env::consts::EXE_SUFFIX);
    #[cfg_attr(not(any(unix, windows)), allow(unused_mut))]
    let mut dirs: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    #[cfg(unix)]
    dirs.extend(["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"].map(PathBuf::from));
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        dirs.push(PathBuf::from(root).join("System32").join("OpenSSH"));
    }
    dirs.into_iter()
        .filter(|d| d.is_absolute())
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
        .unwrap_or_else(|| exe.into())
}

/// Checks a destination typed by the user: `host`, `user@host`, an alias from
/// `~/.ssh/config` or `ssh://user@host:port`. Returns it trimmed.
pub fn parse_destination(text: &str) -> Result<String, String> {
    let dest = text.trim();
    if dest.is_empty() {
        return Err("Enter a host".into());
    }
    if dest.starts_with('-') {
        return Err("A host can't start with \"-\"".into());
    }
    if dest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("A host can't contain spaces".into());
    }
    Ok(dest.to_string())
}

/// `ssh <dest> <remote command>`. Each word of `remote` is quoted for the
/// remote shell, which joins and re-splits them.
pub fn command(dest: &str, remote: &[&str]) -> Command {
    let mut cmd = Command::new(program());
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        &format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()),
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        // Logs compress very well.
        "-o",
        "Compression=yes",
        // Never forward the agent or X11 to a host we only read logs from.
        "-a",
        "-x",
        "-T",
        "--",
        dest,
    ]);
    cmd.arg(
        remote
            .iter()
            .map(|w| quote(w))
            .collect::<Vec<_>>()
            .join(" "),
    );
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Quotes a word for a POSIX shell. Words made of safe characters are left as
/// they are, so commands also work when the login shell is fish.
pub fn quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Background jobs of a non-interactive shell read from /dev/null, so stdin is
/// kept on fd 3 for the watcher. When the command ends by itself, the watcher
/// is stopped so it can't kill a process that later got the same id.
const STREAM_SCRIPT: &str = r#"exec 3<&0; "$@" 3<&- & p=$!; (read x <&3; kill $p) >/dev/null 2>&1 & r=$!; exec 3<&-; wait $p; s=$?; kill $r 2>/dev/null; exit $s"#;

/// Runs `remote` on `dest` in `sh` and stops it once logship closes the
/// connection, even when it writes nothing anymore (as `tail -F` on a quiet
/// file would). The caller keeps the process's stdin open: when ssh goes away
/// the remote `read` sees end of file and kills the command. The script holds
/// no quotes or backslashes, so it survives any login shell.
pub fn streaming_command(dest: &str, remote: &[&str]) -> Command {
    let mut words = vec!["sh", "-c", STREAM_SCRIPT, "sh"];
    words.extend_from_slice(remote);
    let mut cmd = command(dest, &words);
    cmd.stdin(Stdio::piped());
    cmd
}

/// The first meaningful line of what a failed ssh wrote on stderr, with a hint
/// for the failures batch mode causes.
pub fn explain_failure(dest: &str, stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
        .unwrap_or("ssh failed");
    let hint = if line.contains("Host key verification failed")
        || line.contains("No ED25519 host key is known")
        || line.contains("host key")
    {
        format!(" Connect once with `ssh {dest}` in a terminal to trust its host key.")
    } else if line.contains("Permission denied") && line.contains("publickey")
        || line.contains("password")
        || line.contains("passphrase")
    {
        " logship can't type a password: add your key to the SSH agent (ssh-add).".to_string()
    } else {
        String::new()
    };
    format!("{line}{hint}")
}

/// Runs a short command on `dest` and returns its stdout.
pub fn run(dest: &str, remote: &[&str]) -> Result<String, String> {
    let output = command(dest, remote)
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "ssh is not installed".to_string(),
            _ => format!("ssh: {err}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 255 is ssh's own failure; anything else is the remote command's.
        return Err(if output.status.code() == Some(255) {
            explain_failure(dest, &stderr)
        } else {
            stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("failed")
                .to_string()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    /// `path` is a directory (made absolute by the remote host).
    Dir { path: String, entries: Vec<Entry> },
    /// `path` is a readable file.
    File { path: String },
}

/// Prints `D <dir>` and the directory's entries, or `F <file>`. A leading `~`
/// is the remote user's home. No quotes or backslashes, see [`streaming_command`].
const LIST_SCRIPT: &str = r#"p=$1; case $p in "~"*) p=$HOME${p#?};; esac; if [ -d "$p" ]; then cd -- "$p" || exit 1; echo "D $(pwd)"; LC_ALL=C exec ls -1Ap; elif [ ! -e "$p" ]; then echo "$p: No such file or directory" >&2; exit 1; elif [ ! -r "$p" ]; then echo "$p: Permission denied" >&2; exit 1; else echo "F $p"; fi"#;

/// Lists a directory on `dest`, or tells that `path` is a file.
pub fn list(dest: &str, path: &str) -> Result<Listing, String> {
    let path = if path.trim().is_empty() { "~" } else { path };
    let out = run(dest, &["sh", "-c", LIST_SCRIPT, "sh", path])?;
    parse_listing(&out).ok_or_else(|| "Unexpected answer from the host".to_string())
}

fn parse_listing(out: &str) -> Option<Listing> {
    let mut lines = out.lines();
    let head = lines.next()?;
    if let Some(path) = head.strip_prefix("F ") {
        return Some(Listing::File {
            path: path.to_string(),
        });
    }
    let path = head.strip_prefix("D ")?.to_string();
    let mut entries: Vec<Entry> = lines
        .filter(|l| !l.is_empty())
        .map(|l| match l.strip_suffix('/') {
            Some(name) => Entry {
                name: name.to_string(),
                dir: true,
            },
            None => Entry {
                name: l.to_string(),
                dir: false,
            },
        })
        .collect();
    // Directories first, then by name; hidden entries last.
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.starts_with('.').cmp(&b.name.starts_with('.')))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Some(Listing::Dir { path, entries })
}

/// Joins a remote directory and an entry name, POSIX style.
pub fn join(dir: &str, name: &str) -> String {
    if name == ".." {
        return parent(dir);
    }
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The parent of a remote directory; `/` is its own parent.
pub fn parent(dir: &str) -> String {
    let trimmed = dir.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// Host aliases from `~/.ssh/config` and the files it includes, without
/// patterns like `*.example.com`.
pub fn config_hosts() -> Vec<String> {
    let Some(ssh_dir) = home_dir().map(|h| h.join(".ssh")) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    collect_hosts(&ssh_dir.join("config"), &ssh_dir, 0, &mut hosts);
    hosts
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn collect_hosts(file: &Path, ssh_dir: &Path, depth: usize, hosts: &mut Vec<String>) {
    if depth > 4 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    for (keyword, args) in text.lines().filter_map(config_line) {
        if keyword.eq_ignore_ascii_case("host") {
            for host in args {
                let pattern = host.contains(['*', '?', '!']);
                if !pattern && !hosts.iter().any(|h| h == host) {
                    hosts.push(host.to_string());
                }
            }
        } else if keyword.eq_ignore_ascii_case("include") {
            for arg in args {
                for path in include_paths(arg, ssh_dir) {
                    collect_hosts(&path, ssh_dir, depth + 1, hosts);
                }
            }
        }
    }
}

/// Splits `Keyword arg arg` or `Keyword=arg`, dropping comments and quotes.
fn config_line(line: &str) -> Option<(&str, Vec<&str>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (keyword, rest) = line.split_at(split);
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    let args = rest
        .split_whitespace()
        .take_while(|a| !a.starts_with('#'))
        .map(|a| a.trim_matches('"'))
        .filter(|a| !a.is_empty())
        .collect();
    Some((keyword, args))
}

/// Files an `Include` names. Relative paths are in `~/.ssh`; a `*` is
/// supported in the last component, which covers `Include config.d/*`.
fn include_paths(arg: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = match arg.strip_prefix("~/") {
        Some(rest) => match home_dir() {
            Some(home) => home.join(rest),
            None => return Vec::new(),
        },
        None => ssh_dir.join(arg),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains('*') {
        return vec![path];
    }
    let (prefix, suffix) = name.split_once('*').unwrap_or((&name, ""));
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            p.is_file()
                && n.len() >= prefix.len() + suffix.len()
                && n.starts_with(prefix)
                && n.ends_with(suffix)
        })
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_the_remote_shell() {
        assert_eq!(quote("/var/log/syslog"), "/var/log/syslog");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("x;y"), "'x;y'");
    }

    #[cfg(unix)]
    #[test]
    fn quoted_words_come_back_unchanged() {
        let words = [
            "plain",
            "with space",
            "it's",
            "$HOME",
            "a\"b",
            "`id`",
            "*",
            "",
        ];
        let script = format!(
            "printf '%s\\n' {}",
            words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
        );
        let out = Command::new("sh").args(["-c", &script]).output().unwrap();
        let printed = String::from_utf8(out.stdout).unwrap();
        assert_eq!(printed.lines().collect::<Vec<_>>(), words);
    }

    #[test]
    fn scripts_need_no_quoting() {
        for script in [LIST_SCRIPT, STREAM_SCRIPT] {
            assert!(!script.contains(['\'', '\\']));
        }
    }

    #[test]
    fn rejects_option_like_hosts() {
        assert!(parse_destination("-oProxyCommand=evil").is_err());
        assert!(parse_destination("a b").is_err());
        assert!(parse_destination("  ").is_err());
        assert_eq!(parse_destination(" me@web1 ").unwrap(), "me@web1");
        assert_eq!(
            parse_destination("ssh://me@web1:2222").unwrap(),
            "ssh://me@web1:2222"
        );
    }

    #[test]
    fn destination_follows_the_options() {
        let cmd = command("web1", &["tail", "-F", "/var/log/a b"]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        let dd = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dd + 1], "web1");
        assert_eq!(args[dd + 2], "tail -F '/var/log/a b'");
        assert!(args.contains(&"BatchMode=yes".into()));
    }

    #[test]
    fn parses_listings() {
        let out = "D /var/log\nsyslog\nnginx/\n.hidden\nauth.log\napt/\n";
        let Some(Listing::Dir { path, entries }) = parse_listing(out) else {
            panic!()
        };
        assert_eq!(path, "/var/log");
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["apt", "nginx", "auth.log", "syslog", ".hidden"]);
        assert!(entries[0].dir && !entries[2].dir);
        assert_eq!(
            parse_listing("F /home/me/app.log\n"),
            Some(Listing::File {
                path: "/home/me/app.log".into()
            })
        );
        assert_eq!(parse_listing("garbage"), None);
    }

    /// Runs the listing script locally, the way the remote shell would.
    #[cfg(unix)]
    #[test]
    fn list_script_works_in_sh() {
        let dir = std::env::temp_dir().join(format!("logship-ssh-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.log"), "x").unwrap();
        let run = |p: &str| {
            let out = Command::new("sh")
                .args(["-c", LIST_SCRIPT, "sh", p])
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let (ok, out) = run(dir.to_str().unwrap());
        assert!(ok);
        let Some(Listing::Dir { entries, .. }) = parse_listing(&out) else {
            panic!("{out}")
        };
        assert_eq!(
            entries,
            [
                Entry {
                    name: "sub".into(),
                    dir: true
                },
                Entry {
                    name: "a.log".into(),
                    dir: false
                }
            ]
        );
        let file = dir.join("a.log");
        let (ok, out) = run(file.to_str().unwrap());
        assert!(ok);
        assert_eq!(out.trim(), format!("F {}", file.display()));
        let (ok, _) = run(dir.join("missing").to_str().unwrap());
        assert!(!ok);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn streaming_script_stops_when_stdin_closes() {
        let spawn = |args: &[&str]| {
            Command::new("sh")
                .args(["-c", STREAM_SCRIPT, "sh"])
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut child = spawn(&["sleep", "30"]);
        std::thread::sleep(Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none(), "stopped too early");
        let start = std::time::Instant::now();
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(start.elapsed() < Duration::from_secs(10));

        // A command that ends by itself ends the script, stdin still open.
        let mut child = spawn(&["sh", "-c", "exit 3"]);
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "did not end");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join("/var/log", "syslog"), "/var/log/syslog");
        assert_eq!(join("/", "var"), "/var");
        assert_eq!(join("/var/log", ".."), "/var");
        assert_eq!(parent("/var"), "/");
        assert_eq!(parent("/"), "/");
    }

    #[test]
    fn reads_config_lines() {
        assert_eq!(
            config_line("Host web1 web2 *.prod # comment"),
            Some(("Host", vec!["web1", "web2", "*.prod"]))
        );
        assert_eq!(config_line("Host=db"), Some(("Host", vec!["db"])));
        assert_eq!(config_line("  # nothing"), None);
        assert_eq!(
            config_line("Include \"config.d/*\""),
            Some(("Include", vec!["config.d/*"]))
        );
    }

    #[test]
    fn collects_hosts_and_includes() {
        let dir = std::env::temp_dir().join(format!("logship-sshcfg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("config.d")).unwrap();
        std::fs::write(
            dir.join("config"),
            "Host web1 *.internal\n  User me\nInclude config.d/*\nHost db\n",
        )
        .unwrap();
        std::fs::write(dir.join("config.d").join("work"), "Host bastion !nope\n").unwrap();
        let mut hosts = Vec::new();
        collect_hosts(&dir.join("config"), &dir, 0, &mut hosts);
        assert_eq!(hosts, ["web1", "bastion", "db"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
