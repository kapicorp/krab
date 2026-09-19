//! Dependency fetching (`parameters.kapitan.dependencies`): git repositories,
//! http(s) files and helm charts land in their `output_path` before the
//! targets compile, so the inputs that read them see the files and the
//! manifest records them like any other read.
//!
//! Semantics follow kapitan's `dependency_manager`: a dependency is only
//! written where nothing exists yet unless it is forced (`--force-fetch`, or
//! `force_fetch: true` on the item), in which case existing files are
//! overwritten. One difference: a dependency whose output path already
//! exists is not fetched at all (kapitan does the same for helm charts, and
//! for git and http would only add files that are missing), so a compile
//! with `fetch: true` in `.kapitan` stays offline once everything is there.
//! OCI artifacts (`type: oci`) are pulled by `oci.rs` the way oras does.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::inputs::helm::{helm_binary, run_helm};
use crate::oci::{self, TlsVerify};

pub const DEPENDENCIES_PATH: &str = "parameters.kapitan.dependencies";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    Git {
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        submodules: bool,
    },
    Http {
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        unpack: bool,
    },
    Helm {
        chart_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        helm_path: Option<String>,
    },
    /// An OCI artifact (what `oras push` produces).
    Oci {
        #[serde(skip_serializing_if = "Option::is_none")]
        subpath: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        insecure: bool,
        tls_verify: TlsVerify,
    },
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Git { .. } => "git",
            Kind::Http { .. } => "http",
            Kind::Helm { .. } => "helm",
            Kind::Oci { .. } => "oci",
        }
    }
}

/// One `dependencies` item as a target declares it.
#[derive(Clone, Debug, Serialize)]
pub struct Dependency {
    /// The target that declares it.
    pub target: String,
    #[serde(flatten)]
    pub kind: Kind,
    pub source: String,
    /// `output_path` as written in the inventory.
    pub output_path: String,
    /// Where it lands: `output_path` under the compile output directory, normalised.
    #[serde(skip)]
    pub dest: PathBuf,
    pub force_fetch: bool,
}

/// The `dependencies` of one target's rendered document.
pub fn dependencies(
    target: &str,
    doc: &Json,
    output_root: &Path,
) -> Result<Vec<Dependency>, String> {
    let items = match doc.as_array() {
        Some(a) => a,
        None if doc.is_null() => return Ok(vec![]),
        None => {
            return Err(format!(
                "target {target}: {DEPENDENCIES_PATH} must be a list"
            ));
        }
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let s = |k: &str| item.get(k).and_then(Json::as_str).map(str::to_string);
            let b = |k: &str| item.get(k).and_then(Json::as_bool).unwrap_or(false);
            let required = |k: &str| {
                s(k).ok_or_else(|| {
                    format!("target {target}: {DEPENDENCIES_PATH}[{i}] has no `{k}`")
                })
            };
            let ty = required("type")?;
            let kind = match ty.as_str() {
                "git" => Kind::Git {
                    git_ref: s("ref"),
                    subdir: s("subdir"),
                    submodules: b("submodules"),
                },
                "http" | "https" => Kind::Http { unpack: b("unpack") },
                "helm" => Kind::Helm {
                    chart_name: required("chart_name")?,
                    version: s("version"),
                    helm_path: s("helm_path"),
                },
                "oci" => {
                    let source = required("source")?;
                    for prefix in ["https://", "http://", "oci://"] {
                        if source.starts_with(prefix) {
                            return Err(format!(
                                "target {target}: {DEPENDENCIES_PATH}[{i}]: OCI source must be a bare registry reference (e.g. 'ghcr.io/org/repo:tag'), not a URL. Remove the '{prefix}' prefix."
                            ));
                        }
                    }
                    if let Some(mt) = s("media_type")
                        && !mt.contains('/')
                    {
                        return Err(format!(
                            "target {target}: {DEPENDENCIES_PATH}[{i}]: media_type '{mt}' is not a valid MIME type (expected 'type/subtype')"
                        ));
                    }
                    Kind::Oci {
                        subpath: s("subpath"),
                        media_type: s("media_type"),
                        insecure: b("insecure"),
                        tls_verify: match item.get("tls_verify") {
                            Some(Json::Bool(v)) => TlsVerify::Bool(*v),
                            Some(Json::String(path)) => TlsVerify::CaBundle(path.clone()),
                            _ => TlsVerify::Bool(true),
                        },
                    }
                }
                other => {
                    return Err(format!(
                        "target {target}: {DEPENDENCIES_PATH}[{i}] has unknown type `{other}` (git, http, https, helm, oci)"
                    ));
                }
            };
            let output_path = required("output_path")?;
            Ok(Dependency {
                target: target.to_string(),
                kind,
                source: required("source")?,
                dest: normalise_join(output_root, &output_path),
                output_path,
                force_fetch: b("force_fetch"),
            })
        })
        .collect()
}

/// kapitan `normalise_join_path`: `os.path.normpath(os.path.join(root, path))`.
pub fn normalise_join(root: &Path, path: &str) -> PathBuf {
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            c => out.push(c.as_os_str()),
        }
    }
    out
}

