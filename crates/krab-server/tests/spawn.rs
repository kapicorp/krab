//! A daemon that exits during start-up is reaped and reported at once,
//! instead of the client waiting the full start-up timeout for its socket.

use std::time::{Duration, Instant};

use krab_server::Connector;

#[test]
fn a_daemon_that_exits_at_once_does_not_cost_the_start_timeout() {
    let dir = std::env::temp_dir().join(format!("krab-spawn-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment. Keeps the socket and log out of the user's directories.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        std::env::set_var("XDG_STATE_HOME", &dir);
    }
    let connector = Connector {
        inventory_root: dir.clone(),
        exe: "false".into(),
        version: "test".into(),
        idle_timeout: Duration::from_secs(1),
    };
    let start = Instant::now();
    let err = connector
        .connect_or_spawn()
        .err()
        .expect("no server can start");
    let elapsed = start.elapsed();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(elapsed < Duration::from_secs(5), "waited {elapsed:?}");
    assert!(err.to_string().contains("exit status: 1"), "{err}");
}
