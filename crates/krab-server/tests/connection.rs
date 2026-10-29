//! A connection to a running server answers requests (SRV-51). On macOS an
//! accepted socket inherits O_NONBLOCK from the listener, so a server that
//! did not clear it dropped every client after accept.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use krab_inventory::resolvers::Registry;
use krab_inventory::{Inventory, InventoryConfig};
use krab_server::{Client, Server, ServerConfig, State};

#[test]
fn a_connection_is_answered() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    // Short: the socket path must fit the unix socket limit.
    let dir = PathBuf::from(format!("/tmp/krab-conn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("s.sock");
    let state = Arc::new(State::new(Inventory::new(
        InventoryConfig::new(fixtures.join("inventory")),
        Arc::new(Registry::with_builtins()),
    )));
    state.render_all();
    let server = Arc::new(Server {
        state,
        cfg: ServerConfig {
            socket: socket.clone(),
            log: None,
            idle_timeout: Duration::from_secs(60),
            version: "test".into(),
        },
        shutdown: AtomicBool::new(false),
    });
    let listener = server.bind().unwrap();
    let serving = std::thread::spawn(move || server.serve_on(listener));

    let mut client = Client::connect(&socket).unwrap();
    let info = client.info();
    // A second request on the same connection: the read after the first
    // answer must block too.
    let stopped = client.shutdown();
    let served = serving.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(info.unwrap().version, "test");
    stopped.unwrap();
    served.unwrap();
}
