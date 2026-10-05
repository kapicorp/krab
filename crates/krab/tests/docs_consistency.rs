//! The version of the reference implementation krab matches appears in prose
//! across the repository. There is no machine-readable source for it, so this
//! checks the copies against each other: bumping the parity target in one file
//! and not the rest is the failure that matters, and it is quiet.
//!
//! Mentions without a patch version (`kapitan 0.36`) name the minor series on
//! purpose and are left alone.
//!
//! The daemon's RPC methods and the diagnostic codes are lists in the docs
//! whose source is the code, so those are checked against the code.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Every `kapitan <major>.<minor>.<patch>` in the file, with its line number.
fn parity_versions(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("kapitan ") {
            let tail = &rest[at + "kapitan ".len()..];
            let end = tail
                .find(|c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(tail.len());
            let candidate = &tail[..end];
            if candidate.split('.').count() == 3 && candidate.split('.').all(|p| !p.is_empty()) {
                found.push((n + 1, candidate.to_string()));
            }
            rest = &tail[end..];
        }
    }
    found
}

#[test]
fn every_document_names_the_same_reference_version() {
    let root = repo_root();
    let mut seen: Vec<(PathBuf, usize, String)> = Vec::new();

    let mut markdown = Vec::new();
    collect_markdown(&root, &mut markdown);
    assert!(
        markdown.len() > 3,
        "found {} markdown files under {}; the walk is broken, not the docs",
        markdown.len(),
        root.display()
    );

    for path in markdown {
        let text = std::fs::read_to_string(&path).expect("reading a markdown file");
        for (line, version) in parity_versions(&text) {
            seen.push((path.clone(), line, version));
        }
    }

    assert!(
        !seen.is_empty(),
        "no document names the reference version any more; this test guards nothing"
    );

    let (_, _, first) = &seen[0];
    let disagreeing: Vec<String> = seen
        .iter()
        .filter(|(_, _, v)| v != first)
        .map(|(p, l, v)| {
            format!(
                "{}:{l} says {v}",
                p.strip_prefix(&root).unwrap_or(p).display()
            )
        })
        .collect();

    assert!(
        disagreeing.is_empty(),
        "documents disagree on which kapitan release krab matches. \
         Most say {first}, but:\n  {}",
        disagreeing.join("\n  ")
    );
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let skip = ["target", "vendor", "node_modules", ".git"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !skip.contains(&name.as_ref()) {
                collect_markdown(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The backticked `server.*` and `inventory.*` names in `text`.
fn method_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = text
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|s| {
            (s.starts_with("server.") || s.starts_with("inventory."))
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.')
        })
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn every_rpc_method_is_documented() {
    let mut served: Vec<String> = read("crates/krab-server/src/rpc.rs")
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix('"')?
                .split_once("\" =>")
                .map(|(m, _)| m)
        })
        .filter(|m| m.starts_with("server.") || m.starts_with("inventory."))
        .map(str::to_string)
        .collect();
    served.sort();
    assert!(
        served.len() > 5,
        "found {served:?} in rpc.rs; the scan is broken"
    );

    let cli = read("docs/CLI.md");
    let start = cli
        .find("The protocol is JSON-RPC 2.0")
        .expect("docs/CLI.md lists the RPC methods");
    let paragraph = cli[start..].split("\n\n").next().unwrap();
    assert_eq!(
        method_names(paragraph),
        served,
        "docs/CLI.md against rpc.rs"
    );

    let protocol = read("crates/krab-server/src/protocol.rs");
    let doc: String = protocol
        .lines()
        .filter_map(|l| l.strip_prefix("//! * "))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        method_names(&doc),
        served,
        "protocol.rs module doc against rpc.rs"
    );
}

/// Every `"area::name"` string literal on the line.
fn code_literals(line: &str) -> Vec<String> {
    let word = |c: char| c.is_ascii_lowercase() || c == '_';
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = line[from..].find("::").map(|i| from + i) {
        from = at + 2;
        let area_start = line[..at].rfind(|c| !word(c)).map_or(0, |i| i + 1);
        let name_end = line[at + 2..]
            .find(|c| !word(c))
            .map_or(line.len(), |i| at + 2 + i);
        if area_start > 0
            && area_start < at
            && name_end > at + 2
            && line[..area_start].ends_with('"')
            && line[name_end..].starts_with('"')
        {
            found.push(line[area_start..name_end].to_string());
        }
    }
    found
}

fn collect_rust(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_diagnostic_code_is_documented() {
    let root = repo_root();
    let mut sources = Vec::new();
    for krate in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        let src = krate.path().join("src");
        if src.is_dir() {
            collect_rust(&src, &mut sources);
        }
    }
    let mut codes: Vec<String> = sources
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .flat_map(code_literals)
                .collect::<Vec<_>>()
        })
        .collect();
    codes.sort();
    codes.dedup();
    assert!(codes.len() > 20, "found {codes:?}; the scan is broken");

    let doc = read("docs/diagnostics.md");
    let missing: Vec<&String> = codes
        .iter()
        .filter(|c| !doc.contains(&format!("`{c}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "diagnostic codes with no entry in docs/diagnostics.md: {missing:?}"
    );
}
