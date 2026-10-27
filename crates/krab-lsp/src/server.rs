//! The LSP message loop.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::error::Diagnostic as KDiagnostic;
use krab_inventory::explain::Explanation;
use krab_inventory::path::{Key, KeyPath};
use krab_inventory::source::Location as KLocation;
use krab_inventory::{Node, Value};
use krab_server::protocol::{WaitParams, WaitResult};
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Completion, GotoDefinition, HoverRequest, Request as _};
use lsp_types::*;
use parking_lot::Mutex;
use url::Url;

use crate::backend::Backend;
use crate::yaml_index::{Hit, Pos, YamlIndex, interpolation_at};

struct State {
    backend: Arc<Backend>,
    /// Open buffers (unsaved content wins over disk).
    docs: HashMap<Url, String>,
    repo_root: PathBuf,
}

pub fn run_stdio(
    backend: Backend,
    repo_root: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec!["{".into(), ".".into(), " ".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let _init: InitializeParams =
        serde_json::from_value(connection.initialize(serde_json::to_value(capabilities)?)?)?;
    let backend = Arc::new(backend);
    let state = Arc::new(Mutex::new(State {
        backend: backend.clone(),
        docs: HashMap::new(),
        repo_root,
    }));

    // Diagnostics: publish now and after every change the daemon renders.
    let published: Arc<Mutex<HashSet<Url>>> = Arc::new(Mutex::new(HashSet::new()));
    publish_diagnostics(&connection.sender, &backend, &published, &state.lock());
    // Emptied on exit: the writer thread, and so `io_threads.join()`, ends
    // only once every sender is dropped.
    let diagnostics_sender = Arc::new(Mutex::new(Some(connection.sender.clone())));
    {
        let sender = diagnostics_sender.clone();
        let backend = backend.clone();
        let published = published.clone();
        let state = state.clone();
        std::thread::spawn(move || {
            let mut generation;
            let mut delay = std::time::Duration::from_secs(1);
            while sender.lock().is_some() {
                if let Ok(mut client) = backend.wait_client() {
                    // A restarted daemon counts generations from zero again.
                    generation = 0;
                    while let Ok(r) = client.call::<_, WaitResult>(
                        "inventory.wait",
                        WaitParams {
                            generation,
                            timeout_ms: Some(60_000),
                        },
                    ) {
                        delay = std::time::Duration::from_secs(1);
                        if !r.timed_out {
                            let Some(sender) = sender.lock().clone() else {
                                return;
                            };
                            publish_diagnostics(&sender, &backend, &published, &state.lock());
                        }
                        generation = r.generation;
                    }
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_secs(60));
            }
        });
    }

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    break;
                }
                let response = handle_request(&state, req);
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(n) => handle_notification(&state, n),
            Message::Response(_) => {}
        }
    }
    diagnostics_sender.lock().take();
    drop(connection);
    io_threads.join()?;
    Ok(())
}

fn handle_notification(state: &Mutex<State>, n: Notification) {
    let mut st = state.lock();
    match n.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(n.params) {
                st.docs.insert(p.text_document.uri, p.text_document.text);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(n.params)
                && let Some(change) = p.content_changes.into_iter().last()
            {
                st.docs.insert(p.text_document.uri, change.text);
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(n.params) {
                st.docs.remove(&p.text_document.uri);
            }
        }
        DidSaveTextDocument::METHOD => {}
        _ => {}
    }
}

fn handle_request(state: &Mutex<State>, req: Request) -> Response {
    let id = req.id.clone();
    let result = match req.method.as_str() {
        HoverRequest::METHOD => serde_json::from_value::<HoverParams>(req.params)
            .map(|p| serde_json::to_value(hover(&state.lock(), p)).unwrap()),
        GotoDefinition::METHOD => serde_json::from_value::<GotoDefinitionParams>(req.params)
            .map(|p| serde_json::to_value(definition(&state.lock(), p)).unwrap()),
        Completion::METHOD => serde_json::from_value::<CompletionParams>(req.params)
            .map(|p| serde_json::to_value(completion(&state.lock(), p)).unwrap()),
        _ => {
            return Response::new_err(
                id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("unsupported: {}", req.method),
            );
        }
    };
    match result {
        Ok(v) => Response::new_ok(id, v),
        Err(e) => Response::new_err(
            id,
            lsp_server::ErrorCode::InvalidParams as i32,
            e.to_string(),
        ),
    }
}

// ---- what is under the cursor -----------------------------------------------

