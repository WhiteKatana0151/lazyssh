//! Background TCP reachability checks for the server list's status dots.
//!
//! This only answers "does the SSH port accept a TCP connection", not
//! "can I log in". Each probe runs on its own short-lived thread and
//! reports over a channel, so the UI thread never blocks on DNS or a
//! dead host.

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::config::Server;

/// Per-address connect timeout.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A probe is in flight.
    Probing,
    /// The port accepted a TCP connection.
    Up,
    /// Resolution failed or every address refused/timed out.
    Down,
    /// Reached through a jump host; a direct probe would be meaningless.
    ViaJump,
}

/// Results are keyed by `host:port` rather than list index, so sorting,
/// pinning, or deleting servers never mislabels a dot.
pub fn probe_key(server: &Server) -> String {
    format!("{}:{}", server.host, server.effective_port())
}

/// Whether `host:port` accepts a TCP connection within `timeout`.
pub fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
}

#[derive(Debug)]
pub struct Prober {
    tx: Sender<(String, Reach)>,
    rx: Receiver<(String, Reach)>,
}

impl Default for Prober {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx }
    }
}

impl Prober {
    /// Starts probes for `servers`, recording `Probing` (or `ViaJump`) in
    /// `state` straight away so the UI can show progress.
    pub fn spawn(&self, servers: &[Server], state: &mut HashMap<String, Reach>) {
        for server in servers {
            let key = probe_key(server);
            if server
                .jump_host
                .as_deref()
                .is_some_and(|j| !j.trim().is_empty())
            {
                state.insert(key, Reach::ViaJump);
                continue;
            }
            if state.get(&key) == Some(&Reach::Probing) {
                continue;
            }
            state.insert(key.clone(), Reach::Probing);
            let tx = self.tx.clone();
            let host = server.host.clone();
            let port = server.effective_port();
            thread::spawn(move || {
                let reach = if tcp_reachable(&host, port, PROBE_TIMEOUT) {
                    Reach::Up
                } else {
                    Reach::Down
                };
                // The receiver is gone only when the app is exiting.
                let _ = tx.send((key, reach));
            });
        }
    }

    /// Applies every finished probe to `state`, returning how many landed.
    pub fn drain(&self, state: &mut HashMap<String, Reach>) -> usize {
        let mut landed = 0;
        while let Ok((key, reach)) = self.rx.try_recv() {
            state.insert(key, reach);
            landed += 1;
        }
        landed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn server(host: &str, port: u16) -> Server {
        Server {
            name: host.into(),
            host: host.into(),
            port: Some(port),
            ..Default::default()
        }
    }

    #[test]
    fn key_uses_host_and_effective_port() {
        let mut s = server("box", 2222);
        assert_eq!(probe_key(&s), "box:2222");
        s.port = None;
        assert_eq!(probe_key(&s), "box:22");
    }

    #[test]
    fn probes_open_and_closed_ports_and_skips_jumped_servers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap().port();
        // Bind then drop to get a port that is very likely closed.
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let mut jumped = server("10.9.9.9", 22);
        jumped.jump_host = Some("bastion".into());
        let servers = [
            server("127.0.0.1", open),
            server("127.0.0.1", closed),
            jumped,
        ];

        let prober = Prober::default();
        let mut state = HashMap::new();
        prober.spawn(&servers, &mut state);
        assert_eq!(state["10.9.9.9:22"], Reach::ViaJump);
        assert_eq!(state[&format!("127.0.0.1:{open}")], Reach::Probing);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut landed = 0;
        while landed < 2 && Instant::now() < deadline {
            landed += prober.drain(&mut state);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(state[&format!("127.0.0.1:{open}")], Reach::Up);
        assert_eq!(state[&format!("127.0.0.1:{closed}")], Reach::Down);
        drop(listener);
    }

    #[test]
    fn unresolvable_hosts_are_down() {
        assert!(!tcp_reachable("no-such-host.invalid", 22, PROBE_TIMEOUT));
    }
}
