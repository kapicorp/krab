//! Shared CLI state: the inventory handle, output settings, server connector.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use krab_inventory::dotkapitan::DotKapitan;
use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::error::Diagnostic;
use krab_inventory::resolvers::python::{PythonConfig, PythonResolvers};
use krab_inventory::{Inventory, InventoryConfig, Map, Node, Registry, Value};
use krab_server::protocol::AllResult;
use krab_server::{Client, ClientError, Connector};

use crate::report;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version plus an identity of this very binary, so a rebuilt kapitan restarts
/// a server left behind by the previous build even when the version is unchanged.
pub fn build_version() -> String {
    let id = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}-{mtime}", m.len())
        })
        .unwrap_or_default();
    format!("{VERSION}+{id}")
}

pub enum Failure {
    Diagnostics(Vec<Diagnostic>, bool),
    Message(String),
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Failure::Message(e.to_string())
    }
}

#[derive(Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Json,
}

pub struct App {
    pub inv: Inventory,
    pub dot: DotKapitan,
    pub cwd: PathBuf,
    pub inventory_path: PathBuf,
    pub json: bool,
    pub indent: usize,
    /// `None` when the server must not be used (`--no-daemon`, `--raw`).
    pub connector: Option<Connector>,
}

impl App {
    pub fn new(
        inventory_path: Option<PathBuf>,
        json: bool,
        raw: bool,
        no_daemon: bool,
    ) -> Result<App, Failure> {
        let cwd = std::env::current_dir()?;
        let dot = DotKapitan::load(&cwd).map_err(|e| Failure::Message(e.to_string()))?;
        let inventory_path = inventory_path
            .or(dot.inventory_path.clone())
            .unwrap_or_else(|| PathBuf::from("./inventory"));
        if !inventory_path.join("targets").is_dir() {
            return Err(Failure::Diagnostics(
                vec![
                    Diagnostic::error("inventory::no_targets_dir", format!("no inventory at {}", inventory_path.display()))
                        .with_help("run from the directory containing `.kapitan`/`inventory/`, or pass --inventory-path"),
                ],
                json,
            ));
        }
        let inventory_path = inventory_path.canonicalize().unwrap_or(inventory_path);
        let mut cfg = InventoryConfig::new(inventory_path.clone());
        cfg.compose_target_name = dot.compose_target_name.unwrap_or(false);
        cfg.normalize = !raw;
        let mut registry = Registry::with_builtins();
        match PythonConfig::discover(&inventory_path, &cwd, &dot.python_resolvers) {
            Some(python) => {
                if !python.python.exists() {
                    return Err(Failure::Diagnostics(
                        vec![missing_python(&python.python.description, &dot)],
                        json,
                    ));
                }
                tracing::info!(file = %python.file.display(), python = %python.python.description, "Python resolvers configured");
                let resolvers = PythonResolvers::new(python);
                PythonResolvers::install(&resolvers, &mut registry).map_err(|e| {
                    Failure::Diagnostics(
                        vec![Diagnostic::error("inventory::python_resolvers", e).with_help(
                            "fix the file, point `inventory.python-resolvers.file` in `.kapitan` elsewhere, or set `inventory.python-resolvers: false`",
                        )],
                        json,
                    )
                })?;
            }
            None if dot.python_resolvers.enabled == Some(false) => {
                registry.set_description("Python resolvers disabled in .kapitan");
            }
            None => registry.set_description("no resolvers.py found, native resolvers only"),
        }
        let inv = Inventory::new(cfg, Arc::new(registry));
        let connector = (!no_daemon && !raw).then(|| Connector {
            inventory_root: inventory_path.clone(),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("krab")),
            version: build_version(),
            idle_timeout: Duration::from_secs(1800),
        });
        Ok(App {
            inv,
            indent: dot.indent.unwrap_or(2),
            dot,
            cwd,
            inventory_path,
            json,
            connector,
        })
    }

    pub fn fail(&self, errors: Vec<krab_inventory::Error>) -> Failure {
        Failure::Diagnostics(
            errors
                .into_iter()
                .map(|e| e.resolve(&self.inv.sources).into_diagnostic())
                .collect(),
            self.json,
        )
    }

    pub fn rpc_fail(&self, e: ClientError) -> Failure {
        match e {
            ClientError::Rpc(err) => {
                let diags: Vec<Diagnostic> = err
                    .data
                    .and_then(|d| serde_json::from_value(d).ok())
                    .unwrap_or_default();
                if diags.is_empty() {
                    Failure::Diagnostics(
                        vec![Diagnostic::error("server::error", err.message)],
                        self.json,
                    )
                } else {
                    Failure::Diagnostics(diags, self.json)
                }
            }
            other => Failure::Message(other.to_string()),
        }
    }

    /// A connected client, starting the server when needed; `None` means render locally.
    pub fn client(&self) -> Option<Client> {
        let connector = self.connector.as_ref()?;
        match connector.connect_or_spawn() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(
                    socket = %connector.socket().display(),
                    "inventory server unavailable ({e}); rendering locally"
                );
                None
            }
        }
    }

    pub fn dump_opts(&self) -> DumpOptions {
        DumpOptions {
            indent: self.indent,
            ..DumpOptions::default()
        }
    }

    pub fn warn_all(&self, warnings: &[Diagnostic]) {
        for w in warnings {
            report::print_diagnostic(&w.clone().resolve(&self.inv.sources), self.json);
        }
    }

    pub fn print_value(&self, node: &Node, format: Format, indent: Option<usize>) {
        match format {
            Format::Yaml => {
                let opts = DumpOptions {
                    indent: indent.unwrap_or(self.indent),
                    ..DumpOptions::default()
                };
                print!("{}", dump_yaml(node, &opts));
            }
            Format::Json => println!("{}", serde_json::to_string_pretty(node).unwrap()),
        }
    }

    /// Every target's document (optionally only those carrying `labels`),
    /// from the server when possible.
    pub fn all_documents(&self, labels: &[(String, String)]) -> Result<Map, Failure> {
        let mut m = Map::new();
        match self.client() {
            Some(mut c) => {
                let all: AllResult = c
                    .call(
                        "inventory.all",
                        krab_server::protocol::TargetsParams {
                            labels: labels.to_vec(),
                        },
                    )
                    .map_err(|e| self.rpc_fail(e))?;
                if !all.errors.is_empty() {
                    return Err(Failure::Diagnostics(all.errors, self.json));
                }
                for (name, doc) in all.documents {
                    m.insert(name, Node::synthetic(doc.into()));
                }
            }
            None => {
                let report = self.inv.render_all().map_err(|e| self.fail(vec![e]))?;
                if !report.errors.is_empty() {
                    return Err(self.fail(report.errors));
                }
                for (name, t) in &report.targets {
                    if !labels.is_empty() && !krab_server::rpc::has_labels(t, labels) {
                        continue;
                    }
                    self.warn_all(&t.warnings);
                    m.insert(name.clone(), t.to_document());
                }
            }
        }
        Ok(m)
    }

    pub fn dump_value(&self, node: &Node, format: Format) -> String {
        match format {
            Format::Yaml => dump_yaml(node, &self.dump_opts()),
            Format::Json => serde_json::to_string_pretty(node).unwrap(),
        }
    }
}