enum Subject {
    /// A key path inside `parameters`.
    Param(KeyPath),
    /// A `${...}` reference: the parameter path it points at.
    Reference(KeyPath, String),
    /// A resolver call `${name:...}`.
    Resolver(String),
    /// An entry of the `classes:` list.
    Class(String),
}

fn document_text(st: &State, uri: &Url) -> Option<(PathBuf, String)> {
    let path = uri.to_file_path().ok()?;
    let text = match st.docs.get(uri) {
        Some(t) => t.clone(),
        None => std::fs::read_to_string(&path).ok()?,
    };
    Some((path, text))
}

fn subject_at(text: &str, pos: Position) -> Option<Subject> {
    let index = YamlIndex::parse(text);
    let at = Pos {
        line: pos.line,
        col: pos.character,
    };
    let hit = index.hit(at)?;
    let (entry, offset) = match hit {
        Hit::Key(e) => (e, None),
        Hit::Value(e, off) => (e, Some(off)),
    };
    let first = entry.path.0.first()?.as_str()?.to_string();
    if first == "classes" {
        return entry
            .value
            .as_ref()
            .map(|v| Subject::Class(v.text.trim().to_string()));
    }
    if first != "parameters" {
        return None;
    }
    let param_path = KeyPath(entry.path.0[1..].to_vec());
    if let (Some(off), Some(v)) = (offset, &entry.value)
        && let Some((expr, _)) = interpolation_at(&v.text, off)
    {
        return Some(reference_subject(&expr, &param_path));
    }
    Some(Subject::Param(param_path))
}

/// `${a.b}` → `a.b`; `${.x}` is relative to the referencing node's parent.
fn reference_subject(expr: &str, node: &KeyPath) -> Subject {
    let expr = expr.trim();
    if let Some((name, _)) = expr.split_once(':')
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && !expr.contains("${")
    {
        return Subject::Resolver(name.to_string());
    }
    let dots = expr.chars().take_while(|c| *c == '.').count();
    let rest = &expr[dots..];
    let mut base = if dots == 0 {
        KeyPath::root()
    } else {
        node.clone()
    };
    for _ in 0..dots {
        base.pop();
    }
    let mut path = base;
    for seg in krab_inventory::path::split_key(rest) {
        if seg.is_empty() {
            continue;
        }
        match seg.parse::<usize>() {
            Ok(i) => path.push(Key::Index(i)),
            Err(_) => path.push(Key::Str(seg)),
        }
    }
    Subject::Reference(path, expr.to_string())
}

fn targets_for(st: &State, file: &Path) -> Vec<String> {
    let mut targets = st.backend.targets_for_file(file);
    targets.sort();
    targets
}

// ---- hover ------------------------------------------------------------------

fn hover(st: &State, p: HoverParams) -> Option<Hover> {
    let uri = &p.text_document_position_params.text_document.uri;
    let (file, text) = document_text(st, uri)?;
    let subject = subject_at(&text, p.text_document_position_params.position)?;
    let md = match subject {
        Subject::Class(name) => class_hover(st, &name, &file),
        Subject::Resolver(name) => format!("Resolver `{name}`"),
        Subject::Param(path) => value_hover(st, &file, &path, None)?,
        Subject::Reference(path, expr) => value_hover(st, &file, &path, Some(&expr))?,
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    })
}

fn class_hover(st: &State, name: &str, from_file: &Path) -> String {
    let mut md = format!("Class `{name}`");
    match st.backend.resolve_class(name, from_file) {
        Some(p) => md.push_str(&format!("\n\n`{}`", rel(&p, &st.repo_root))),
        None => md.push_str("\n\n**not found** (no matching file under `classes/`)"),
    }
    if let Some(p) = st.backend.resolve_class(name, from_file) {
        let usage = st.backend.class_usage();
        if let Some(u) = usage.iter().find(|u| u.file == p) {
            md.push_str(&format!(
                "\n\nIncluded by {} target{}",
                u.targets,
                if u.targets == 1 { "" } else { "s" }
            ));
        }
    }
    md
}

