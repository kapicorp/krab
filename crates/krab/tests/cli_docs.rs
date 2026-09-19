//! `docs/CLI.md` is the flag reference. Nothing stopped it from drifting away
//! from `--help`, so this walks every subcommand of the built binary and fails
//! when a command or a long flag is missing from the document.
//!
//! The check is deliberately one-directional. A flag that exists and is not
//! documented is a bug; a document that still mentions a flag that was removed
//! is a judgement call, and a migration note has a reason to outlive the flag.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn help(path: &[String]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(path)
        .arg("--help")
        .output()
        .expect("running the binary");
    assert!(
        out.status.success(),
        "krab {} --help failed: {}",
        path.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("help is utf-8")
}

/// The subcommand names listed under `Commands:`, in order.
fn subcommands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in help.lines() {
        if line == "Commands:" {
            inside = true;
            continue;
        }
        if inside {
            // A blank line, or any unindented line, ends the section.
            if line.trim().is_empty() || !line.starts_with("  ") {
                break;
            }
            // Entries are `  name  description`; anything more deeply indented
            // is the continuation of the previous description.
            if line.starts_with("    ") {
                continue;
            }
            if let Some(name) = line.split_whitespace().next()
                && name != "help"
            {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Every `--long-flag` the help text mentions, except the two clap adds to
/// every command.
fn long_flags(help: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    let bytes: Vec<char> = help.chars().collect();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == '-' && bytes[i + 1] == '-' && bytes[i + 2].is_ascii_lowercase() {
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_lowercase() || bytes[end] == '-') {
                end += 1;
            }
            // A trailing dash belongs to the prose, not to the flag.
            let name: String = bytes[start..end].iter().collect();
            let name = name.trim_end_matches('-').to_string();
            if name != "help" && name != "version" {
                flags.insert(format!("--{name}"));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    flags
}

/// Walks the command tree, collecting `(path, subcommands, flags)`.
fn walk(path: Vec<String>, commands: &mut BTreeSet<String>, flags: &mut BTreeSet<String>) {
    let text = help(&path);
    for flag in long_flags(&text) {
        flags.insert(flag);
    }
    for name in subcommands(&text) {
        let mut child = path.clone();
        child.push(name.clone());
        commands.insert(child.join(" "));
        walk(child, commands, flags);
    }
}

#[test]
fn every_command_and_flag_is_in_the_cli_reference() {
    let doc_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/CLI.md")
        .canonicalize()
        .expect("docs/CLI.md exists");
    let doc = std::fs::read_to_string(&doc_path).expect("reading docs/CLI.md");

    let mut commands = BTreeSet::new();
    let mut flags = BTreeSet::new();
    walk(Vec::new(), &mut commands, &mut flags);

    assert!(
        !commands.is_empty() && !flags.is_empty(),
        "parsed no commands or no flags out of --help; the parser is broken, \
         not the documentation"
    );

    let missing_commands: Vec<_> = commands
        .iter()
        .filter(|c| !doc.contains(c.as_str()))
        .collect();
    let missing_flags: Vec<_> = flags.iter().filter(|f| !doc.contains(f.as_str())).collect();

    assert!(
        missing_commands.is_empty() && missing_flags.is_empty(),
        "docs/CLI.md does not mention:\n  commands: {missing_commands:?}\n  flags: {missing_flags:?}\n\
         Document them in {}, or remove them from the CLI.",
        doc_path.display()
    );
}