pub struct FetchOptions<'a> {
    pub repo_root: &'a Path,
    /// Every dependency is considered (`--fetch`); otherwise only items
    /// with `force_fetch: true`.
    pub fetch_all: bool,
    /// Overwrite what exists (`--force-fetch`).
    pub force: bool,
    pub dry_run: bool,
    pub parallelism: usize,
    /// Where versioned helm charts are kept across runs
    /// (`$XDG_CACHE_HOME/kapitan`).
    pub cache_dir: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FetchStatus {
    Fetched { ms: u64 },
    WouldFetch,
    Skipped,
    Failed { error: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct FetchOutcome {
    #[serde(rename = "type")]
    pub kind: String,
    pub source: String,
    /// Where it landed, relative to the repository root when it is inside it.
    pub output_path: String,
    pub target: String,
    #[serde(flatten)]
    pub status: FetchStatus,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl FetchOutcome {
    pub fn failed(&self) -> bool {
        matches!(self.status, FetchStatus::Failed { .. })
    }
}

/// A dependency that will be fetched, with the effective force flag.
#[derive(Clone, Debug)]
struct Wanted {
    dep: Dependency,
    force: bool,
    reason: String,
}

/// Fetch every dependency that needs it. Duplicates (same source and
/// destination, declared by several targets) are fetched once; sources are
/// fetched once per run and copied to each destination; distinct sources
/// are fetched in parallel.
pub fn fetch(deps: Vec<Dependency>, opts: &FetchOptions) -> Vec<FetchOutcome> {
    let mut outcomes = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, PathBuf)> = Default::default();
    // Groups keyed like kapitan: the source (git, http) or the chart identity (helm).
    let mut groups: BTreeMap<String, Vec<Wanted>> = BTreeMap::new();
    for dep in deps {
        if !seen.insert((dep.source.clone(), dep.dest.clone())) {
            continue;
        }
        if !opts.fetch_all && !dep.force_fetch {
            continue;
        }
        let reason = if opts.force {
            "forced (--force-fetch)".to_string()
        } else if dep.force_fetch {
            "forced (force_fetch: true)".to_string()
        } else if dep.dest.symlink_metadata().is_err() {
            format!("{} missing", display_path(opts.repo_root, &dep.dest))
        } else {
            outcomes.push(outcome(
                opts.repo_root,
                &dep,
                FetchStatus::Skipped,
                "already present",
            ));
            continue;
        };
        let force = opts.force || dep.force_fetch;
        let key = match &dep.kind {
            Kind::Helm {
                chart_name,
                version,
                helm_path,
            } => format!(
                "helm\0{}\0{chart_name}\0{}\0{}",
                dep.source,
                version.as_deref().unwrap_or(""),
                helm_path.as_deref().unwrap_or("")
            ),
            k => format!("{}\0{}", k.name(), dep.source),
        };
        groups
            .entry(key)
            .or_default()
            .push(Wanted { dep, force, reason });
    }

    if opts.dry_run {
        for w in groups.into_values().flatten() {
            outcomes.push(outcome(
                opts.repo_root,
                &w.dep,
                FetchStatus::WouldFetch,
                &w.reason,
            ));
        }
        return outcomes;
    }
    if groups.is_empty() {
        return outcomes;
    }

    let save_dir = std::env::temp_dir().join(format!(
        "kapitan-fetch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if let Err(e) = std::fs::create_dir_all(&save_dir) {
        for w in groups.into_values().flatten() {
            outcomes.push(outcome(
                opts.repo_root,
                &w.dep,
                FetchStatus::Failed {
                    error: format!("cannot create {}: {e}", save_dir.display()),
                },
                &w.reason,
            ));
        }
        return outcomes;
    }
    let counter = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.parallelism.max(1))
        .build();
    let run = || -> Vec<FetchOutcome> {
        groups
            .par_iter()
            .flat_map_iter(|(_, wanted)| fetch_group(wanted, &save_dir, opts, &counter))
            .collect()
    };
    let mut fetched = match pool {
        Ok(pool) => pool.install(run),
        Err(_) => run(),
    };
    let _ = std::fs::remove_dir_all(&save_dir);
    outcomes.append(&mut fetched);
    outcomes
}

fn outcome(root: &Path, dep: &Dependency, status: FetchStatus, reason: &str) -> FetchOutcome {
    FetchOutcome {
        kind: dep.kind.name().to_string(),
        source: dep.source.clone(),
        output_path: display_path(root, &dep.dest),
        target: dep.target.clone(),
        status,
        reason: reason.to_string(),
        warnings: vec![],
    }
}

fn display_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// One source, every destination that wants it.
fn fetch_group(
    wanted: &[Wanted],
    save_dir: &Path,
    opts: &FetchOptions,
    counter: &AtomicUsize,
) -> Vec<FetchOutcome> {
    let started = Instant::now();
    let first = &wanted[0].dep;
    let results: Vec<Result<Vec<String>, String>> = match &first.kind {
        Kind::Git { .. } => fetch_git(wanted, save_dir),
        Kind::Http { .. } => fetch_http(wanted, save_dir, counter),
        Kind::Helm { .. } => fetch_helm(wanted, save_dir, opts, counter),
        Kind::Oci { .. } => fetch_oci(wanted, save_dir),
    };
    let ms = started.elapsed().as_millis() as u64;
    wanted
        .iter()
        .zip(results)
        .map(|(w, r)| {
            let (status, warnings) = match r {
                Ok(warnings) => (FetchStatus::Fetched { ms }, warnings),
                Err(error) => (FetchStatus::Failed { error }, vec![]),
            };
            let mut o = outcome(opts.repo_root, &w.dep, status, &w.reason);
            o.warnings = warnings;
            o
        })
        .collect()
}

/// Results for every destination when the shared step succeeded.
type GroupResults = Vec<Result<Vec<String>, String>>;

/// Results for every destination; a failure of the shared step fails all of them.
fn all_failed(wanted: &[Wanted], e: String) -> GroupResults {
    wanted.iter().map(|_| Err(e.clone())).collect()
}

fn hash8(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))[..8].to_string()
}

