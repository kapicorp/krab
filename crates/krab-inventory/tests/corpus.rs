//! Emitter parity against a corpus of compiled files. Each corpus entry is a
//! JSON file `{kind: yaml|json, path, multi, docs: [...]}` produced by loading
//! a file that kapitan 0.36 wrote; the original file must be reproduced.
//! Skipped unless `KRAB_CORPUS` (the JSON dir) and `KRAB_COMPILED`
//! (the compiled dir) are set.
// A skipped test that says nothing looks like a passing one, so these report
// why they did not run.
#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};

use krab_inventory::emit::ryml::{RymlOptions, dump_ryml, needs_pyyaml_fallback};
use krab_inventory::emit::yaml::{DumpOptions, dump_yaml, dump_yaml_all};
use krab_inventory::emit::{MultilineStyle, dumps_pretty};
use krab_inventory::{Node, Value};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn corpus_parity() {
    let (Ok(corpus), Ok(compiled)) = (std::env::var("KRAB_CORPUS"), std::env::var("KRAB_COMPILED"))
    else {
        eprintln!("KRAB_CORPUS / KRAB_COMPILED not set; skipping");
        return;
    };
    let mut files = Vec::new();
    walk(Path::new(&corpus), &mut files);
    files.sort();
    let mut ok = 0;
    let mut failures: Vec<(String, String)> = Vec::new();
    for f in &files {
        if f.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".krab-manifest"))
        {
            continue;
        }
        let entry: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        let rel = entry["path"].as_str().unwrap();
        let expected = std::fs::read_to_string(Path::new(&compiled).join(rel)).unwrap();
        let docs: Vec<Node> = entry["docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| Node::synthetic(d.clone().into()))
            .collect();
        let actual = if entry["kind"] == "json" {
            dumps_pretty(&docs[0].value, 2, true)
        } else {
            let multi = entry["multi"].as_bool().unwrap_or(false);
            let value = if multi {
                Value::List(docs.clone())
            } else {
                docs[0].value.clone()
            };
            let node = Node::synthetic(value);
            if needs_pyyaml_fallback(&node.value) {
                let opts = DumpOptions {
                    multiline: Some(MultilineStyle::DoubleQuotes),
                    null_as_empty: true,
                    ..DumpOptions::default()
                };
                if multi {
                    dump_yaml_all(&docs, &opts)
                } else {
                    dump_yaml(&node, &opts)
                }
            } else {
                dump_ryml(
                    &node,
                    &RymlOptions {
                        multiline: MultilineStyle::DoubleQuotes,
                        null_as_empty: true,
                    },
                    multi,
                )
            }
        };
        if actual == expected {
            ok += 1;
        } else {
            let diff = first_diff(&expected, &actual);
            failures.push((rel.to_string(), diff));
        }
    }
    for (rel, diff) in failures.iter().take(15) {
        eprintln!("--- {rel}\n{diff}");
    }
    eprintln!(
        "corpus: {ok} identical, {} different out of {}",
        failures.len(),
        files.len()
    );
    assert!(failures.is_empty(), "{} files differ", failures.len());
}

fn first_diff(expected: &str, actual: &str) -> String {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    for i in 0..e.len().max(a.len()) {
        if e.get(i) != a.get(i) {
            let (el, al) = (
                e.get(i).copied().unwrap_or("<eof>"),
                a.get(i).copied().unwrap_or("<eof>"),
            );
            let common = el
                .chars()
                .zip(al.chars())
                .take_while(|(x, y)| x == y)
                .count();
            let start = common.saturating_sub(40);
            let cut = |s: &str| s.chars().skip(start).take(120).collect::<String>();
            return format!(
                "line {} (col {}):\n  expected: …{:?}\n  actual:   …{:?}",
                i + 1,
                common + 1,
                cut(el),
                cut(al)
            );
        }
    }
    if expected.ends_with('\n') != actual.ends_with('\n') {
        return "trailing newline differs".into();
    }
    "identical lines but different bytes".into()
}
