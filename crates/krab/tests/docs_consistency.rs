//! The version of the reference implementation krab matches appears in prose
//! across the repository. There is no machine-readable source for it, so this
//! checks the copies against each other: bumping the parity target in one file
//! and not the rest is the failure that matters, and it is quiet.
//!
//! Mentions without a patch version (`kapitan 0.36`) name the minor series on
//! purpose and are left alone.

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
