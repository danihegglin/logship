//! Docker and Podman containers, on this computer or on a remote host over
//! SSH.
//!
//! Containers are listed with `<engine> ps -a`, and a container's logs are
//! streamed with `<engine> logs --follow` (see [`crate::stream`]). On a remote
//! host the same commands run through `ssh`, so only the engine's command line
//! tool needs to be installed there.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::ssh;

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

    /// `<engine> <args>`, here or on `host`.
    fn command(self, host: Option<&str>, args: &[&str]) -> Command {
        if let Some(host) = host {
            let mut words = vec![self.name()];
            words.extend_from_slice(args);
            return ssh::command(host, &words);
        }
        let mut cmd = Command::new(self.program());
        cmd.args(args);
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
    /// The SSH destination the container runs on; `None` for this computer.
    pub host: Option<String>,
    pub id: String,
    pub name: String,
    pub image: String,
    /// Human readable, e.g. "Up 5 minutes" or "Exited (0) 2 hours ago".
    pub status: String,
    pub running: bool,
}

impl Container {
    /// Tab title, e.g. "docker: web" or "web1 docker: web".
    pub fn label(&self) -> String {
        match &self.host {
            Some(host) => format!("{host} {}: {}", self.engine.name(), self.name),
            None => format!("{}: {}", self.engine.name(), self.name),
        }
    }

    /// `<engine> logs --follow <id>`. On a remote host it is stopped when
    /// logship closes the connection.
    pub fn logs_command(&self) -> Command {
        match &self.host {
            Some(host) => ssh::streaming_command(
                host,
                &[self.engine.name(), "logs", "--follow", "--", &self.id],
            ),
            None => self
                .engine
                .command(None, &["logs", "--follow", "--", &self.id]),
        }
    }
}

const PS_FORMAT: &str = "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Status}}";

/// Lists the engine's containers, running and stopped, here or on `host`.
/// Fails when the tool is missing or its daemon / machine can't be reached.
pub fn list(engine: Engine, host: Option<&str>) -> Result<Vec<Container>, String> {
    let output = engine
        .command(host, &["ps", "-a", "--format", PS_FORMAT])
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound if host.is_some() => "ssh is not installed".into(),
            std::io::ErrorKind::NotFound => format!("{} is not installed", engine.name()),
            _ => format!("{}: {err}", engine.name()),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 255 is ssh failing to connect, not the engine failing.
        if let (Some(host), Some(255)) = (host, output.status.code()) {
            return Err(ssh::explain_failure(host, &stderr));
        }
        // A shell on the host that can't find the tool says so with 127.
        if output.status.code() == Some(127) {
            return Err(format!("{} is not installed", engine.name()));
        }
        let reason = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed");
        return Err(format!("{}: {}", engine.name(), reason.trim()));
    }
    Ok(parse_ps(
        engine,
        host,
        &String::from_utf8_lossy(&output.stdout),
    ))
}

fn parse_ps(engine: Engine, host: Option<&str>, stdout: &str) -> Vec<Container> {
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
                host: host.map(str::to_string),
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
/// engines that could not list anything. The engines are asked in parallel,
/// which matters when each one is a round trip to a remote host.
pub fn list_all(host: Option<&str>) -> (Vec<Container>, Vec<String>) {
    let mut containers: Vec<Container> = Vec::new();
    let mut errors = Vec::new();
    let results: Vec<_> = std::thread::scope(|s| {
        let jobs: Vec<_> = Engine::ALL
            .map(|engine| s.spawn(move || list(engine, host)))
            .into_iter()
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap_or_else(|_| Err("failed".into())))
            .collect()
    });
    // Both fail the same way when the host can't be reached; say it once.
    if host.is_some() && results.iter().all(|r| r.is_err()) {
        let first = results[0].clone().unwrap_err();
        if results.iter().all(|r| r.as_ref().unwrap_err() == &first) {
            return (Vec::new(), vec![first]);
        }
    }
    for result in results {
        match result {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_ps() {
        let out = "a1b2c3d4e5f6\tweb,proxy/web\tnginx:1.27\trunning\tUp 5 minutes (healthy)\n\
                   0f9e8d7c6b5a\tjob\talpine\texited\tExited (0) 2 hours ago\n\n";
        let list = parse_ps(Engine::Docker, None, out);
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
        let list = parse_ps(Engine::Podman, Some("web1"), out);
        assert!(list[0].running);
        assert!(!list[1].running);
        // A container without a name is shown by its id.
        assert_eq!(list[1].name, "def");
        assert_eq!(list[0].host.as_deref(), Some("web1"));
        assert_eq!(list[0].label(), "web1 podman: db");
    }

    #[test]
    fn remote_logs_run_over_ssh() {
        let mut c = parse_ps(
            Engine::Docker,
            Some("me@web1"),
            "abc\tweb\tnginx\trunning\tUp\n",
        )
        .remove(0);
        let cmd = c.logs_command();
        assert!(cmd.get_program().to_string_lossy().contains("ssh"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let last = args.last().unwrap();
        assert!(last.ends_with("docker logs --follow -- abc"), "{last}");
        c.host = None;
        let cmd = c.logs_command();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["logs", "--follow", "--", "abc"]);
    }
}
