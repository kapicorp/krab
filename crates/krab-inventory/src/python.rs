//! Talking to Python: choosing an interpreter, materialising an embedded
//! worker script, and a worker process speaking newline-delimited JSON.
//!
//! Used by the Python resolvers ([`crate::resolvers::python`]) and by the
//! compile crate's kadet and kapitan runners.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
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
    /// override), else `explicit` (from the shared `.kapitan`); the Python
    /// of `$VIRTUAL_ENV`, of `$CONDA_PREFIX`, and of a `.venv` in the
    /// working directory or a parent up to the repository root; a kapitan
    /// PEX on `$PATH` (run as an interpreter); then `python3`.
    pub fn candidates(explicit: Option<&str>) -> Vec<PythonCmd> {
        let project = std::env::current_dir().unwrap_or_default();
        Self::candidates_in(explicit, &project, &|k| std::env::var_os(k))
    }

    /// [`PythonCmd::candidates`] for the project in `project`, reading the
    /// environment through `var`.
    fn candidates_in(
        explicit: Option<&str>,
        project: &Path,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Vec<PythonCmd> {
        let mut candidates = Vec::new();
        if let Some(c) = PythonCmd::explicit_in(explicit, var) {
            candidates.push(c);
        }
        // The nearest `.venv`, not looking past the repository root.
        let mut dot_venv = None;
        for dir in project.ancestors() {
            if dir.join(".venv/bin/python").is_file() {
                dot_venv = Some(dir.join(".venv"));
                break;
            }
            if dir.join(".git").exists() {
                break;
            }
        }
        let venvs = ["VIRTUAL_ENV", "CONDA_PREFIX"]
            .into_iter()
            .filter_map(|k| var(k).filter(|v| !v.is_empty()).map(PathBuf::from))
            .chain(dot_venv);
        for venv in venvs {
            let program = venv.join("bin/python");
            if program.is_file() && !candidates.iter().any(|c| c.program == program) {
                candidates.push(PythonCmd {
                    description: program.display().to_string(),
                    program,
                    args: vec![],
                    env: vec![],
                });
            }
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
        Self::explicit_in(explicit, &|k| std::env::var_os(k))
    }

    fn explicit_in(
        explicit: Option<&str>,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Option<PythonCmd> {
        let spec = var("KRAB_PYTHON")
            .and_then(|s| s.into_string().ok())
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

#[derive(Debug)]
pub enum WorkerError {
    Io(std::io::Error),
    Protocol(String),
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
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pub info: Value,
}

impl Worker {
    pub fn spawn(python: &PythonCmd, script: &Path, init: Value) -> Result<Worker, WorkerError> {
        let mut cmd: Command = python.command();
        cmd.arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env("PYTHONUNBUFFERED", "1");
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut w = Worker {
            child,
            stdin,
            stdout,
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
    /// and the reply goes back on its stdin.
    pub fn call_with(
        &mut self,
        mut req: Value,
        mut on_request: impl FnMut(&Value) -> Value,
    ) -> Result<Value, WorkerError> {
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = json!(id);
        self.send(&req)?;
        loop {
            let mut buf = String::new();
            if self.stdout.read_line(&mut buf)? == 0 {
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

    /// Creates `<root>/<rel>/bin/python` and returns `<root>/<rel>`.
    fn venv(root: &Path, rel: &str) -> PathBuf {
        let dir = root.join(rel);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/python"), "").unwrap();
        dir
    }

    fn order(explicit: Option<&str>, project: &Path, vars: &[(&str, &Path)]) -> Vec<String> {
        let var = |k: &str| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.as_os_str().to_owned())
        };
        PythonCmd::candidates_in(explicit, project, &var)
            .into_iter()
            .map(|c| c.description)
            .filter(|d| !d.starts_with("PEX_INTERPRETER="))
            .collect()
    }

    #[test]
    fn candidates_follow_the_explicit_setting_then_the_environments() {
        let root = std::env::temp_dir().join(format!("krab-candidates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let active = venv(&root, "active");
        let conda = venv(&root, "conda");
        let dot_venv = venv(&root, "repo/.venv");
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        let project = root.join("repo/sub/dir");
        std::fs::create_dir_all(&project).unwrap();
        let python = |d: &Path| d.join("bin/python").display().to_string();

        assert_eq!(
            order(
                Some("/opt/py"),
                &project,
                &[("VIRTUAL_ENV", &active), ("CONDA_PREFIX", &conda)]
            ),
            vec![
                "/opt/py".to_string(),
                python(&active),
                python(&conda),
                python(&dot_venv),
                "python3".to_string()
            ]
        );
        // `$KRAB_PYTHON` wins over the `.kapitan` setting; an environment
        // that is also the project's `.venv` is listed once.
        assert_eq!(
            order(
                Some("/opt/py"),
                &project,
                &[
                    ("KRAB_PYTHON", Path::new("/usr/bin/python3.12")),
                    ("VIRTUAL_ENV", &dot_venv)
                ]
            ),
            vec![
                "/usr/bin/python3.12".to_string(),
                python(&dot_venv),
                "python3".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_venv_above_the_repository_root_or_without_python_is_skipped() {
        let root = std::env::temp_dir().join(format!("krab-candidates-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        venv(&root, ".venv");
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        assert_eq!(
            order(
                None,
                &root.join("repo"),
                &[("VIRTUAL_ENV", &root.join("empty"))]
            ),
            vec!["python3".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
