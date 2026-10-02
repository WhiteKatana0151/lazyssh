//! Lifecycle of background port-forward ssh processes.
//!
//! Each running forward is one `ssh -N` child keyed by server name and
//! canonical spec. Forwards live exactly as long as LazySSH: dropping the
//! manager kills every child, and sharing LazySSH's process group covers
//! the signal paths where `Drop` never runs.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

/// Identifies one forward of one server.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForwardKey {
    pub server: String,
    pub spec: String,
}

impl ForwardKey {
    pub fn new(server: &str, spec: &str) -> Self {
        Self {
            server: server.to_string(),
            spec: spec.to_string(),
        }
    }
}

/// A forward that stopped on its own, with ssh's explanation if it gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exited {
    pub key: ForwardKey,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Forwards {
    running: BTreeMap<ForwardKey, Child>,
}

impl Forwards {
    /// Spawns `cmd` as the process behind `key`, detached from the terminal:
    /// no stdin, no stdout, stderr captured for the exit reason.
    pub fn start(&mut self, key: ForwardKey, mut cmd: Command) -> Result<()> {
        if self.running.contains_key(&key) {
            return Ok(());
        }
        // Deliberately left in LazySSH's process group: if the terminal is
        // closed or LazySSH is killed by a signal (so `Drop` never runs),
        // the forwards receive the same signal instead of lingering as
        // orphaned ssh processes.
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start ssh for the forward")?;
        self.running.insert(key, child);
        Ok(())
    }

    /// Stops the forward behind `key`, returning whether one was running.
    pub fn stop(&mut self, key: &ForwardKey) -> bool {
        match self.running.remove(key) {
            Some(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
                true
            }
            None => false,
        }
    }

    /// Stops every forward of `server`.
    pub fn stop_for(&mut self, server: &str) {
        let keys: Vec<_> = self
            .running
            .keys()
            .filter(|k| k.server == server)
            .cloned()
            .collect();
        for key in keys {
            self.stop(&key);
        }
    }

    pub fn stop_all(&mut self) {
        let keys: Vec<_> = self.running.keys().cloned().collect();
        for key in keys {
            self.stop(&key);
        }
    }

    pub fn is_running(&self, key: &ForwardKey) -> bool {
        self.running.contains_key(key)
    }

    /// Number of live forwards for `server`.
    pub fn count_for(&self, server: &str) -> usize {
        self.running.keys().filter(|k| k.server == server).count()
    }

    pub fn len(&self) -> usize {
        self.running.len()
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// Reaps forwards whose ssh exited — auth failure, port in use, network
    /// drop — and reports them with the last line ssh printed.
    pub fn poll(&mut self) -> Vec<Exited> {
        let mut exited = Vec::new();
        let finished: Vec<ForwardKey> = self
            .running
            .iter_mut()
            .filter_map(|(key, child)| matches!(child.try_wait(), Ok(Some(_))).then(|| key.clone()))
            .collect();
        for key in finished {
            let Some(mut child) = self.running.remove(&key) else {
                continue;
            };
            let status = child.wait().ok();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let reason = stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| match status {
                    Some(status) => format!("ssh exited with {status}"),
                    None => "ssh exited".to_string(),
                });
            exited.push(Exited { key, reason });
        }
        exited
    }
}

impl Drop for Forwards {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[test]
    fn start_stop_and_count() {
        let mut forwards = Forwards::default();
        let a = ForwardKey::new("node", "L1:h:2");
        let b = ForwardKey::new("node", "D1080");
        forwards.start(a.clone(), sh("sleep 30")).unwrap();
        forwards.start(b.clone(), sh("sleep 30")).unwrap();
        // Starting an already running forward is a no-op.
        forwards.start(a.clone(), sh("exit 1")).unwrap();
        assert!(forwards.is_running(&a));
        assert_eq!(forwards.count_for("node"), 2);
        assert_eq!(forwards.count_for("other"), 0);
        assert!(forwards.poll().is_empty());

        assert!(forwards.stop(&a));
        assert!(!forwards.stop(&a));
        assert_eq!(forwards.len(), 1);
        forwards.stop_all();
        assert!(forwards.is_empty());
    }

    #[test]
    fn poll_reports_dead_forwards_with_their_last_stderr_line() {
        let mut forwards = Forwards::default();
        let key = ForwardKey::new("node", "L8080:localhost:80");
        forwards
            .start(
                key.clone(),
                sh("echo noise >&2; echo 'bind: Address already in use' >&2; exit 255"),
            )
            .unwrap();
        let silent = ForwardKey::new("node", "D1");
        forwards.start(silent.clone(), sh("exit 3")).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = Vec::new();
        while exited.len() < 2 && Instant::now() < deadline {
            exited.extend(forwards.poll());
            std::thread::sleep(Duration::from_millis(10));
        }
        exited.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(exited.len(), 2);
        assert_eq!(exited[0].key, silent);
        assert!(
            exited[0].reason.contains("exit status: 3"),
            "{:?}",
            exited[0]
        );
        assert_eq!(exited[1].reason, "bind: Address already in use");
        assert!(forwards.is_empty());
    }

    #[test]
    fn dropping_the_manager_kills_children() {
        let mut forwards = Forwards::default();
        let key = ForwardKey::new("node", "D1080");
        forwards.start(key.clone(), sh("sleep 30")).unwrap();
        let pid = forwards.running[&key].id();
        drop(forwards);
        // The reaped pid no longer exists.
        let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
        assert!(!alive);
    }
}
