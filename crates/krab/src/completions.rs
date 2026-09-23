//! Shell completions: static scripts and dynamic target-name completion.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use clap_complete::CompletionCandidate;
use krab_inventory::dotkapitan::DotKapitan;
use krab_inventory::{Inventory, InventoryConfig, Registry};

#[derive(Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
            Shell::Elvish => "elvish",
            Shell::Powershell => "powershell",
        }
    }
}

/// Print the registration script. Completion of subcommands, flags and target
/// names is computed live by the binary (`COMPLETE=<shell> <name>`), where
/// `<name>` is whatever this process was invoked as (`krab`, `krab-dev`, a
/// path), so a renamed or symlinked install completes under its own name.
pub fn print_registration(shell: Shell) -> std::io::Result<()> {
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
        .completer(shell.name())
        .ok_or_else(|| std::io::Error::other(format!("unsupported shell {}", shell.name())))?;
    let invoked = invoked_as();
    let (name, bin) = registration_names(&invoked, std::env::current_dir().ok().as_deref());
    let mut out = std::io::stdout().lock();
    completer.write_registration("COMPLETE", &name, &name, &bin, &mut out)
}

/// How this process was started: `argv[0]` as the shell passed it. Not
/// `current_exe()`, which follows symlinks and would report the link target
/// (`krab`) for an install like `~/.local/bin/krab-dev -> .../krab`.
fn invoked_as() -> PathBuf {
    std::env::args_os()
        .next()
        .filter(|a| !a.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("krab"))
}

/// The command name to register completion for and the completer the shell
/// should run. A bare name is left for the shell to find on `PATH`; a path is
/// made absolute so the script keeps working from other directories.
fn registration_names(invoked: &Path, cwd: Option<&Path>) -> (String, String) {
    let name = invoked
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "krab".into());
    let mut completer = invoked.to_path_buf();
    if completer.components().count() > 1 {
        if let Some(cwd) = cwd {
            completer = cwd.join(completer);
        }
    }
    (name, completer.to_string_lossy().into_owned())
}

/// Target names of the inventory in the current directory (cheap: a directory
/// walk, no rendering, no server).
pub fn complete_target(current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    let Ok(cwd) = std::env::current_dir() else {
        return vec![];
    };
    let dot = DotKapitan::load(&cwd).unwrap_or_default();
    let root = dot
        .inventory_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("./inventory"));
    let mut cfg = InventoryConfig::new(root);
    cfg.compose_target_name = dot.compose_target_name.unwrap_or(false);
    let inv = Inventory::new(cfg, std::sync::Arc::new(Registry::new()));
    inv.discover_targets()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.name.starts_with(&*current))
        .map(|t| CompletionCandidate::new(t.name).help(Some(t.path.into())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_name_is_kept_for_path_lookup() {
        let (name, bin) = registration_names(Path::new("krab-dev"), Some(Path::new("/work")));
        assert_eq!(name, "krab-dev");
        assert_eq!(bin, "krab-dev");
    }

    #[test]
    fn relative_path_is_anchored_to_cwd() {
        let (name, bin) =
            registration_names(Path::new("target/release/krab"), Some(Path::new("/work")));
        assert_eq!(name, "krab");
        assert_eq!(bin, "/work/target/release/krab");
    }

    #[test]
    fn absolute_path_is_unchanged() {
        let (name, bin) = registration_names(Path::new("/opt/bin/kap"), Some(Path::new("/work")));
        assert_eq!(name, "kap");
        assert_eq!(bin, "/opt/bin/kap");
    }
}
