//! A command answers the same through the daemon as with `--no-daemon`
//! (SRV-5).
//!
//! Each step changes the inventory the way a user does and compares exit
//! status and stdout of the two paths. The daemon picks changes up through
//! its watcher, so a step retries for a few seconds before it calls a
//! difference a failure. New cases are new steps.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Repo {
    dir: PathBuf,
    env: PathBuf,
}

impl Repo {
    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Exit status and stdout of `krab <args>` run in `cwd`.
    fn run(&self, cwd: &Path, args: &[&str], daemon: bool) -> (Option<i32>, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_krab"));
        cmd.args(args)
            .current_dir(cwd)
            .env("XDG_RUNTIME_DIR", self.env.join("run"))
            .env("XDG_STATE_HOME", self.env.join("state"));
        if daemon {
            cmd.env_remove("KRAB_NO_DAEMON");
        } else {
            cmd.env("KRAB_NO_DAEMON", "1");
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn assert_same(&self, args: &[&str], step: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let local = self.run(&self.dir, args, false);
            let daemon = self.run(&self.dir, args, true);
            if local == daemon {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "after `{step}`, `krab {}` differs:\n--no-daemon: {local:?}\ndaemon:      {daemon:?}",
                    args.join(" ")
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = self.run(&self.dir, &["server", "stop"], true);
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap());
    }
}

const TARGET: &str =
    "parameters:\n  kapitan:\n    vars:\n      target: NAME\n  data: ${from_file:data.txt}\n";

#[test]
fn the_daemon_answers_like_no_daemon() {
    // Short: the socket path must fit the unix socket limit (see SRV-15).
    let base = PathBuf::from(format!("/tmp/krab-dl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = Repo {
        dir: base.join("repo"),
        env: base.join("env"),
    };
    repo.write(
        ".kapitan",
        "global:\n  inventory-backend: omegaconf\n  compose-target-name: true\n",
    );
    repo.write("data.txt", "one");
    repo.write(
        "inventory/targets/env/t1.yml",
        &TARGET.replace("NAME", "t1"),
    );
    let show = ["inventory", "-t", "env.t1", "-p", "parameters.data"];
    repo.assert_same(&["inventory", "targets"], "first start");
    repo.assert_same(&show, "first start");

    // A file a resolver reads, not an inventory file.
    repo.write("data.txt", "two");
    repo.assert_same(&show, "change a file from_file reads");

    // Two target files with one name: the inventory cannot be listed.
    repo.write("inventory/targets/a/x.yml", &TARGET.replace("NAME", "x"));
    repo.write("inventory/targets/b/x.yml", &TARGET.replace("NAME", "x"));
    repo.write(".kapitan", "global:\n  inventory-backend: omegaconf\n");
    repo.assert_same(&["inventory", "targets"], "two targets with one name");
    std::fs::remove_dir_all(repo.dir.join("inventory/targets/b")).unwrap();
    repo.write(
        ".kapitan",
        "global:\n  inventory-backend: omegaconf\n  compose-target-name: true\n",
    );
    repo.assert_same(&["inventory", "targets"], "resolve the conflict");

    // A daemon first started from another directory.
    let _ = repo.run(&repo.dir, &["server", "stop"], true);
    let _ = repo.run(
        &repo.dir.join("inventory"),
        &["--inventory-path", ".", "inventory", "targets"],
        true,
    );
    repo.assert_same(
        &["inventory", "targets"],
        "start the daemon from inventory/",
    );
}
