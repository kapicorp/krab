//! The krab inventory server: an always-on process holding the rendered
//! inventory in memory, watching the files, re-rendering only what changed,
//! and answering JSON-RPC over a unix socket. Plus the client side.

pub mod client;
pub mod paths;
pub mod protocol;
pub mod rpc;
pub mod state;
pub mod watch;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use krab_inventory::Inventory;

pub use client::{Client, ClientError, Connector};
pub use rpc::{Server, ServerConfig};
pub use state::State;

/// Run a server in the current process until it is asked to stop or idles out.
pub fn run(inv: Inventory, idle_timeout: Duration, version: String) -> std::io::Result<()> {
    let root = inv.cfg.root.clone();
    let socket = paths::socket_path(&root, &version);
    let log = paths::log_path(&root);
    let state = Arc::new(State::new(inv));
    let server = Arc::new(Server {
        state: state.clone(),
        cfg: ServerConfig {
            socket,
            log: Some(log),
            idle_timeout,
            version,
        },
        shutdown: AtomicBool::new(false),
    });
    // Bind before rendering: a second starter sees the socket at once instead
    // of rendering too and losing at bind time, and clients wait on the socket
    // (`State::wait_ready`) rather than on a start-up timeout.
    let listener = server.bind()?;
    let accept = {
        let server = server.clone();
        std::thread::spawn(move || server.serve_on(listener))
    };
    // Watch before the first render: a file changed while it runs (a
    // checkout on a big inventory) is applied after it instead of lost (SRV-50).
    let _watcher =
        watch::start(&root, state.clone()).map_err(|e| std::io::Error::other(e.to_string()))?;
    tracing::info!("initial render started");
    let summary = state.render_all();
    tracing::info!(
        targets = summary.rerendered.len(),
        errors = summary.errors.len(),
        ms = summary.duration_ms,
        "initial render"
    );
    state.set_ready();
    accept
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("the accept loop panicked")))
}

pub fn socket_for(inventory_root: &Path, build: &str) -> std::path::PathBuf {
    paths::socket_path(inventory_root, build)
}