/// `os.path.dirname` / `os.path.basename` of a URL-ish source.
fn split_source(source: &str) -> (&str, &str) {
    match source.rsplit_once('/') {
        Some((dir, base)) => (dir, base),
        None => ("", source),
    }
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git binary not found. git must be present in the PATH to fetch git dependencies"
                .to_string()
        } else {
            format!("cannot run git: {e}")
        }
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// kapitan `fetch_git_dependency`: clone once, then per destination check
/// out the ref (the remote's default branch when none is given), update
/// submodules if asked, and copy the repository or its `subdir`.
fn fetch_git(wanted: &[Wanted], save_dir: &Path) -> GroupResults {
    let source = &wanted[0].dep.source;
    let (dir, base) = split_source(source);
    let clone = save_dir.join(format!("{}{base}", hash8(dir)));
    let _ = std::fs::remove_dir_all(&clone);
    if let Err(e) = git(&["clone", source, &clone.to_string_lossy()], None) {
        return all_failed(
            wanted,
            format!("Dependency {source}: fetching unsuccessful\n{e}"),
        );
    }
    let default_branch = git(&["symbolic-ref", "--short", "HEAD"], Some(&clone))
        .ok()
        .or_else(|| {
            git(&["symbolic-ref", "refs/remotes/origin/HEAD"], Some(&clone))
                .ok()
                .and_then(|r| r.rsplit('/').next().map(str::to_string))
        });
    wanted
        .iter()
        .map(|w| {
            let Kind::Git {
                git_ref,
                subdir,
                submodules,
            } = &w.dep.kind
            else {
                unreachable!()
            };
            if let Some(r) = git_ref.as_deref().or(default_branch.as_deref()) {
                git(&["checkout", "--quiet", r], Some(&clone))
                    .map_err(|e| format!("Dependency {source}: cannot check out `{r}`\n{e}"))?;
            }
            if *submodules {
                git(&["submodule", "update", "--init"], Some(&clone))
                    .map_err(|e| format!("Dependency {source}: submodule update failed\n{e}"))?;
            }
            let src = match subdir {
                Some(sub) => {
                    let full = clone.join(sub);
                    if !full.is_dir() {
                        return Err(format!(
                            "Dependency {source}: subdir {sub} not found in repo"
                        ));
                    }
                    full
                }
                None => clone.clone(),
            };
            if w.force {
                copy_tree(&src, &w.dep.dest)
            } else {
                safe_copy_tree(&src, &w.dep.dest)
            }
            .map(|_| vec![])
            .map_err(|e| {
                format!(
                    "Dependency {source}: cannot copy to {}: {e}",
                    w.dep.dest.display()
                )
            })
        })
        .collect()
}

fn download(source: &str) -> Result<(Vec<u8>, Option<String>), String> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .http_status_as_error(true)
        .user_agent(format!("kapitan/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let mut resp = agent.get(source).call().map_err(|e| e.to_string())?;
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok((bytes, content_type))
}

/// kapitan `fetch_http_dependency`: download once, then per destination
/// either unpack the archive into it or save the file there.
fn fetch_http(wanted: &[Wanted], save_dir: &Path, counter: &AtomicUsize) -> GroupResults {
    let source = &wanted[0].dep.source;
    let (dir, base) = split_source(source);
    let file = save_dir.join(format!("{}{base}", hash8(dir)));
    let content_type = match download(source) {
        Ok((bytes, ct)) => match std::fs::write(&file, bytes) {
            Ok(()) => ct,
            Err(e) => {
                return all_failed(
                    wanted,
                    format!("Dependency {source}: cannot save download: {e}"),
                );
            }
        },
        Err(e) => {
            return all_failed(
                wanted,
                format!("Dependency {source}: fetching unsuccessful\n{e}"),
            );
        }
    };
    wanted
        .iter()
        .map(|w| {
            let Kind::Http { unpack } = &w.dep.kind else {
                unreachable!()
            };
            let dest = &w.dep.dest;
            if *unpack {
                std::fs::create_dir_all(dest).map_err(|e| {
                    format!("Dependency {source}: cannot create {}: {e}", dest.display())
                })?;
                let unpacked = if w.force {
                    unpack_file(&file, dest, content_type.as_deref())?
                } else {
                    let tmp = save_dir.join(format!(
                        "extracted-{}",
                        counter.fetch_add(1, Ordering::Relaxed)
                    ));
                    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
                    let ok = unpack_file(&file, &tmp, content_type.as_deref())?;
                    if ok {
                        safe_copy_tree(&tmp, dest).map_err(|e| {
                            format!(
                                "Dependency {source}: cannot copy to {}: {e}",
                                dest.display()
                            )
                        })?;
                    }
                    let _ = std::fs::remove_dir_all(&tmp);
                    ok
                };
                if !unpacked {
                    return Err(format!(
                        "Dependency {source}: Content-Type {} is not supported for unpack",
                        content_type.as_deref().unwrap_or("unknown")
                    ));
                }
                Ok(vec![])
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!(
                            "Dependency {source}: cannot create {}: {e}",
                            parent.display()
                        )
                    })?;
                }
                if w.force || !dest.is_file() {
                    std::fs::copy(&file, dest).map_err(|e| {
                        format!("Dependency {source}: cannot write {}: {e}", dest.display())
                    })?;
                }
                Ok(vec![])
            }
        })
        .collect()
}

/// kapitan `unpack_downloaded_file`: tar, zip and gzipped tar by content
/// type (with the archive's magic bytes deciding when the type is generic).
/// `Ok(false)` when the type is not one that can be unpacked.
pub fn unpack_file(file: &Path, dest: &Path, content_type: Option<&str>) -> Result<bool, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let is_zip = bytes.starts_with(b"PK\x03\x04");
    let is_gzip = bytes.starts_with(&[0x1f, 0x8b]);
    let name = file.to_string_lossy();
    let content_type = match content_type {
        None | Some("application/octet-stream") if is_zip => "application/zip",
        Some(ct) => ct,
        None => "",
    };
    let untar = |gz: bool| -> Result<(), String> {
        let mut archive: tar::Archive<Box<dyn Read>> = if gz {
            tar::Archive::new(Box::new(flate2::read::GzDecoder::new(&bytes[..])))
        } else {
            tar::Archive::new(Box::new(&bytes[..]))
        };
        archive.set_overwrite(true);
        archive
            .unpack(dest)
            .map_err(|e| format!("cannot unpack {name}: {e}"))
    };
    match content_type {
        "application/x-tar" => {
            untar(is_gzip)?;
            Ok(true)
        }
        "application/zip" => {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..]))
                .map_err(|e| format!("cannot open {name}: {e}"))?;
            zip.extract(dest)
                .map_err(|e| format!("cannot unpack {name}: {e}"))?;
            Ok(true)
        }
        "application/gzip"
        | "application/octet-stream"
        | "application/x-gzip"
        | "application/x-compressed"
        | "application/x-compressed-tar" => {
            if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
                untar(is_gzip)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        _ => Ok(false),
    }
}