fn value_hover(st: &State, file: &Path, path: &KeyPath, expr: Option<&str>) -> Option<String> {
    let targets = targets_for(st, file);
    if targets.is_empty() {
        return Some(format!(
            "`{path}`\n\nNo target includes this file, so it is never rendered."
        ));
    }
    let mut md = match expr {
        Some(e) => format!("`${{{e}}}` → `{path}`\n\n"),
        None => format!("`{path}`\n\n"),
    };
    // Group targets by resolved value so identical results are shown once.
    let mut groups: BTreeMap<String, (Explanation, Vec<String>)> = BTreeMap::new();
    let mut missing = Vec::new();
    for t in &targets {
        match st.backend.explain(t, &path.to_string()) {
            Some(e) => {
                let key = serde_json::to_string(&e.value).unwrap_or_default();
                groups
                    .entry(key)
                    .or_insert_with(|| (e, Vec::new()))
                    .1
                    .push(t.clone());
            }
            None => missing.push(t.clone()),
        }
    }
    // Most common value first.
    let mut groups: Vec<(Explanation, Vec<String>)> = groups.into_values().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.1.cmp(&b.1)));
    for (i, (e, ts)) in groups.iter().enumerate() {
        if i >= 4 {
            md.push_str(&format!(
                "…and {} more distinct value{}\n\n",
                groups.len() - 4,
                if groups.len() - 4 == 1 { "" } else { "s" }
            ));
            break;
        }
        let who = if ts.len() == targets.len() && targets.len() > 1 {
            format!("in all {} targets", ts.len())
        } else if ts.len() > 2 {
            format!("in `{}`, `{}` and {} more", ts[0], ts[1], ts.len() - 2)
        } else {
            ts.iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        md.push_str(&format!(
            "**{}** {who}\n\n```yaml\n{}```\n",
            e.r#type,
            snippet(&e.value)
        ));
        if let Some(o) = &e.origin {
            md.push_str(&format!("written at `{}`", rel_loc(o, &st.repo_root)));
        } else {
            md.push_str("written by kapitan");
        }
        if let Some(r) = &e.resolved_from {
            md.push_str(&format!(" · resolved from `{}`", r.expr));
            if let Some(o) = &r.source_origin {
                md.push_str(&format!(" (`{}`)", rel_loc(o, &st.repo_root)));
            }
        }
        if !e.history.is_empty() {
            md.push_str(&format!(
                " · {} earlier value{} overridden",
                e.history.len(),
                if e.history.len() == 1 { "" } else { "s" }
            ));
        }
        md.push_str("\n\n");
    }
    if !missing.is_empty() {
        md.push_str(&format!(
            "Not present in {} target{}.\n",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" }
        ));
    }
    Some(md)
}

fn snippet(v: &Value) -> String {
    let text = dump_yaml(&Node::synthetic(v.clone()), &DumpOptions::default());
    // PyYAML closes a scalar document with `...`; not useful in a tooltip.
    let text = text.strip_suffix("...\n").unwrap_or(&text).to_string();
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 12 {
        format!(
            "{}\n… ({} more lines)\n",
            lines[..12].join("\n"),
            lines.len() - 12
        )
    } else {
        text
    }
}

// ---- definition -------------------------------------------------------------

fn definition(st: &State, p: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
    let uri = &p.text_document_position_params.text_document.uri;
    let (file, text) = document_text(st, uri)?;
    let subject = subject_at(&text, p.text_document_position_params.position)?;
    let locations: Vec<lsp_types::Location> = match subject {
        Subject::Class(name) => {
            let p = st.backend.resolve_class(&name, &file)?;
            vec![lsp_types::Location {
                uri: Url::from_file_path(&p).ok()?,
                range: Range::default(),
            }]
        }
        Subject::Resolver(_) => return None,
        Subject::Param(path) | Subject::Reference(path, _) => {
            let mut out: Vec<KLocation> = Vec::new();
            for t in targets_for(st, &file) {
                if let Some(e) = st.backend.explain(&t, &path.to_string()) {
                    out.extend(e.origin);
                    for h in e.history {
                        if let krab_inventory::explain::HistoryEntry::Override {
                            old_location,
                            new_location,
                            ..
                        } = h
                        {
                            out.extend(old_location);
                            out.extend(new_location);
                        }
                    }
                }
            }
            out.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
            out.dedup();
            out.into_iter()
                .filter_map(|l| to_lsp_location(&l))
                .collect()
        }
    };
    if locations.is_empty() {
        return None;
    }
    Some(GotoDefinitionResponse::Array(locations))
}

fn to_lsp_location(l: &KLocation) -> Option<lsp_types::Location> {
    let uri = Url::from_file_path(&l.file).ok()?;
    let pos = Position {
        line: l.line.saturating_sub(1),
        character: l.col.saturating_sub(1),
    };
    Some(lsp_types::Location {
        uri,
        range: Range {
            start: pos,
            end: pos,
        },
    })
}

// ---- completion -------------------------------------------------------------

