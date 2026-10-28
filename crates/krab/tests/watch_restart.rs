//! `krab inventory watch` survives the daemon restarting itself after a
//! configuration source (`.kapitan`) changes.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

fn krab(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_krab"));
    cmd.current_dir(dir)
        .env_remove("KRAB_NO_DAEMON")
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("XDG_STATE_HOME", dir.join("state"));
    cmd
}

/// Rewrite the target until watch reports a change to it, or give up.
fn saw_target_change(watch: &mut Child, lines: &Receiver<String>, target: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut n = 0;
    while Instant::now() < deadline {
        if watch.try_wait().unwrap().is_some() {
            return false;
        }
        // A write before the daemon watches the files is not reported; retry.
        n += 1;
        std::fs::write(target, format!("parameters:\n  a: {n}\n")).unwrap();
        while let Ok(line) = lines.recv_timeout(Duration::from_millis(500)) {
            if line.contains("t.yml") {
                return true;
            }
        }
    }
    false
}

#[test]
fn watch_survives_a_daemon_restart() {
    let dir = std::env::temp_dir().join(format!("krab-watch-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::create_dir_all(dir.join("run")).unwrap();
    let dot = "global:\n  inventory-backend: omegaconf\n";
    std::fs::write(dir.join(".kapitan"), dot).unwrap();
    let target = dir.join("inventory/targets/t.yml");
    std::fs::write(&target, "parameters:\n  a: 0\n").unwrap();

    let mut watch = krab(&dir)
        .args(["--json", "inventory", "watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = watch.stdout.take().unwrap();
    let (tx, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let before = saw_target_change(&mut watch, &lines, &target);
    // The daemon re-renders this batch, answers the wait, then exits.
    std::fs::write(dir.join(".kapitan"), format!("{dot}# changed\n")).unwrap();
    // A late event for t.yml can come first (FSEvents on macOS), so read on.
    let deadline = Instant::now() + Duration::from_secs(10);
    let restarted = before
        && std::iter::from_fn(|| {
            lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok()
        })
        .any(|l| l.contains(".kapitan"));
    let after = restarted && saw_target_change(&mut watch, &lines, &target);
    let alive = watch.try_wait().unwrap().is_none();

    let _ = watch.kill();
    let mut stderr = String::new();
    let _ = watch.stderr.take().unwrap().read_to_string(&mut stderr);
    let _ = watch.wait();
    let _ = krab(&dir).args(["server", "stop"]).output();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        before,
        "watch reported no change before the restart: {stderr}"
    );
    assert!(
        restarted,
        "watch did not report the .kapitan change: {stderr}"
    );
    assert!(
        after && alive,
        "watch did not survive the restart: {stderr}"
    );
}