/// kapitan `fetch_helm_chart`: `helm pull --untar` once per chart identity,
/// then copy the chart directory to each destination. Charts with a version
/// are kept under `$XDG_CACHE_HOME/kapitan/charts` (a published version is
/// immutable); forced fetches pull again.
fn fetch_helm(
    wanted: &[Wanted],
    save_dir: &Path,
    opts: &FetchOptions,
    counter: &AtomicUsize,
) -> GroupResults {
    let first = &wanted[0];
    let Kind::Helm {
        chart_name,
        version,
        helm_path,
    } = &first.dep.kind
    else {
        unreachable!()
    };
    let repo = &first.dep.source;
    let label = format!(
        "Dependency helm chart {chart_name} and version {}",
        version.as_deref().unwrap_or("latest")
    );
    let cached = match version {
        Some(v) => opts
            .cache_dir
            .join("charts")
            .join(hash8(repo))
            .join(format!("{chart_name}-{v}")),
        None => save_dir
            .join(hash8(repo))
            .join(format!("{chart_name}-latest")),
    };
    let force = wanted.iter().any(|w| w.force);
    if force || !cached.is_dir() {
        let tmp = save_dir.join(format!(
            "helm-{}-{}",
            hash8(repo),
            counter.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(e) = std::fs::create_dir_all(&tmp) {
            return all_failed(
                wanted,
                format!("{label}: cannot create {}: {e}", tmp.display()),
            );
        }
        let mut args = vec![
            "pull".to_string(),
            "--destination".to_string(),
            tmp.to_string_lossy().into_owned(),
            "--untar".to_string(),
        ];
        if let Some(v) = version {
            args.push("--version".into());
            args.push(v.clone());
        }
        if repo.starts_with("oci://") {
            args.push(repo.clone());
        } else {
            args.push("--repo".into());
            args.push(repo.clone());
            args.push(chart_name.clone());
        }
        let binary = helm_binary(helm_path.as_deref());
        if let Err(e) = run_helm(&binary, &args, opts.repo_root) {
            return all_failed(wanted, format!("{label}: {}", e.trim()));
        }
        let pulled = tmp.join(chart_name);
        if !pulled.is_dir() {
            return all_failed(
                wanted,
                format!(
                    "{label}: helm pull produced no directory named {chart_name} (is chart_name the chart's name?)"
                ),
            );
        }
        let _ = std::fs::remove_dir_all(&cached);
        if let Some(parent) = cached.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return all_failed(
                wanted,
                format!("{label}: cannot create {}: {e}", parent.display()),
            );
        }
        if std::fs::rename(&pulled, &cached).is_err()
            && let Err(e) = copy_tree(&pulled, &cached)
        {
            return all_failed(wanted, format!("{label}: cannot cache chart: {e}"));
        }
    }
    wanted
        .iter()
        .map(|w| {
            let dest = &w.dep.dest;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{label}: cannot create {}: {e}", parent.display()))?;
            }
            if w.force {
                copy_tree(&cached, dest)
            } else {
                safe_copy_tree(&cached, dest)
            }
            .map(|_| vec![])
            .map_err(|e| format!("{label}: cannot copy to {}: {e}", dest.display()))
        })
        .collect()
}

/// kapitan `fetch_oci_dependency`: pull the artifact once (the layers every
/// destination asks for), extract the tar blobs in it, then copy the
/// artifact or the declared `subpath` to each destination.
fn fetch_oci(wanted: &[Wanted], save_dir: &Path) -> GroupResults {
    let source = &wanted[0].dep.source;
    let settings: Vec<(bool, &TlsVerify, Option<&String>)> = wanted
        .iter()
        .map(|w| match &w.dep.kind {
            Kind::Oci {
                insecure,
                tls_verify,
                media_type,
                ..
            } => (*insecure, tls_verify, media_type.as_ref()),
            _ => unreachable!(),
        })
        .collect();
    let (insecure, tls_verify, _) = settings[0];
    if settings
        .iter()
        .any(|(i, t, _)| *i != insecure || *t != tls_verify)
    {
        return all_failed(
            wanted,
            format!(
                "Dependency {source}: multiple dependencies share the same source but declare conflicting connection settings. All dependencies for the same source must use identical insecure and tls_verify settings."
            ),
        );
    }
    // Union of the media type filters: any destination wanting everything wins.
    let mut allowed: Option<Vec<String>> = Some(vec![]);
    for (_, _, mt) in &settings {
        match (mt, &mut allowed) {
            (None, _) => allowed = None,
            (Some(mt), Some(list)) if !list.contains(mt) => list.push((*mt).clone()),
            _ => {}
        }
    }
    let credentials = match (std::env::var("OCI_USERNAME"), std::env::var("OCI_PASSWORD")) {
        (Ok(u), Ok(p)) if !u.is_empty() && !p.is_empty() => Some((u, p)),
        _ => None,
    };
    let target_dir = save_dir.join(format!("oci_{}", hash8(source)));
    let _ = std::fs::remove_dir_all(&target_dir);
    if let Err(e) = oci::pull(
        source,
        &target_dir,
        &oci::PullOptions {
            insecure,
            tls_verify,
            allowed_media_types: allowed.as_deref(),
            credentials,
        },
    ) {
        return all_failed(
            wanted,
            format!(
                "Dependency {source}: fetching unsuccessful
{e}"
            ),
        );
    }
    if let Err(e) = extract_tar_blobs(&target_dir) {
        return all_failed(
            wanted,
            format!(
                "Dependency {source}: failed to extract tar blobs from pulled artifact
{e}"
            ),
        );
    }
    wanted
        .iter()
        .map(|w| {
            let Kind::Oci { subpath, .. } = &w.dep.kind else {
                unreachable!()
            };
            let src = match subpath {
                Some(sub) => {
                    let full = oci::safe_join(&target_dir, sub).ok_or_else(|| {
                        format!(
                            "Dependency {source}: subpath '{sub}' resolves outside the artifact directory"
                        )
                    })?;
                    if !full.is_dir() {
                        return Err(format!(
                            "Dependency {source}: subpath '{sub}' not found in pulled artifact"
                        ));
                    }
                    full
                }
                None => target_dir.clone(),
            };
            let dest = &w.dep.dest;
            if w.force {
                copy_tree(&src, dest)
            } else {
                safe_copy_tree(&src, dest)
            }
            .map_err(|e| format!("Dependency {source}: cannot copy to {}: {e}", dest.display()))?;
            // An artifact pushed from a parent directory lands one level deep.
            let mut warnings = vec![];
            if subpath.is_none()
                && let Ok(rd) = std::fs::read_dir(dest)
            {
                let children: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                    .collect();
                let mut dirs: Vec<String> = children
                    .iter()
                    .filter(|p| p.is_dir())
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                dirs.sort();
                if !dirs.is_empty() && !children.iter().any(|p| p.is_file()) {
                    warnings.push(format!(
                        "Dependency {source}: output_path '{}' contains only subdirectories: [{}]. The artifact may have been pushed with nested paths; set 'subpath' to the directory that contains your content (e.g. subpath: {})",
                        dest.display(),
                        dirs.join(", "),
                        dirs[0]
                    ));
                }
            }
            Ok(warnings)
        })
        .collect()
}