fn completion(st: &State, p: CompletionParams) -> Option<CompletionResponse> {
    let uri = &p.text_document_position.text_document.uri;
    let (file, text) = document_text(st, uri)?;
    let pos = p.text_document_position.position;
    let line = text.lines().nth(pos.line as usize)?;
    let before: String = line.chars().take(pos.character as usize).collect();

    // `${prefix` → parameter paths.
    if let Some(start) = before.rfind("${") {
        let prefix = &before[start + 2..];
        if prefix.contains(':') || prefix.contains('}') {
            return None;
        }
        let prefix = prefix.trim_start_matches('.');
        let (parent, partial) = match prefix.rsplit_once('.') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (String::new(), prefix.to_string()),
        };
        let targets = targets_for(st, &file);
        let target = targets.first()?;
        let path = if parent.is_empty() {
            "parameters".to_string()
        } else {
            format!("parameters.{parent}")
        };
        let value = st.backend.target_value(target, Some(&path))?;
        let keys: Vec<(String, String)> = match value {
            serde_json::Value::Object(m) => {
                m.iter().map(|(k, v)| (k.clone(), kind_of(v))).collect()
            }
            _ => return None,
        };
        let items = keys
            .into_iter()
            .filter(|(k, _)| k.starts_with(&partial))
            .map(|(k, detail)| CompletionItem {
                label: k.clone(),
                kind: Some(if detail == "mapping" {
                    CompletionItemKind::MODULE
                } else {
                    CompletionItemKind::FIELD
                }),
                detail: Some(detail),
                insert_text: Some(k),
                ..Default::default()
            })
            .collect();
        return Some(CompletionResponse::Array(items));
    }

    // `classes:` list entry → class names.
    let index = YamlIndex::parse(&text);
    let in_classes_value = index
        .hit(Pos { line: pos.line, col: pos.character.saturating_sub(1) })
        .is_some_and(|h| matches!(h, Hit::Value(e, _) if e.path.0.first().and_then(Key::as_str) == Some("classes")));
    // A fresh `- ` item has no scalar yet: look at the enclosing top-level key.
    let lines: Vec<&str> = text.lines().collect();
    let in_new_item = before.trim_start().starts_with('-')
        && lines[..(pos.line as usize).min(lines.len())]
            .iter()
            .rev()
            .find(|l| !l.starts_with(' ') && !l.starts_with('-') && !l.trim().is_empty())
            .is_some_and(|l| l.starts_with("classes:"));
    let in_classes = in_classes_value || in_new_item;
    if in_classes {
        let items = st
            .backend
            .class_usage()
            .into_iter()
            .filter(|u| !u.name.is_empty())
            .map(|u| CompletionItem {
                label: u.name.clone(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(format!(
                    "{} · {} target{}",
                    rel(&u.file, &st.repo_root),
                    u.targets,
                    if u.targets == 1 { "" } else { "s" }
                )),
                ..Default::default()
            })
            .collect();
        return Some(CompletionResponse::Array(items));
    }
    None
}

fn kind_of(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(_) => "mapping".into(),
        serde_json::Value::Array(a) => format!("list ({} items)", a.len()),
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(_) => "bool".into(),
        serde_json::Value::Number(_) => "number".into(),
        serde_json::Value::String(s) => {
            let one = s.lines().next().unwrap_or("");
            if one.chars().count() > 40 {
                format!("{}…", one.chars().take(40).collect::<String>())
            } else {
                one.to_string()
            }
        }
    }
}

// ---- diagnostics ------------------------------------------------------------

fn publish_diagnostics(
    sender: &crossbeam_channel::Sender<Message>,
    backend: &Backend,
    published: &Mutex<HashSet<Url>>,
    st: &State,
) {
    let Some(result) = backend.diagnostics() else {
        return;
    };
    let target_files: HashMap<String, PathBuf> = backend
        .targets()
        .into_iter()
        .map(|t| (t.name, t.file))
        .collect();
    let mut by_uri = group_by_uri(
        result
            .errors
            .iter()
            .map(|d| (d, DiagnosticSeverity::ERROR))
            .chain(
                result
                    .warnings
                    .iter()
                    .map(|d| (d, DiagnosticSeverity::WARNING)),
            )
            .filter_map(|(d, severity)| to_lsp_diagnostic(d, severity, &target_files, st)),
    );
    let mut prev = published.lock();
    for uri in prev.iter() {
        by_uri.entry(uri.clone()).or_default();
    }
    *prev = by_uri
        .keys()
        .filter(|u| !by_uri[*u].is_empty())
        .cloned()
        .collect();
    for (uri, diagnostics) in by_uri {
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics,
            version: None,
        };
        let _ = sender.send(Message::Notification(Notification::new(
            PublishDiagnostics::METHOD.to_string(),
            params,
        )));
    }
}

