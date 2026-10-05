//! The socket server: accepts connections, dispatches JSON-RPC requests.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use krab_inventory::explain::explain;
use krab_inventory::merge::get;
use krab_inventory::{Diagnostic, KeyPath};
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

use crate::protocol::*;
use crate::state::State;

pub struct ServerConfig {
    pub socket: PathBuf,
    pub log: Option<PathBuf>,
    pub idle_timeout: Duration,
    pub version: String,
}

pub struct Server {
    pub state: Arc<State>,
    pub cfg: ServerConfig,
    pub shutdown: AtomicBool,
}

impl Server {
    /// Bind the socket, replacing a dead one; `AddrInUse` when a live server
    /// holds it. Dead sockets other builds left for this inventory go too.
    pub fn bind(&self) -> std::io::Result<UnixListener> {
        if let Some(parent) = self.cfg.socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if self.cfg.socket.exists() {
            if socket_alive(&self.cfg.socket) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "a server is already running",
                ));
            }
            std::fs::remove_file(&self.cfg.socket)?;
        }
        for dead in crate::paths::sockets(&self.state.inv.cfg.root).1 {
            let _ = std::fs::remove_file(dead);
        }
        let listener = UnixListener::bind(&self.cfg.socket)?;
        listener.set_nonblocking(true)?;
        tracing::info!(socket = %self.cfg.socket.display(), "listening");
        Ok(listener)
    }

    /// Accept connections until shutdown or idle.
    pub fn serve_on(self: Arc<Self>, listener: UnixListener) -> std::io::Result<()> {
        loop {
            if self.shutdown.load(Ordering::SeqCst) || self.state.should_stop() {
                break;
            }
            if self.state.idle_for() > self.cfg.idle_timeout {
                tracing::info!("idle for {:?}, exiting", self.cfg.idle_timeout);
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let server = self.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = server.handle(stream) {
                            tracing::debug!("connection closed: {e}");
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
        let _ = std::fs::remove_file(&self.cfg.socket);
        Ok(())
    }

    fn handle(&self, stream: UnixStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            if line.trim().is_empty() {
                continue;
            }
            self.state.touch();
            let response = match serde_json::from_str::<Request>(&line) {
                Ok(req) => {
                    // Everything but `server.*` needs the initial render.
                    if !req.method.starts_with("server.") {
                        self.state.wait_ready();
                    }
                    let id = req.id;
                    match self.dispatch(&req) {
                        Ok(result) => Response {
                            jsonrpc: "2.0".into(),
                            id,
                            result: Some(result),
                            error: None,
                        },
                        Err(e) => Response {
                            jsonrpc: "2.0".into(),
                            id,
                            result: None,
                            error: Some(e),
                        },
                    }
                }
                Err(e) => Response {
                    jsonrpc: "2.0".into(),
                    id: 0,
                    result: None,
                    error: Some(RpcError {
                        code: ERR_PARSE,
                        message: format!("invalid request: {e}"),
                        data: None,
                    }),
                },
            };
            let mut out = serde_json::to_vec(&response)?;
            out.push(b'\n');
            writer.write_all(&out)?;
            writer.flush()?;
            self.state.touch();
            if self.shutdown.load(Ordering::SeqCst) || self.state.should_stop() {
                return Ok(());
            }
        }
    }

    fn dispatch(&self, req: &Request) -> Result<Json, RpcError> {
        match req.method.as_str() {
            "server.info" => Ok(serde_json::to_value(self.info()).unwrap()),
            "server.shutdown" => {
                self.shutdown.store(true, Ordering::SeqCst);
                Ok(json!(true))
            }
            "inventory.targets" => {
                let p: TargetsParams = if req.params.is_null() {
                    TargetsParams::default()
                } else {
                    params(req)?
                };
                let inner = self.state.read();
                let targets = inner
                    .specs
                    .iter()
                    .filter(|s| {
                        p.labels.is_empty()
                            || inner
                                .targets
                                .get(&s.name)
                                .is_some_and(|t| has_labels(t, &p.labels))
                    })
                    .map(|s| TargetSummary {
                        name: s.name.clone(),
                        path: s.path.clone(),
                        file: s.file.clone(),
                        digest: inner.targets.get(&s.name).map(|t| t.digest.clone()),
                        doc_digest: inner.targets.get(&s.name).map(|t| t.doc_digest.clone()),
                        ok: inner.targets.contains_key(&s.name),
                        error: inner.errors.get(&s.name).cloned(),
                        labels: inner
                            .targets
                            .get(&s.name)
                            .map(|t| target_labels(t))
                            .unwrap_or_default(),
                        classes: inner
                            .targets
                            .get(&s.name)
                            .map(|t| t.classes.len())
                            .unwrap_or(0),
                        inputs: inner
                            .targets
                            .get(&s.name)
                            .map(|t| target_inputs(t))
                            .unwrap_or_default(),
                    })
                    .collect();
                Ok(serde_json::to_value(TargetsResult {
                    generation: inner.generation,
                    targets,
                })
                .unwrap())
            }
            "inventory.target" => {
                let p: TargetParams = params(req)?;
                let inner = self.state.read();
                let t = self.target(&inner, &p.name)?;
                let doc = t.to_document();
                let doc = match &p.path {
                    Some(path) => get(&doc, &KeyPath::parse(path)).cloned().ok_or_else(|| {
                        inventory_error(Diagnostic::error(
                            "inventory::pattern_not_found",
                            format!("nothing at `{path}` in target `{}`", p.name),
                        ))
                    })?,
                    None => doc,
                };
                Ok(serde_json::to_value(TargetResult {
                    name: t.name.clone(),
                    digest: t.digest.clone(),
                    generation: inner.generation,
                    document: doc.value.to_json(),
                    warnings: t
                        .warnings
                        .iter()
                        .map(|w| w.clone().resolve(&self.state.inv.sources))
                        .collect(),
                })
                .unwrap())
            }
            "inventory.all" => {
                let p: TargetsParams = if req.params.is_null() {
                    TargetsParams::default()
                } else {
                    params(req)?
                };
                let inner = self.state.read();
                let mut documents = serde_json::Map::new();
                for (name, t) in &inner.targets {
                    if p.labels.is_empty() || has_labels(t, &p.labels) {
                        documents.insert(name.clone(), t.to_document().value.to_json());
                    }
                }
                Ok(serde_json::to_value(AllResult {
                    generation: inner.generation,
                    documents,
                    errors: inner.errors.values().cloned().collect(),
                })
                .unwrap())
            }
            "inventory.class_usage" => {
                let inner = self.state.read();
                let report = krab_inventory::RenderReport {
                    targets: inner
                        .targets
                        .iter()
                        .map(|(k, v)| (k.clone(), (**v).clone()))
                        .collect(),
                    errors: vec![],
                };
                let usage = self
                    .state
                    .inv
                    .class_usage(&report)
                    .map_err(|e| inventory_error(e.into_diagnostic()))?;
                Ok(serde_json::to_value(usage).unwrap())
            }
            "inventory.classes" => {
                let p: TargetParams = params(req)?;
                let inner = self.state.read();
                Ok(json!(self.target(&inner, &p.name)?.classes))
            }
            "inventory.explain" => {
                let p: ExplainParams = params(req)?;
                let inner = self.state.read();
                let t = self.target(&inner, &p.target)?;
                let e = explain(&self.state.inv, t, &p.path)
                    .map_err(|e| inventory_error(e.into_diagnostic()))?;
                Ok(serde_json::to_value(e).unwrap())
            }
            "inventory.deps" => {
                let p: DepsParams = params(req)?;
                let inner = self.state.read();
                let wanted: Vec<PathBuf> = p
                    .files
                    .iter()
                    .map(|f| f.canonicalize().unwrap_or(f.clone()))
                    .collect();
                let mut names: Vec<&String> = inner
                    .targets
                    .iter()
                    .filter(|(_, t)| {
                        t.files
                            .iter()
                            .any(|f| wanted.contains(&f.canonicalize().unwrap_or(f.clone())))
                    })
                    .map(|(n, _)| n)
                    .collect();
                names.sort();
                Ok(json!(names))
            }
            "inventory.diagnostics" => {
                let inner = self.state.read();
                let warnings = self
                    .state
                    .inv
                    .registry
                    .diagnostics()
                    .iter()
                    .cloned()
                    .chain(inner.targets.values().flat_map(|t| {
                        t.warnings
                            .iter()
                            .map(|w| w.clone().resolve(&self.state.inv.sources))
                    }))
                    .collect();
                Ok(serde_json::to_value(DiagnosticsResult {
                    generation: inner.generation,
                    errors: inner.errors.values().cloned().collect(),
                    warnings,
                })
                .unwrap())
            }
            "inventory.wait" => {
                let p: WaitParams = params(req)?;
                let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(30_000).min(120_000));
                let (generation, timed_out, changes) = self.state.wait_for(p.generation, timeout);
                Ok(serde_json::to_value(WaitResult {
                    generation,
                    timed_out,
                    changes,
                })
                .unwrap())
            }
            other => Err(RpcError {
                code: ERR_METHOD,
                message: format!("unknown method `{other}`"),
                data: None,
            }),
        }
    }

    fn target<'i>(
        &self,
        inner: &'i crate::state::Inner,
        name: &str,
    ) -> Result<&'i Arc<krab_inventory::RenderedTarget>, RpcError> {
        if let Some(t) = inner.targets.get(name) {
            return Ok(t);
        }
        if let Some(e) = inner.errors.get(name) {
            return Err(inventory_error(e.clone()));
        }
        Err(inventory_error(
            Diagnostic::error(
                "inventory::unknown_target",
                format!("target `{name}` not found"),
            )
            .with_help("list targets with `krab inventory targets`"),
        ))
    }

    pub fn info(&self) -> InfoResult {
        let inner = self.state.read();
        InfoResult {
            version: self.cfg.version.clone(),
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
            exe: std::env::current_exe().unwrap_or_default(),
            ready: self.state.is_ready(),
            resolvers: self.state.inv.registry.description().to_string(),
            inventory_path: self.state.inv.cfg.root.clone(),
            socket: self.cfg.socket.clone(),
            log: self.cfg.log.clone(),
            generation: inner.generation,
            targets: inner.targets.len(),
            errors: inner.errors.len(),
            uptime_secs: self.state.started.elapsed().as_secs(),
            idle_timeout_secs: self.cfg.idle_timeout.as_secs(),
            last_change: inner.history.back().cloned(),
        }
    }
}