/// The configured interpreter is not an executable on this machine.
fn missing_python(python: &str, dot: &DotKapitan) -> Diagnostic {
    let (what, help) = if std::env::var_os("KRAB_PYTHON").is_some() {
        (
            format!("KRAB_PYTHON names `{python}`, which is not an executable here"),
            "unset it, or point it at a Python that can import omegaconf",
        )
    } else if dot.python_resolvers.python.is_some() {
        (
            format!(
                "`inventory.python-resolvers.python: {python}` in .kapitan is not an executable here"
            ),
            "install it, change the key, or override it for this machine with KRAB_PYTHON=/path/to/python",
        )
    } else {
        (
            format!("no Python interpreter found (`{python}`)"),
            "install python3, or set KRAB_PYTHON=/path/to/python",
        )
    };
    Diagnostic::error("inventory::python_resolvers", what).with_help(help)
}

pub fn flatten(node: &Node) -> Node {
    fn walk(node: &Node, prefix: &str, out: &mut Map) {
        match &node.value {
            Value::Map(m) => {
                for (k, v) in m {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(v, &key, out);
                }
            }
            _ => {
                out.insert(prefix.to_string(), node.clone());
            }
        }
    }
    let mut out = Map::new();
    walk(node, "", &mut out);
    Node::synthetic(Value::Map(out))
}