/// kapitan `_extract_tar_blobs`: oras saves each layer as a file; layers
/// that are tar archives (gzipped or not) are extracted into `dir` and
/// the archive removed, so the tree matches what was pushed.
fn extract_tar_blobs(dir: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_files(dir, &mut files);
    for file in files {
        let bytes =
            std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let Some(tar) = tar_bytes(&bytes) else {
            continue;
        };
        let mut archive = tar::Archive::new(&tar[..]);
        archive.set_overwrite(true);
        std::fs::remove_file(&file)
            .map_err(|e| format!("cannot remove {}: {e}", file.display()))?;
        // A blob titled `a/b` leaves `a/` behind; drop such empty directories.
        let mut parent = file.parent();
        while let Some(p) = parent
            && p != dir
            && std::fs::remove_dir(p).is_ok()
        {
            parent = p.parent();
        }
        archive
            .unpack(dir)
            .map_err(|e| format!("cannot extract {}: {e}", file.display()))?;
    }
    Ok(())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let p = entry.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The uncompressed bytes when `bytes` is a tar archive, gzipped or plain
/// (`tarfile.is_tarfile` accepts both).
fn tar_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    let plain: Vec<u8> = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .ok()?;
        out
    } else {
        bytes.to_vec()
    };
    let is_tar = plain.len() >= 512
        && (plain[257..262] == *b"ustar"
            || tar::Archive::new(&plain[..])
                .entries()
                .ok()?
                .next()?
                .is_ok());
    is_tar.then_some(plain)
}

