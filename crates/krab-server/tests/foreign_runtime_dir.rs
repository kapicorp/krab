//! The client refuses a runtime directory that is not a directory of its own
//! user (SRV-14a): whoever controls that directory could answer on the socket.

use std::time::{Duration, Instant};

use krab_server::Connector;

#[test]
fn the_client_refuses_a_runtime_directory_that_is_not_its_own() {
    // Not temp_dir(): on macOS that path is long enough that the socket
    // path would exceed the unix socket limit and krab would fall back to
    // /tmp/krab-<uid> (SRV-15), which is not the directory under test.
    let base = std::path::PathBuf::from(format!("/tmp/krab-ft-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, base.join("krab")).unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &base);
        std::env::set_var("XDG_STATE_HOME", &base);
    }
    let connector = Connector {
        inventory_root: base.clone(),
        exe: "true".into(),
        version: "test".into(),
        idle_timeout: Duration::from_secs(1),
    };

    assert!(
        connector.socket().starts_with(base.join("krab")),
        "the socket is not in the directory under test: {}",
        connector.socket().display()
    );

    let start = Instant::now();
    let err = connector.connect_or_spawn().err().expect("must refuse");
    let elapsed = start.elapsed();
    let existing = connector.connect_existing();
    let written: Vec<_> = std::fs::read_dir(&elsewhere).unwrap().flatten().collect();

    std::fs::remove_dir_all(&base).unwrap();
    assert!(existing.is_none());
    assert!(
        written.is_empty(),
        "wrote into the other directory: {written:?}"
    );
    assert!(elapsed < Duration::from_secs(2), "waited {elapsed:?}");
    assert!(
        err.to_string().contains("not a directory owned by"),
        "{err}"
    );
}
