//! Talking to Python: choosing an interpreter, materialising an embedded
//! worker script, and a worker process speaking newline-delimited JSON.
//!
//! Used by the Python resolvers ([`crate::resolvers::python`]) and by the
//! compile crate's kadet and kapitan runners.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A Python interpreter invocation: program, leading arguments and
/// environment (`PEX_INTERPRETER=1 /path/to/kapitan.pex`).
#[derive(Clone, Debug)]
pub struct PythonCmd {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub description: String,
}

impl PythonCmd {
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    /// Parse a user supplied command such as `python3`, `/opt/venv/bin/python`
    /// or `PEX_INTERPRETER=1 /usr/local/bin/kapitan`.
    pub fn parse(spec: &str) -> Option<PythonCmd> {
        let mut env = Vec::new();
        let mut parts = spec.split_whitespace().peekable();
        while let Some(p) = parts.peek() {
            if let Some((k, v)) = p.split_once('=')
                && !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                env.push((k.to_string(), v.to_string()));
                parts.next();
            } else {
                break;
            }
        }
        let program = PathBuf::from(parts.next()?);
        Some(PythonCmd {
            program,
            args: parts.map(String::from).collect(),
            env,
            description: spec.to_string(),
        })
    }

    /// Interpreters to try, in order: `$KRAB_PYTHON` (the per-machine
    /// override), else `explicit` (from the shared `.kapitan`); a kapitan PEX
    /// on `$PATH` (run as an interpreter); then `python3`.
    pub fn candidates(explicit: Option<&str>) -> Vec<PythonCmd> {
        let mut candidates = Vec::new();
        if let Some(c) = PythonCmd::explicit(explicit) {
            candidates.push(c);
        }
        if let Some(pex) = find_pex_on_path() {
            candidates.push(PythonCmd {
                program: pex.clone(),
                args: vec![],
                env: vec![("PEX_INTERPRETER".into(), "1".into())],
                description: format!("PEX_INTERPRETER=1 {}", pex.display()),
            });
        }
        candidates.push(PythonCmd {
            program: "python3".into(),
            args: vec![],
            env: vec![],
            description: "python3".into(),
        });
        candidates
    }

    /// The interpreter the user named: `$KRAB_PYTHON` (the per-machine
    /// override), else `explicit` (from the shared `.kapitan` or a flag).
    pub fn explicit(explicit: Option<&str>) -> Option<PythonCmd> {
        let spec = std::env::var("KRAB_PYTHON")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| explicit.map(str::to_string))?;
        PythonCmd::parse(&spec)
    }

    /// The first candidate, without probing it.
    pub fn preferred(explicit: Option<&str>) -> PythonCmd {
        Self::candidates(explicit).remove(0)
    }

    /// The program is a file here (absolute, or found on `PATH`).
    pub fn exists(&self) -> bool {
        which(&self.program).is_some_and(|p| p.is_file())
    }

    /// Cache key: the command plus the interpreter file's size and mtime.
    pub fn cache_key(&self) -> String {
        let program = which(&self.program);
        let stamp = program
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
            .unwrap_or_default();
        format!("{}|{}", self.description, stamp)
    }
}

/// A `kapitan` on PATH that is a PEX (zip with a shebang) rather than this binary.
fn find_pex_on_path() -> Option<PathBuf> {
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    for dir in std::env::var_os("PATH")?.to_str()?.split(':') {
        let candidate = Path::new(dir).join("kapitan");
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if Some(&canonical) == me.as_ref() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        if bytes.starts_with(b"#!")
            && bytes.len() > 4
            && bytes.windows(4).take(8192).any(|w| w == b"PK\x03\x04")
        {
            return Some(candidate);
        }
    }
    None
}

/// `~/.cache/kapitan` (honouring `XDG_CACHE_HOME`).
pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("krab")
}

/// `program` as a file: as given when it has a directory, else the first
/// match on `PATH`.
pub fn which(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return Some(program.to_path_buf());
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(program))
        .find(|p| p.is_file())
}

/// Write an embedded worker script to the cache directory (keyed by its
/// digest, so every build gets its own copy) and return its path.
pub fn materialize_script(name: &str, source: &str) -> std::io::Result<PathBuf> {
    let base = cache_dir()
        .join("runner")
        .join(&script_digest(source)[..16]);
    std::fs::create_dir_all(&base)?;
    let path = base.join(name);
    if !path.exists() {
        std::fs::write(&path, source)?;
    }
    Ok(path)
}