/// kapitan `safe_copy_tree`: copy `src` into `dst` without overwriting any
/// existing file and without copying entries whose name starts with `.`.
/// Returns how many files were copied.
pub fn safe_copy_tree(src: &Path, dst: &Path) -> std::io::Result<usize> {
    if !src.is_dir() {
        return Err(std::io::Error::other(format!(
            "Cannot copy tree {}: not a directory",
            src.display()
        )));
    }
    std::fs::create_dir_all(dst)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copied += safe_copy_tree(&from, &to)?;
        } else if !to.is_file() {
            std::fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// kapitan `copy_tree(clobber_files=True)`: copy everything, dot-entries
/// included, replacing existing files. Returns how many files were copied.
pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<usize> {
    if !src.is_dir() {
        return Err(std::io::Error::other(format!(
            "Cannot copy tree {}: not a directory",
            src.display()
        )));
    }
    if dst.exists() && !dst.is_dir() {
        return Err(std::io::Error::other(format!(
            "Cannot copy tree to {}: destination exists but not a directory",
            dst.display()
        )));
    }
    std::fs::create_dir_all(dst)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copied += copy_tree(&from, &to)?;
        } else {
            if to.is_file() {
                // Read-only files (git pack files) cannot be overwritten in place.
                std::fs::remove_file(&to)?;
            }
            std::fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kapitan-fetch-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn opts(root: &Path, fetch_all: bool, force: bool) -> FetchOptions<'_> {
        FetchOptions {
            repo_root: root,
            fetch_all,
            force,
            dry_run: false,
            parallelism: 2,
            cache_dir: root.join(".cache"),
        }
    }

    #[test]
    fn parses_every_kind_and_normalises_paths() {
        let root = Path::new("/repo");
        let deps = dependencies(
            "t",
            &json!([
                {"type": "git", "source": "https://x/y.git", "output_path": "system/lib/", "ref": "main", "subdir": "lib/", "force_fetch": true},
                {"type": "https", "source": "https://x/f.tgz", "output_path": "./a/../b/c", "unpack": true},
                {"type": "helm", "source": "https://charts", "output_path": "charts/x/1.0", "chart_name": "x", "version": "1.0"},
                {"type": "oci", "source": "ghcr.io/a/b:1", "output_path": "oci"},
            ]),
            root,
        )
        .unwrap();
        assert_eq!(deps.len(), 4);
        assert_eq!(
            deps[0].kind,
            Kind::Git {
                git_ref: Some("main".into()),
                subdir: Some("lib/".into()),
                submodules: false
            }
        );
        assert!(deps[0].force_fetch);
        assert_eq!(deps[0].dest, PathBuf::from("/repo/system/lib"));
        assert_eq!(deps[1].kind, Kind::Http { unpack: true });
        assert_eq!(deps[1].dest, PathBuf::from("/repo/b/c"));
        assert_eq!(
            deps[2].kind,
            Kind::Helm {
                chart_name: "x".into(),
                version: Some("1.0".into()),
                helm_path: None
            }
        );
        assert_eq!(
            deps[3].kind,
            Kind::Oci {
                subpath: None,
                media_type: None,
                insecure: false,
                tls_verify: TlsVerify::Bool(true)
            }
        );
        assert!(
            dependencies(
                "t",
                &json!([{"type": "oci", "source": "oci://ghcr.io/a/b", "output_path": "o"}]),
                root
            )
            .unwrap_err()
            .contains("Remove the 'oci://' prefix")
        );
        assert!(
            dependencies("t", &json!([{"type": "oci", "source": "ghcr.io/a/b", "output_path": "o", "media_type": "tar"}]), root)
                .unwrap_err()
                .contains("not a valid MIME type")
        );
        assert!(dependencies("t", &json!(null), root).unwrap().is_empty());
        assert!(
            dependencies(
                "t",
                &json!([{"type": "svn", "source": "s", "output_path": "o"}]),
                root
            )
            .unwrap_err()
            .contains("unknown type `svn`")
        );
        assert!(
            dependencies(
                "t",
                &json!([{"type": "helm", "source": "s", "output_path": "o"}]),
                root
            )
            .unwrap_err()
            .contains("no `chart_name`")
        );
    }

    #[test]
    fn normalise_join_matches_normpath() {
        assert_eq!(
            normalise_join(Path::new("/r"), "a/./b/"),
            PathBuf::from("/r/a/b")
        );
        assert_eq!(normalise_join(Path::new("/r"), "../x"), PathBuf::from("/x"));
        assert_eq!(
            normalise_join(Path::new("/r"), "/abs/p"),
            PathBuf::from("/abs/p")
        );
        assert_eq!(
            normalise_join(Path::new("r"), "../../x"),
            PathBuf::from("../x")
        );
    }

    #[test]
    fn safe_copy_never_overwrites_and_skips_dotfiles() {
        let dir = tmp("safe-copy");
        let (src, dst) = (dir.join("src"), dir.join("dst"));
        write(&src.join("a.txt"), "new");
        write(&src.join("sub/b.txt"), "b");
        write(&src.join(".git/HEAD"), "ref");
        write(&dst.join("a.txt"), "old");
        assert_eq!(safe_copy_tree(&src, &dst).unwrap(), 1);
        assert_eq!(read(&dst.join("a.txt")), "old");
        assert_eq!(read(&dst.join("sub/b.txt")), "b");
        assert!(!dst.join(".git").exists());

        assert_eq!(copy_tree(&src, &dst).unwrap(), 3);
        assert_eq!(read(&dst.join("a.txt")), "new");
        assert!(dst.join(".git/HEAD").exists());
        assert!(copy_tree(&src.join("a.txt"), &dst).is_err());
    }

    fn tar_gz(files: &[(&str, &str)]) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        for (name, text) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(text.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, name, text.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, text) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(text.as_bytes()).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    #[test]
    fn unpack_dispatches_on_content_type_and_magic() {
        let dir = tmp("unpack");
        let tgz = dir.join("f.tgz");
        std::fs::write(&tgz, tar_gz(&[("d/x.txt", "x")])).unwrap();
        let out = dir.join("out1");
        assert!(unpack_file(&tgz, &out, Some("application/gzip")).unwrap());
        assert_eq!(read(&out.join("d/x.txt")), "x");
        // A gzipped tar served as plain tar still unpacks (tarfile.open sniffs too).
        assert!(unpack_file(&tgz, &dir.join("out2"), Some("application/x-tar")).unwrap());
        // gzip with an unknown extension is not unpacked.
        let gz = dir.join("f.bin");
        std::fs::copy(&tgz, &gz).unwrap();
        assert!(!unpack_file(&gz, &dir.join("out3"), Some("application/gzip")).unwrap());
        // zip by magic when the type is generic.
        let zf = dir.join("f.dat");
        std::fs::write(&zf, zip_bytes(&[("z/y.txt", "y")])).unwrap();
        assert!(unpack_file(&zf, &dir.join("out4"), Some("application/octet-stream")).unwrap());
        assert_eq!(read(&dir.join("out4/z/y.txt")), "y");
        assert!(!unpack_file(&zf, &dir.join("out5"), Some("text/plain")).unwrap());
    }

    /// git for the fixture repositories, with the user's configuration out of
    /// the way: `tag.gpgsign` turns `git tag v1` into an annotated tag that
    /// wants a message, `commit.gpgsign` needs a usable key and
    /// `init.templatedir` copies hooks into every repository built here. The
    /// production helper keeps that configuration, which real fetches need for
    /// credentials and `url.*.insteadOf`.
    fn git_ok(args: &[&str], cwd: &Path) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: cannot run git: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    fn make_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        git_ok(&["init", "-q", "-b", "main"], dir);
        git_ok(&["config", "user.email", "t@t"], dir);
        git_ok(&["config", "user.name", "t"], dir);
        write(&dir.join("lib/a.py"), "main");
        write(&dir.join("top.txt"), "top");
        git_ok(&["add", "."], dir);
        git_ok(&["commit", "-q", "-m", "one"], dir);
        git_ok(&["tag", "v1"], dir);
        write(&dir.join("lib/a.py"), "v2");
        git_ok(&["commit", "-q", "-am", "two"], dir);
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn git_dependency_checks_out_ref_and_copies_subdir() {
        let dir = tmp("git");
        let source = make_repo(&dir.join("origin"));
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let deps = dependencies(
            "t",
            &json!([
                {"type": "git", "source": source, "output_path": "system/lib", "subdir": "lib"},
                {"type": "git", "source": source, "output_path": "system/v1", "ref": "v1", "subdir": "lib"},
                {"type": "git", "source": source, "output_path": "system/all"},
                {"type": "git", "source": source, "output_path": "system/lib"},
                {"type": "git", "source": source, "output_path": "system/missing", "subdir": "nope"},
            ]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        // The duplicate (same source and destination) is not reported.
        assert_eq!(out.len(), 4, "{out:?}");
        assert_eq!(read(&root.join("system/lib/a.py")), "v2");
        assert_eq!(read(&root.join("system/v1/a.py")), "main");
        assert_eq!(read(&root.join("system/all/top.txt")), "top");
        assert!(
            !root.join("system/all/.git").exists(),
            "safe copy skips dot entries"
        );
        let failed: Vec<_> = out.iter().filter(|o| o.failed()).collect();
        assert_eq!(failed.len(), 1);
        assert!(
            matches!(&failed[0].status, FetchStatus::Failed { error } if error.contains("subdir nope not found"))
        );
        assert_eq!(failed[0].output_path, "system/missing");
        assert!(out.iter().any(|o| o.reason == "system/lib missing"));

        // Present outputs are left alone without --force-fetch...
        write(&root.join("system/lib/a.py"), "edited");
        let deps = dependencies(
            "t",
            &json!([{"type": "git", "source": source, "output_path": "system/lib", "subdir": "lib"}]),
            &root,
        )
        .unwrap();
        let out = fetch(deps.clone(), &opts(&root, true, false));
        assert!(matches!(out[0].status, FetchStatus::Skipped));
        assert_eq!(read(&root.join("system/lib/a.py")), "edited");
        // ...and nothing at all is considered without --fetch unless the item forces it.
        assert!(fetch(deps.clone(), &opts(&root, false, false)).is_empty());
        // --force-fetch overwrites.
        let out = fetch(deps, &opts(&root, true, true));
        assert!(
            matches!(out[0].status, FetchStatus::Fetched { .. }),
            "{out:?}"
        );
        assert_eq!(out[0].reason, "forced (--force-fetch)");
        assert_eq!(read(&root.join("system/lib/a.py")), "v2");
    }

    #[test]
    fn item_force_fetch_applies_without_fetch_flag() {
        let dir = tmp("git-force-item");
        let source = make_repo(&dir.join("origin"));
        let root = dir.join("repo");
        write(&root.join("system/lib/a.py"), "edited");
        let deps = dependencies(
            "t",
            &json!([
                {"type": "git", "source": source, "output_path": "system/lib", "subdir": "lib", "force_fetch": true},
                {"type": "git", "source": source, "output_path": "system/other", "subdir": "lib"},
            ]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, false, false));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, "forced (force_fetch: true)");
        assert_eq!(read(&root.join("system/lib/a.py")), "v2");
        assert!(!root.join("system/other").exists());
    }

    #[test]
    fn dry_run_reports_without_fetching() {
        let dir = tmp("dry");
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let deps = dependencies(
            "t",
            &json!([{"type": "git", "source": "https://example.invalid/x.git", "output_path": "x"}]),
            &root,
        )
        .unwrap();
        let out = fetch(
            deps,
            &FetchOptions {
                dry_run: true,
                ..opts(&root, true, false)
            },
        );
        assert!(matches!(out[0].status, FetchStatus::WouldFetch));
        assert_eq!(out[0].reason, "x missing");
        assert!(!root.join("x").exists());
    }

    /// A one-shot HTTP server on localhost serving `body` with `content_type`.
    fn serve(body: Vec<u8>, content_type: &str, hits: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let content_type = content_type.to_string();
        std::thread::spawn(move || {
            for _ in 0..hits {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(&body).unwrap();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn http_dependency_saves_or_unpacks() {
        let dir = tmp("http");
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let base = serve(
            tar_gz(&[("pkg/f.txt", "hello")]),
            "application/x-gzip; charset=binary",
            1,
        );
        let deps = dependencies(
            "t",
            &json!([
                {"type": "https", "source": format!("{base}/dl/pkg.tgz"), "output_path": "vendor/pkg", "unpack": true},
                {"type": "https", "source": format!("{base}/dl/pkg.tgz"), "output_path": "vendor/raw.tgz"},
            ]),
            &root,
        )
        .unwrap();
        // Two destinations, one download.
        let out = fetch(deps, &opts(&root, true, false));
        assert!(out.iter().all(|o| !o.failed()), "{out:?}");
        assert_eq!(read(&root.join("vendor/pkg/pkg/f.txt")), "hello");
        assert!(root.join("vendor/raw.tgz").is_file());

        let base = serve(b"plain".to_vec(), "text/plain", 1);
        let deps = dependencies(
            "t",
            &json!([{"type": "http", "source": format!("{base}/x.txt"), "output_path": "vendor/x", "unpack": true}]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        assert!(
            matches!(&out[0].status, FetchStatus::Failed { error } if error.contains("not supported for unpack"))
        );

        let deps = dependencies(
            "t",
            &json!([{"type": "http", "source": "http://127.0.0.1:1/none", "output_path": "vendor/none"}]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        assert!(
            matches!(&out[0].status, FetchStatus::Failed { error } if error.contains("fetching unsuccessful"))
        );
    }

    #[test]
    fn helm_dependency_pulls_once_and_copies() {
        let dir = tmp("helm");
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        // A stand-in helm that records its arguments and "untars" a chart.
        let fake = dir.join("helm");
        write(
            &fake,
            "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/calls\"\nwhile [ $# -gt 0 ]; do case $1 in --destination) dest=$2; shift;; esac; shift; done\nmkdir -p \"$dest/mychart/templates\"\necho 'name: mychart' > \"$dest/mychart/Chart.yaml\"\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let helm_path = fake.to_string_lossy().into_owned();
        let deps = dependencies(
            "t",
            &json!([
                {"type": "helm", "source": "https://charts.example/repo", "chart_name": "mychart", "version": "1.2.3", "output_path": "charts/mychart/1.2.3", "helm_path": helm_path},
                {"type": "helm", "source": "https://charts.example/repo", "chart_name": "mychart", "version": "1.2.3", "output_path": "charts/copy", "helm_path": helm_path},
                {"type": "helm", "source": "oci://ghcr.io/org/mychart", "chart_name": "mychart", "output_path": "charts/oci", "helm_path": helm_path},
            ]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        assert!(out.iter().all(|o| !o.failed()), "{out:?}");
        assert_eq!(
            read(&root.join("charts/mychart/1.2.3/Chart.yaml")),
            "name: mychart\n"
        );
        assert_eq!(
            read(&root.join("charts/copy/Chart.yaml")),
            "name: mychart\n"
        );
        assert!(root.join("charts/oci/Chart.yaml").is_file());
        let calls = read(&dir.join("calls"));
        let lines: Vec<&str> = calls.lines().collect();
        assert_eq!(lines.len(), 2, "one pull per chart identity: {calls}");
        assert!(
            lines.iter().any(|l| l
                .contains("--untar --version 1.2.3 --repo https://charts.example/repo mychart")),
            "{calls}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with("--untar oci://ghcr.io/org/mychart")),
            "{calls}"
        );
        assert!(root.join(".cache/charts").is_dir());

        // Cached: a new destination for the same version needs no pull.
        let deps = dependencies(
            "t",
            &json!([{"type": "helm", "source": "https://charts.example/repo", "chart_name": "mychart", "version": "1.2.3", "output_path": "charts/again", "helm_path": helm_path}]),
            &root,
        )
        .unwrap();
        let out = fetch(deps.clone(), &opts(&root, true, false));
        assert!(matches!(out[0].status, FetchStatus::Fetched { .. }));
        assert_eq!(read(&dir.join("calls")).lines().count(), 2);
        // Forced: pulled again.
        let out = fetch(deps, &opts(&root, true, true));
        assert!(matches!(out[0].status, FetchStatus::Fetched { .. }));
        assert_eq!(read(&dir.join("calls")).lines().count(), 3);
    }

    /// A tiny registry on localhost: token challenge on the first request,
    /// then a manifest with a tar.gz layer and a plain file layer.
    fn serve_registry() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let tgz = tar_gz(&[("gen/main.py", "def main(): pass\n")]);
        let readme = b"# art\n".to_vec();
        let d = |b: &[u8]| format!("sha256:{}", hex::encode(Sha256::digest(b)));
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "layers": [
                {"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": d(&tgz), "size": tgz.len(),
                 "annotations": {"org.opencontainers.image.title": "system/generators"}},
                {"mediaType": "text/markdown", "digest": d(&readme), "size": readme.len(),
                 "annotations": {"org.opencontainers.image.title": "README.md"}},
            ]
        }))
        .unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = s.read(&mut chunk) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let req = String::from_utf8_lossy(&buf).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                let authed = req
                    .to_ascii_lowercase()
                    .contains("authorization: bearer tok-1");
                let (status, ct, body): (&str, &str, Vec<u8>) = if path.starts_with("/token?") {
                    assert!(path.contains("scope=repository:org/art:pull"), "{path}");
                    (
                        "200 OK",
                        "application/json",
                        br#"{"token": "tok-1"}"#.to_vec(),
                    )
                } else if !authed {
                    let hdr = format!(
                        "HTTP/1.1 401 Unauthorized\r\nWww-Authenticate: Bearer realm=\"http://{addr}/token\",service=\"reg\",scope=\"repository:org/art:pull\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    s.write_all(hdr.as_bytes()).unwrap();
                    continue;
                } else if path == "/v2/org/art/manifests/v1" {
                    (
                        "200 OK",
                        "application/vnd.oci.image.manifest.v1+json",
                        manifest.clone(),
                    )
                } else if path == format!("/v2/org/art/blobs/{}", d(&tgz)) {
                    ("200 OK", "application/octet-stream", tgz.clone())
                } else if path == format!("/v2/org/art/blobs/{}", d(&readme)) {
                    ("200 OK", "application/octet-stream", readme.clone())
                } else {
                    ("404 Not Found", "text/plain", b"no".to_vec())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(&body).unwrap();
            }
        });
        addr.to_string()
    }

    #[test]
    fn oci_dependency_pulls_extracts_and_copies() {
        let dir = tmp("oci");
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let reg = serve_registry();
        let source = format!("{reg}/org/art:v1");
        let deps = dependencies(
            "t",
            &json!([
                {"type": "oci", "source": source, "output_path": "vendor/art", "insecure": true},
                {"type": "oci", "source": source, "output_path": "vendor/gen", "insecure": true, "subpath": "gen",
                 "media_type": "application/vnd.oci.image.layer.v1.tar+gzip"},
                {"type": "oci", "source": source, "output_path": "vendor/bad", "insecure": true, "subpath": "../x"},
                {"type": "oci", "source": source, "output_path": "vendor/none", "insecure": true, "subpath": "nope"},
            ]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        assert_eq!(out.len(), 4);
        let by_path = |p: &str| out.iter().find(|o| o.output_path == p).unwrap();
        assert!(!by_path("vendor/art").failed(), "{out:?}");
        // The tar.gz layer was extracted into the artifact root and its blob removed.
        assert_eq!(
            read(&root.join("vendor/art/gen/main.py")),
            "def main(): pass\n"
        );
        assert!(!root.join("vendor/art/system").exists());
        assert_eq!(read(&root.join("vendor/art/README.md")), "# art\n");
        assert!(
            by_path("vendor/art").warnings.is_empty(),
            "{:?}",
            by_path("vendor/art").warnings
        );
        assert_eq!(read(&root.join("vendor/gen/main.py")), "def main(): pass\n");
        assert!(
            matches!(&by_path("vendor/bad").status, FetchStatus::Failed { error } if error.contains("resolves outside"))
        );
        assert!(
            matches!(&by_path("vendor/none").status, FetchStatus::Failed { error } if error.contains("not found in pulled artifact"))
        );

        // Conflicting connection settings for one source are refused.
        let deps = dependencies(
            "t",
            &json!([
                {"type": "oci", "source": source, "output_path": "vendor/a", "insecure": true},
                {"type": "oci", "source": source, "output_path": "vendor/b", "insecure": false},
            ]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        assert!(out.iter().all(|o| matches!(&o.status, FetchStatus::Failed { error } if error.contains("conflicting connection settings"))));

        // A nested-only tree without subpath is reported.
        let deps = dependencies(
            "t",
            &json!([{"type": "oci", "source": source, "output_path": "vendor/only", "insecure": true,
                     "media_type": "application/vnd.oci.image.layer.v1.tar+gzip"}]),
            &root,
        )
        .unwrap();
        let out = fetch(deps, &opts(&root, true, false));
        assert!(out[0].warnings[0].contains("subpath: gen"), "{out:?}");
    }
}