fn group_by_uri(
    diags: impl Iterator<Item = (Url, lsp_types::Diagnostic)>,
) -> HashMap<Url, Vec<lsp_types::Diagnostic>> {
    let mut by_uri: HashMap<Url, Vec<lsp_types::Diagnostic>> = HashMap::new();
    for (uri, diag) in diags {
        let list = by_uri.entry(uri).or_default();
        // Every target that includes a broken class reports the same error there.
        let same = list
            .iter_mut()
            .find(|d| d.range == diag.range && d.code == diag.code && d.message == diag.message);
        match same {
            Some(d) => {
                // "krab (a)" + "krab (b)" -> "krab (a, b)"
                let target = diag
                    .source
                    .as_deref()
                    .and_then(|s| s.strip_prefix("krab ("));
                if let (Some(src), Some(t)) = (d.source.as_mut(), target)
                    && src.ends_with(')')
                {
                    src.pop();
                    src.push_str(", ");
                    src.push_str(t);
                }
            }
            None => list.push(diag),
        }
    }
    by_uri
}

fn to_lsp_diagnostic(
    d: &KDiagnostic,
    severity: DiagnosticSeverity,
    target_files: &HashMap<String, PathBuf>,
    st: &State,
) -> Option<(Url, lsp_types::Diagnostic)> {
    let mut labels = d.labels.iter().filter(|l| l.location.is_some());
    let (file, line, col, primary_text) = match labels.next() {
        Some(l) => {
            let loc = l.location.as_ref().unwrap();
            (loc.file.clone(), loc.line, loc.col, Some(l.text.clone()))
        }
        None => {
            let f = d.target.as_ref().and_then(|t| target_files.get(t))?.clone();
            (f, 1, 1, None)
        }
    };
    let uri = Url::from_file_path(&file).ok()?;
    let line0 = line.saturating_sub(1);
    let col0 = col.saturating_sub(1);
    let line_len = st
        .docs
        .get(&uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&file).ok())
        .and_then(|t| {
            t.lines()
                .nth(line0 as usize)
                .map(|l| l.chars().count() as u32)
        })
        .unwrap_or(col0 + 1);
    let range = Range {
        start: Position {
            line: line0,
            character: col0,
        },
        end: Position {
            line: line0,
            character: line_len.max(col0 + 1),
        },
    };
    let mut message = d.message.clone();
    if let Some(t) = &primary_text
        && t != "here"
    {
        message.push_str(&format!(" ({t})"));
    }
    if let Some(h) = &d.help {
        message.push_str(&format!("\n{h}"));
    }
    let related: Vec<DiagnosticRelatedInformation> = labels
        .filter_map(|l| {
            let loc = l.location.as_ref()?;
            Some(DiagnosticRelatedInformation {
                location: to_lsp_location(loc)?,
                message: l.text.clone(),
            })
        })
        .collect();
    Some((
        uri,
        lsp_types::Diagnostic {
            range,
            severity: Some(severity),
            code: Some(NumberOrString::String(d.code.to_string())),
            source: Some(match d.target {
                Some(ref t) => format!("krab ({t})"),
                None => "krab".into(),
            }),
            message,
            related_information: if related.is_empty() {
                None
            } else {
                Some(related)
            },
            ..Default::default()
        },
    ))
}

fn rel(p: &Path, root: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .to_string()
}

fn rel_loc(l: &KLocation, root: &Path) -> String {
    format!("{}:{}:{}", rel(&l.file, root), l.line, l.col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_diagnostic_per_location_and_message() {
        let uri = Url::parse("file:///inv/classes/common.yml").unwrap();
        let diag = |target: &str| {
            (
                uri.clone(),
                lsp_types::Diagnostic {
                    range: Range::new(Position::new(2, 3), Position::new(2, 20)),
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: Some(NumberOrString::String("E1".into())),
                    source: Some(format!("krab ({target})")),
                    message: "interpolation failed".into(),
                    ..Default::default()
                },
            )
        };
        let other = (
            uri.clone(),
            lsp_types::Diagnostic {
                message: "another error".into(),
                ..diag("a").1
            },
        );
        let by_uri = group_by_uri([diag("a"), diag("b"), other, diag("c")].into_iter());
        let got = &by_uri[&uri];
        assert_eq!(got.len(), 2, "{got:#?}");
        assert_eq!(got[0].message, "interpolation failed");
        assert_eq!(got[0].source.as_deref(), Some("krab (a, b, c)"));
        assert_eq!(got[1].message, "another error");
        assert_eq!(got[1].source.as_deref(), Some("krab (a)"));
    }
}