pub fn script_digest(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

/// How long one worker request may run unless `.kapitan`
/// `compile.python-timeout` says otherwise.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub enum WorkerError {
    Io(std::io::Error),
    Protocol(String),
    /// No answer within the request deadline; the worker was killed.
    Timeout(Duration),
    /// The worker reported a failure (message, traceback).
    Failed {
        error: String,
        traceback: Option<String>,
    },
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Io(e) => write!(f, "worker I/O error: {e}"),
            WorkerError::Protocol(m) => write!(f, "worker protocol error: {m}"),
            WorkerError::Timeout(t) => write!(
                f,
                "no answer from Python within {} s (`.kapitan` compile.python-timeout); the worker was stopped",
                t.as_secs()
            ),
            WorkerError::Failed { error, .. } => write!(f, "{error}"),
        }
    }
}

impl From<std::io::Error> for WorkerError {
    fn from(e: std::io::Error) -> Self {
        WorkerError::Io(e)
    }
}

/// One Python worker process. Requests carry an `op` and an `id`; the
/// answer echoes the `id` with `ok: true` and payload, or `ok: false`,
/// `error` and `traceback`. While it works the worker may ask the host for
/// things (a line carrying `op` of its own), see [`Worker::call_with`].
pub struct Worker {
    child: Child,
    stdin: ChildStdin,
    /// The worker's stdout, line by line, from a reader thread, so a request
    /// can wait with a deadline. An empty line means end of file.
    lines: Receiver<std::io::Result<String>>,
    /// The deadline of each request, `init` included.
    timeout: Duration,
    next_id: u64,
    pub info: Value,
}

impl Worker {
    pub fn spawn(
        python: &PythonCmd,
        script: &Path,
        init: Value,
        timeout: Duration,
    ) -> Result<Worker, WorkerError> {
        let mut cmd: Command = python.command();
        cmd.arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env("PYTHONUNBUFFERED", "1");
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut buf = String::new();
                let read = stdout.read_line(&mut buf);
                let end = !matches!(read, Ok(n) if n > 0);
                if tx.send(read.map(|_| buf)).is_err() || end {
                    return;
                }
            }
        });
        let mut w = Worker {
            child,
            stdin,
            lines,
            timeout,
            next_id: 1,
            info: Value::Null,
        };
        let mut init = init;
        init["op"] = json!("init");
        w.info = w.call(init)?;
        Ok(w)
    }

    fn send(&mut self, msg: &Value) -> Result<(), WorkerError> {
        let mut line = serde_json::to_vec(msg).unwrap();
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    pub fn call(&mut self, req: Value) -> Result<Value, WorkerError> {
        self.call_with(req, |r| {
            json!({ "ok": false, "error": format!("unsupported host request {:?}", r.get("op")) })
        })
    }

    /// Send `req` and wait for its answer. While it works the worker may ask
    /// the host for things (a line carrying `op`); `on_request` answers each
    /// and the reply goes back on its stdin. Without an answer within the
    /// worker's timeout, counted from the send, the worker is killed.
    pub fn call_with(
        &mut self,
        mut req: Value,
        mut on_request: impl FnMut(&Value) -> Value,
    ) -> Result<Value, WorkerError> {
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = json!(id);
        self.send(&req)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let buf = match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => line?,
                Err(RecvTimeoutError::Timeout) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(WorkerError::Timeout(self.timeout));
                }
                Err(RecvTimeoutError::Disconnected) => String::new(),
            };
            if buf.is_empty() {
                return Err(WorkerError::Protocol("worker exited unexpectedly".into()));
            }
            let msg: Value = serde_json::from_str(&buf)
                .map_err(|e| WorkerError::Protocol(format!("{e}: {buf}")))?;
            if msg.get("op").is_some() {
                let mut reply = on_request(&msg);
                reply["id"] = msg.get("id").cloned().unwrap_or(Value::Null);
                self.send(&reply)?;
                continue;
            }
            return if msg.get("ok").and_then(Value::as_bool) == Some(true) {
                Ok(msg)
            } else {
                Err(WorkerError::Failed {
                    error: msg
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                    traceback: msg
                        .get("traceback")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            };
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A worker killed on timeout has no reader left: writing to its
        // stdin would raise SIGPIPE, which krab does not ignore.
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.stdin.write_all(b"{\"op\":\"exit\",\"id\":0}\n");
        let _ = self.stdin.flush();
        // A worker blocked mid-request never reads the exit op. Give it a
        // moment, then kill it: a server that lost the socket race used to
        // hang here forever, with its Python children.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                _ => return,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_prefix_and_args() {
        let c = PythonCmd::parse("PEX_INTERPRETER=1 /usr/local/bin/kapitan -u").unwrap();
        assert_eq!(
            c.env,
            vec![("PEX_INTERPRETER".to_string(), "1".to_string())]
        );
        assert_eq!(c.program, PathBuf::from("/usr/local/bin/kapitan"));
        assert_eq!(c.args, vec!["-u".to_string()]);
        assert!(PythonCmd::parse("   ").is_none());
    }
}