fn params<T: DeserializeOwned>(req: &Request) -> Result<T, RpcError> {
    serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
        code: ERR_PARAMS,
        message: format!("invalid params for {}: {e}", req.method),
        data: None,
    })
}

pub fn target_labels(
    t: &krab_inventory::RenderedTarget,
) -> std::collections::BTreeMap<String, String> {
    t.parameters
        .get("kapitan")
        .and_then(|k| k.get("labels"))
        .and_then(|l| l.as_map())
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.value.py_str()))
                .collect()
        })
        .unwrap_or_default()
}

/// Input types of `parameters.kapitan.compile` with counts (`kadet×2`), in order of appearance.
pub fn target_inputs(t: &krab_inventory::RenderedTarget) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for item in t
        .parameters
        .get("kapitan")
        .and_then(|k| k.get("compile"))
        .and_then(|c| c.as_list())
        .unwrap_or(&[])
    {
        let paths = item
            .get("input_paths")
            .and_then(|p| p.as_list())
            .map(|l| l.len())
            .unwrap_or(0);
        if paths == 0 {
            continue;
        }
        let ty = item
            .get("input_type")
            .map(|n| n.value.py_str())
            .unwrap_or_else(|| "?".into());
        match counts.iter_mut().find(|(k, _)| *k == ty) {
            Some((_, n)) => *n += 1,
            None => counts.push((ty, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(k, n)| if n > 1 { format!("{k}×{n}") } else { k })
        .collect()
}

/// `parameters.kapitan.labels` contains every wanted pair.
pub fn has_labels(t: &krab_inventory::RenderedTarget, wanted: &[(String, String)]) -> bool {
    let labels = t
        .parameters
        .get("kapitan")
        .and_then(|k| k.get("labels"))
        .and_then(|l| l.as_map());
    wanted.iter().all(|(k, v)| {
        labels
            .and_then(|m| m.get(k))
            .is_some_and(|n| n.value.py_str() == *v)
    })
}

pub fn inventory_error(d: Diagnostic) -> RpcError {
    RpcError {
        code: ERR_INVENTORY,
        message: d.message.clone(),
        data: Some(json!([d])),
    }
}

pub fn socket_alive(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}
