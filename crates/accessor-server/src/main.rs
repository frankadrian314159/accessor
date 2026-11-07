//! `accessor-server`: serves one Accessor database to PostgreSQL clients.

use std::process::ExitCode;

use accessor_core::dialect::Dialect;
use accessor_core::{Database, SharedDatabase};
use accessor_server::{AuthMethod, Config, Server};

const USAGE: &str = "usage: accessor-server [FILE.adb] [--listen ADDR:PORT] [--asql|--postgresql|--mysql|--tsql|--dialect NAME]
                       [--auth trust|password|md5|scram] [--user NAME:PASSWORD]... [--users-file FILE]
                       [--max-connections N]

Serves the database in FILE.adb (or an in-memory one) to PostgreSQL clients: psql, JDBC, psycopg and the rest.
Defaults: --listen 127.0.0.1:5432, --postgresql, --auth trust (or scram when users are given).
The dialect is the one each connection starts in; a client may choose its own with the startup parameter dialect
(\"options=-c dialect=mysql\" in a connection string) or later with SET dialect = 'tsql'.
A users file has one NAME:PASSWORD per line. Stop the server with Ctrl+C; the database is checkpointed on the way out.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("accessor-server: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let (mut listen, mut dialect, mut auth, mut file) = ("127.0.0.1:5432".to_string(), Dialect::Postgresql, None, None);
    let mut config = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value\n{USAGE}"));
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--listen" => listen = value("--listen")?,
            "--asql" => dialect = Dialect::Asql,
            "--postgresql" => dialect = Dialect::Postgresql,
            "--mysql" => dialect = Dialect::Mysql,
            "--tsql" => dialect = Dialect::Tsql,
            "--dialect" => {
                let v = value("--dialect")?;
                dialect = Dialect::parse(&v).ok_or_else(|| format!("unknown dialect '{v}' (asql, postgresql, mysql or tsql)"))?;
            }
            "--auth" => {
                let v = value("--auth")?;
                auth = Some(AuthMethod::parse(&v).ok_or_else(|| format!("unknown authentication method '{v}'"))?);
            }
            "--user" => {
                let v = value("--user")?;
                let (u, p) = v.split_once(':').ok_or("--user takes NAME:PASSWORD")?;
                config.users.push((u.to_string(), p.to_string()));
            }
            "--users-file" => {
                let path = value("--users-file")?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                    let (u, p) = line.split_once(':').ok_or_else(|| format!("{path}: each line is NAME:PASSWORD"))?;
                    config.users.push((u.to_string(), p.to_string()));
                }
            }
            "--max-connections" => config.max_connections = value("--max-connections")?.parse().map_err(|_| "--max-connections takes a number")?,
            other if other.starts_with('-') => return Err(format!("unknown option '{other}'\n{USAGE}")),
            other => file = Some(other.to_string()),
        }
    }
    config.auth = auth.unwrap_or(if config.users.is_empty() { AuthMethod::Trust } else { AuthMethod::Scram });

    let mut db = match &file {
        Some(p) => Database::open(p).map_err(|e| format!("cannot open {p}: {e}"))?,
        None => Database::in_memory(),
    };
    db.set_dialect(dialect);
    let shared = SharedDatabase::new(db);
    let server = Server::bind(&listen, shared.clone(), config.clone()).map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    let stop = server.stop_handle();
    ctrlc::set_handler(move || stop.stop()).map_err(|e| format!("cannot install the Ctrl+C handler: {e}"))?;
    eprintln!(
        "accessor-server: {} on {} ({:?} mode, {:?} authentication, up to {} connections)",
        file.as_deref().unwrap_or("in-memory database"),
        server.local_addr().map_err(|e| e.to_string())?,
        dialect,
        config.auth,
        config.max_connections
    );
    server.run().map_err(|e| e.to_string())?;
    if file.is_some() {
        shared.connect().checkpoint().map_err(|e| format!("checkpoint failed: {e}"))?;
    }
    eprintln!("accessor-server: stopped");
    Ok(())
}
