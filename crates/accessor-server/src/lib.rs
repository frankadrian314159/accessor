//! A server for Accessor that speaks PostgreSQL's wire protocol (version 3).
//!
//! Any PostgreSQL client or driver (`psql`, JDBC, psycopg, node-postgres, ...) can connect. Each connection gets its own
//! thread and its own [`accessor_core::Connection`] on one [`SharedDatabase`], so transactions overlap with snapshot
//! isolation exactly as they do for threads of an embedded program. The embedded library is not changed by any of this.
//!
//! What is there: the startup handshake (SSL and GSS requests are declined), password authentication (cleartext, MD5,
//! SCRAM-SHA-256), simple queries (several statements in one message, each answered), the extended protocol (Parse, Bind,
//! Describe, Execute, Close, Sync, Flush, portals with row limits), results in text or binary format for the common
//! types, error responses with SQLSTATE codes, cancel requests (between statements), a limit on connections, and a
//! graceful stop.
//!
//! What is not: TLS, `COPY`, `LISTEN`/`NOTIFY`, and interrupting a statement that is running. Settings made with `SET`
//! belong to the database, not to one connection, because the engine keeps them there.

mod auth;
mod session;
mod sql;
mod types;
mod wire;

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use accessor_core::SharedDatabase;

pub use auth::AuthMethod;

/// How the server behaves.
#[derive(Clone, Debug)]
pub struct Config {
    pub auth: AuthMethod,
    /// `(user, password)` pairs. With `AuthMethod::Trust` any user name is accepted and these are not looked at.
    pub users: Vec<(String, String)>,
    pub max_connections: usize,
    /// What `server_version` reports to clients.
    pub server_version: String,
}

impl Default for Config {
    fn default() -> Config {
        Config { auth: AuthMethod::Trust, users: Vec::new(), max_connections: 100, server_version: "16.0 (Accessor)".into() }
    }
}

/// What every connection thread shares.
pub(crate) struct Shared {
    pub db: SharedDatabase,
    pub config: Config,
    pub stop: AtomicBool,
    pub active: AtomicUsize,
    /// `(process id, secret)` of each connection, to the flag that a cancel request sets.
    pub cancels: Mutex<HashMap<(i32, i32), Arc<AtomicBool>>>,
}

pub struct Server {
    listener: TcpListener,
    shared: Arc<Shared>,
}

/// Stops a running server from another thread.
#[derive(Clone)]
pub struct StopHandle(Arc<Shared>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.stop.store(true, Ordering::SeqCst);
    }
}

impl Server {
    pub fn bind(addr: impl ToSocketAddrs, db: SharedDatabase, config: Config) -> io::Result<Server> {
        if config.auth != AuthMethod::Trust && config.users.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "password authentication needs at least one user"));
        }
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared { db, config, stop: AtomicBool::new(false), active: AtomicUsize::new(0), cancels: Mutex::new(HashMap::new()) });
        Ok(Server { listener, shared })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(self.shared.clone())
    }

    /// Accepts connections until stopped, then waits (a few seconds) for the ones open to finish.
    pub fn run(self) -> io::Result<()> {
        static NEXT_PID: AtomicI32 = AtomicI32::new(1000);
        let mut threads = Vec::new();
        while !self.shared.stop.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    let shared = self.shared.clone();
                    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
                    shared.active.fetch_add(1, Ordering::SeqCst);
                    threads.push(thread::spawn(move || {
                        // a client that vanishes mid-message is not worth reporting
                        let _ = session::serve(stream, &shared, pid);
                        shared.active.fetch_sub(1, Ordering::SeqCst);
                    }));
                    threads.retain(|t| !t.is_finished());
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e),
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.shared.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        for t in threads {
            if t.is_finished() {
                let _ = t.join();
            }
        }
        Ok(())
    }
}
