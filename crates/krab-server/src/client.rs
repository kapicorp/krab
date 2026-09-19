//! Talking to a running server, and starting one when there is none.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::paths;
use crate::protocol::*;

#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Rpc(RpcError),
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "server connection failed: {e}"),
            ClientError::Rpc(e) => write!(f, "{}", e.message),
            ClientError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pub socket: PathBuf,
}

impl Client {
    pub fn connect(socket: &Path) -> std::io::Result<Client> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(150)))?;
        Ok(Client {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 1,
            socket: socket.to_path_buf(),
        })
    }

    pub fn call<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<R, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params: serde_json::to_value(params).unwrap(),
        };
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line)?;
        self.writer.flush()?;
        let mut buf = String::new();
        if self.reader.read_line(&mut buf)? == 0 {
            return Err(ClientError::Protocol("server closed the connection".into()));
        }
        let resp: Response =
            serde_json::from_str(&buf).map_err(|e| ClientError::Protocol(e.to_string()))?;
        if let Some(err) = resp.error {
            return Err(ClientError::Rpc(err));
        }
        let result = resp
            .result
            .ok_or_else(|| ClientError::Protocol("response without result".into()))?;
        serde_json::from_value(result)
            .map_err(|e| ClientError::Protocol(format!("unexpected result: {e}")))
    }

    pub fn info(&mut self) -> Result<InfoResult, ClientError> {
        self.call("server.info", serde_json::Value::Null)
    }

    pub fn shutdown(&mut self) -> Result<(), ClientError> {
        let _: bool = self.call("server.shutdown", serde_json::Value::Null)?;
        Ok(())
    }
}

/// How the CLI reaches a server.
#[derive(Clone)]
pub struct Connector {
    pub inventory_root: PathBuf,
    /// The binary to launch as a server (normally `current_exe`).
    pub exe: PathBuf,
    pub version: String,
    pub idle_timeout: Duration,
}

impl Connector {
    pub fn socket(&self) -> PathBuf {
        paths::socket_path(&self.inventory_root, &self.version)
    }

    /// Connect to a compatible running server. `None` when there is none.
    pub fn connect_existing(&self) -> Option<Client> {
        let socket = self.socket();
        let mut client = Client::connect(&socket).ok()?;
        match client.info() {
            Ok(info) if info.version == self.version && info.protocol == PROTOCOL_VERSION => {
                Some(client)
            }
            Ok(info) => {
                tracing::info!(
                    "server version {} != {}, restarting it",
                    info.version,
                    self.version
                );
                let _ = client.shutdown();
                wait_until(
                    || !crate::rpc::socket_alive(&socket),
                    Duration::from_secs(3),
                );
                None
            }
            Err(_) => None,
        }
    }

    /// Connect, starting a detached server first when needed.
    pub fn connect_or_spawn(&self) -> Result<Client, ClientError> {
        if let Some(c) = self.connect_existing() {
            return Ok(c);
        }
        self.spawn()?;
        let socket = self.socket();
        if !wait_until(
            || crate::rpc::socket_alive(&socket),
            Duration::from_secs(10),
        ) {
            let log = paths::log_path(&self.inventory_root);
            return Err(ClientError::Protocol(format!(
                "server did not start; see {}{}",
                log.display(),
                log_tail(&log, 5)
            )));
        }
        let mut client = Client::connect(&socket)?;
        // Whatever bound the socket answers here; make sure it is this build
        // (another one racing to the same socket used to win silently).
        let info = client.info()?;
        if info.version != self.version || info.protocol != PROTOCOL_VERSION {
            return Err(ClientError::Protocol(format!(
                "socket {} is served by kapitan {} (pid {}, {}), not by this build ({})",
                socket.display(),
                info.version,
                info.pid,
                info.exe.display(),
                self.version
            )));
        }
        Ok(client)
    }

    /// Start a detached server process (its own session, stdio to the log file).
    pub fn spawn(&self) -> std::io::Result<()> {
        let log = paths::log_path(&self.inventory_root);
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        let err_file = log_file.try_clone()?;
        let mut cmd = Command::new(&self.exe);
        cmd.arg("server")
            .arg("run")
            .arg("--inventory-path")
            .arg(&self.inventory_root)
            .arg("--idle-timeout")
            .arg(self.idle_timeout.as_secs().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(err_file));
        // Detach from the terminal's session so the server outlives the CLI.
        // SAFETY: pre_exec runs the closure in the child between fork and exec,
        // where only async-signal-safe calls are allowed. setsid() is one.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()?;
        Ok(())
    }
}

/// The last `n` lines of the server log, each on its own indented line.
fn log_tail(log: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .skip(lines.len().saturating_sub(n))
        .map(|l| format!("\n  {l}"))
        .collect()
}

fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}
