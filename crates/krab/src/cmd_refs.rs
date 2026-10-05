//! `krab refs`: write, reveal, update and validate references, with
//! kapitan's flags.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use clap::Args;
use krab_compile::inputs::Reads;
use krab_compile::refs::{
    Ref, RefController, RefError, RefType, TargetSecrets, b64_encode, gpg, vault,
};
use krab_inventory::emit::dumps_pretty;
use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::source::SourceId;
use krab_inventory::yaml::parse_documents;
use krab_inventory::{Node, Value};
use krab_server::protocol::{TargetParams, TargetResult, TargetsResult};
use serde_json::{Value as Json, json};

use crate::app::{App, Failure};
use crate::completions::complete_target;

#[derive(Args)]
pub struct RefsArgs {
    /// Write a ref: TOKENNAME is `<type>:<path>`, e.g. `gkms:targets/prod/db-password`
    #[arg(short = 'w', long, value_name = "TOKENNAME")]
    write: Option<String>,

    /// Re-encrypt a ref for new GPG recipients or a new KMS key
    #[arg(long, value_name = "TOKENNAME")]
    update: Option<String>,

    /// Re-encrypt every target's refs (`<refs-path>/<target>/...`) with the
    /// recipients or key its inventory declares
    #[arg(long)]
    update_targets: bool,

    /// Check every target's refs against the recipients or key its inventory declares
    #[arg(long)]
    validate_targets: bool,

    /// With --write: replace a ref file that already exists
    #[arg(long)]
    force: bool,

    /// Base64-encode the file content before storing it
    #[arg(long, alias = "b64")]
    base64: bool,

    /// Treat the file content as binary data
    #[arg(long)]
    binary: bool,

    /// Reveal refs: in --file (or a directory), --ref-file, or --tag
    #[arg(short = 'r', long)]
    reveal: bool,

    /// A ref tag to reveal, e.g. `?{gkms:my/ref:123456}`
    #[arg(long, value_name = "REFTAG")]
    tag: Option<String>,

    /// A ref file to reveal; `-` reads stdin
    #[arg(long, alias = "rf", value_name = "REFFILENAME")]
    ref_file: Option<String>,

    /// The file to write from, or the file/directory to reveal; `-` reads stdin
    #[arg(short = 'f', long, value_name = "FILENAME")]
    file: Option<String>,

    /// Take recipients/keys/vault settings from this target's `parameters.kapitan.secrets`
    #[arg(short = 't', long, value_name = "TARGET_NAME", add = clap_complete::ArgValueCompleter::new(complete_target))]
    target_name: Option<String>,

    /// GPG recipients (names or fingerprints)
    #[arg(short = 'R', long, num_args = 1.., value_name = "RECIPIENT")]
    recipients: Vec<String>,

    /// KMS key (gkms: `projects/P/locations/L/keyRings/R/cryptoKeys/K`)
    #[arg(short = 'K', long)]
    key: Option<String>,

    /// Vault authentication type (token, approle, userpass, ldap, github)
    #[arg(long, value_name = "AUTH")]
    vault_auth: Option<String>,

    /// Mount point of the Vault secrets engine (default: `secret`)
    #[arg(long, value_name = "MOUNT")]
    vault_mount: Option<String>,

    /// Path of the secret in Vault (default: the ref path)
    #[arg(long, value_name = "PATH")]
    vault_path: Option<String>,

    /// Key inside the Vault secret
    #[arg(long, value_name = "KEY")]
    vault_key: Option<String>,

    /// Where ref files live (default: `refs.refs-path` from .kapitan, else ./refs)
    #[arg(long, value_name = "REFS_PATH")]
    refs_path: Option<PathBuf>,

    /// Accepted for compatibility with kapitan
    #[arg(short = 'v', long, hide = true)]
    verbose: bool,
}

fn fail(e: RefError) -> Failure {
    Failure::Message(e.0)
}

pub fn run(app: &App, args: RefsArgs) -> Result<(), Failure> {
    let refs_path = args
        .refs_path
        .clone()
        .or_else(|| app.dot.refs_str("refs-path").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./refs"));
    let refs_path = if refs_path.is_absolute() {
        refs_path
    } else {
        app.cwd.join(refs_path)
    };
    let rc = RefController::new(refs_path, false);
    if let Some(token) = &args.write {
        write(app, &args, &rc, token)
    } else if args.reveal {
        reveal(&args, &rc)
    } else if let Some(token) = &args.update {
        update(app, &args, &rc, token)
    } else if args.update_targets || args.validate_targets {
        update_validate(app, &args, &rc)
    } else {
        Err(Failure::Message(
            "nothing to do: pass --write, --reveal, --update, --update-targets or --validate-targets (see --help)".into(),
        ))
    }
}

/// `type:path` of a token name.
fn split_token(token: &str) -> Result<(RefType, &str), Failure> {
    let parts: Vec<&str> = token.split(':').collect();
    if parts.len() != 2 {
        return Err(Failure::Message(format!(
            "Invalid token name: {token}. Token names must be in the format <type>:<path>, e.g. gpg:my/secret"
        )));
    }
    let type_name = RefType::parse(parts[0]).ok_or_else(|| {
        Failure::Message(format!(
            "Invalid token type: {}. Try using {}",
            parts[0],
            RefType::NAMES
        ))
    })?;
    Ok((type_name, parts[1]))
}

/// The rendered document of one target.
fn target_document(app: &App, name: &str) -> Result<Json, Failure> {
    if let Some(mut c) = app.client() {
        let r: TargetResult = c
            .call(
                "inventory.target",
                TargetParams {
                    name: name.to_string(),
                    path: None,
                },
            )
            .map_err(|e| app.rpc_fail(e))?;
        return Ok(r.document);
    }
    let t = app.inv.render_named(name).map_err(|e| app.fail(vec![e]))?;
    Ok(t.to_document().value.to_json())
}

/// `parameters.kapitan.secrets` of a target, which must exist.
fn target_secrets(app: &App, name: &str) -> Result<TargetSecrets, Failure> {
    let doc = target_document(app, name)?;
    let ts = TargetSecrets::from_document(name, &doc);
    if ts.secrets.as_ref().is_none_or(Json::is_null) {
        return Err(Failure::Message(format!(
            "parameters.kapitan.secrets not defined in {name}"
        )));
    }
    Ok(ts)
}

fn target_names(app: &App) -> Result<Vec<String>, Failure> {
    if let Some(mut c) = app.client() {
        let r: TargetsResult = c
            .call("inventory.targets", Json::Null)
            .map_err(|e| app.rpc_fail(e))?;
        return Ok(r.targets.into_iter().map(|t| t.name).collect());
    }
    Ok(app
        .inv
        .discover_targets()
        .map_err(|e| app.fail(vec![e]))?
        .into_iter()
        .map(|t| t.name)
        .collect())
}

fn read_input(file: &str, binary: bool) -> Result<Vec<u8>, Failure> {
    let data = if file == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| Failure::Message(format!("cannot read stdin: {e}")))?;
        buf
    } else {
        std::fs::read(file).map_err(|e| Failure::Message(format!("cannot read {file}: {e}")))?
    };
    if !binary && std::str::from_utf8(&data).is_err() {
        return Err(Failure::Message(
            "Could not read file. Please add '--binary' if the file contains binary data.".into(),
        ));
    }
    Ok(data)
}

/// The KMS key for `--write`/`--update`: `--key`, else the target's.
fn kms_key(
    args: &RefsArgs,
    ts: &TargetSecrets,
    type_name: RefType,
    config_wins: bool,
) -> Result<String, Failure> {
    let config = ts
        .section(type_name.name())
        .and_then(|s| s.get("key"))
        .and_then(Json::as_str)
        .map(str::to_string);
    let key = if config_wins {
        config.or_else(|| args.key.clone())
    } else {
        args.key.clone().or(config)
    };
    key.ok_or_else(|| {
        Failure::Message(format!(
            "No KMS key specified. Use --key or specify it in parameters.kapitan.secrets.{type_name}.key and use --target-name"
        ))
    })
}

/// GPG recipients: the target's when it declares them, else `--recipients`.
fn gpg_recipients(args: &RefsArgs, ts: &TargetSecrets) -> Result<Vec<Json>, Failure> {
    let recipients: Vec<Json> = match ts
        .section("gpg")
        .and_then(|g| g.get("recipients"))
        .and_then(Json::as_array)
    {
        Some(list) if !list.is_empty() => list.clone(),
        _ => args
            .recipients
            .iter()
            .map(|n| json!({ "name": n }))
            .collect(),
    };
    if recipients.is_empty() {
        return Err(Failure::Message(
            "No GPG recipients specified. Use --recipients or specify them in parameters.kapitan.secrets.gpg.recipients and use --target-name".into(),
        ));
    }
    Ok(recipients)
}

/// Vault settings for `--write`: the target's section plus `--vault-auth`.
fn vault_params(args: &RefsArgs, ts: &TargetSecrets, type_name: RefType) -> Result<Json, Failure> {
    let mut section = ts
        .section(type_name.name())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(auth) = &args.vault_auth {
        section["auth"] = Json::String(auth.clone());
    }
    if section
        .get("auth")
        .and_then(Json::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(Failure::Message(format!(
            "No Authentication type parameter specified. Specify it in parameters.kapitan.secrets.{type_name}.auth and use --target-name or use --vault-auth"
        )));
    }
    Ok(vault::normalize_params(Some(&section), type_name))
}

fn write(app: &App, args: &RefsArgs, rc: &RefController, token: &str) -> Result<(), Failure> {
    let file = args
        .file
        .as_deref()
        .ok_or_else(|| Failure::Message("--file is required with --write".into()))?;
    let data = read_input(file, args.binary)?;
    let ts = match &args.target_name {
        Some(name) => target_secrets(app, name)?,
        None => TargetSecrets::default(),
    };
    let (type_name, path) = split_token(token)?;
    let existing = rc.refs_path.join(path);
    if existing.symlink_metadata().is_ok() && !args.force {
        return Err(Failure::Message(format!(
            "{} already exists; pass --force to replace it",
            existing.display()
        )));
    }
    let (payload, encoding) = if args.base64 {
        (b64_encode(&data).into_bytes(), "base64")
    } else {
        (data, "original")
    };
    let r = match type_name {
        RefType::Gpg => {
            let recipients = gpg_recipients(args, &ts)?;
            rc.encrypt_gpg(&payload, encoding, &recipients)
                .map_err(fail)?
        }
        RefType::Gkms | RefType::AwsKms | RefType::AzKms => {
            let key = kms_key(args, &ts, type_name, false)?;
            rc.encrypt_kms(type_name, &payload, encoding, &key)
                .map_err(fail)?
        }
        RefType::VaultKv => {
            let params = vault_params(args, &ts, RefType::VaultKv)?;
            // The inventory's mount wins over --vault-mount, like kapitan.
            let mount = ts
                .section("vaultkv")
                .and_then(|s| s.get("mount"))
                .and_then(Json::as_str)
                .map(str::to_string)
                .or_else(|| args.vault_mount.clone())
                .unwrap_or_else(|| "secret".into());
            let path_in_vault = args.vault_path.clone().unwrap_or_else(|| path.to_string());
            let key = args.vault_key.as_deref().ok_or_else(|| {
                Failure::Message("Could not create VaultSecret: vaultkv: key is missing".into())
            })?;
            let explicit = vault::skip_verify_explicit(ts.section("vaultkv"));
            rc.write_vaultkv(
                &params,
                explicit,
                &payload,
                encoding,
                &mount,
                &path_in_vault,
                key,
            )
            .map_err(fail)?
        }
        RefType::VaultTransit => {
            let params = vault_params(args, &ts, RefType::VaultTransit)?;
            let explicit = vault::skip_verify_explicit(ts.section("vaulttransit"));
            rc.encrypt_vaulttransit(&params, explicit, &payload, encoding)
                .map_err(fail)?
        }
        RefType::Base64 => Ref::new(RefType::Base64, b64_encode(&payload), encoding),
        RefType::Plain | RefType::Env => {
            let text = String::from_utf8(payload).map_err(|_| {
                Failure::Message(format!(
                    "{type_name} refs hold text; add --base64 to store binary data"
                ))
            })?;
            Ref::new(type_name, text, encoding)
        }
    };
    rc.write(path, &r).map_err(fail)?;
    Ok(())
}

fn reveal(args: &RefsArgs, rc: &RefController) -> Result<(), Failure> {
    let mut reads = Reads::default();
    let mut out = std::io::stdout().lock();
    if args.file.as_deref() == Some("-") || args.ref_file.as_deref() == Some("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| Failure::Message(format!("cannot read stdin: {e}")))?;
        let revealed = reveal_raw(rc, &text, &mut reads)?;
        let _ = out.write_all(revealed.as_bytes());
        return Ok(());
    }
    if let Some(file) = &args.file {
        for content in reveal_path(rc, Path::new(file), &mut reads)? {
            let _ = out.write_all(content.as_bytes());
        }
        return Ok(());
    }
    if let Some(ref_file) = &args.ref_file {
        let r = rc.ref_from_file(Path::new(ref_file)).map_err(fail)?;
        let _ = out.write_all(rc.reveal_ref(&r).map_err(fail)?.as_bytes());
        return Ok(());
    }
    if let Some(tag) = &args.tag {
        let _ = out.write_all(rc.reveal_str(tag, &mut reads).map_err(fail)?.as_bytes());
        return Ok(());
    }
    Err(Failure::Message(
        "--file or --ref-file is required with --reveal".into(),
    ))
}

/// Reveal the tags in text, line by line like kapitan's `reveal_raw_file`.
fn reveal_raw(rc: &RefController, text: &str, reads: &mut Reads) -> Result<String, Failure> {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&rc.reveal_str(line, reads).map_err(fail)?);
    }
    Ok(out)
}

/// kapitan's `Revealer.reveal_path`: a file's revealed content, or for a
/// directory the concatenation of its YAML files (else JSON, else raw).
fn reveal_path(rc: &RefController, path: &Path, reads: &mut Reads) -> Result<Vec<String>, Failure> {
    if path.is_file() {
        return Ok(vec![reveal_file(rc, path, reads)?.0]);
    }
    if path.is_dir() {
        let (mut yaml, mut json, mut raw) = (String::new(), String::new(), String::new());
        let mut files = Vec::new();
        collect_files(path, &mut files)?;
        for file in files {
            let (content, kind) = reveal_file(rc, &file, reads)?;
            match kind {
                "yaml" => yaml.push_str(&content),
                "json" => json.push_str(&content),
                _ => raw.push_str(&content),
            }
        }
        return Ok(if !yaml.is_empty() {
            vec![yaml]
        } else if !json.is_empty() {
            vec![json]
        } else if !raw.is_empty() {
            vec![raw]
        } else {
            vec![]
        });
    }
    Err(Failure::Message(format!(
        "{}: no such file or directory",
        path.display()
    )))
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Failure> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| Failure::Message(format!("cannot read {}: {e}", dir.display())))?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_files(&p, out)?;
        } else if p.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

/// A file's revealed content and its kind (`yaml`, `json`, `raw`).
fn reveal_file(
    rc: &RefController,
    path: &Path,
    reads: &mut Reads,
) -> Result<(String, &'static str), Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Failure::Message(format!("cannot read {}: {e}", path.display())))?;
    let name = path.to_string_lossy();
    if name.ends_with(".yml") || name.ends_with(".yaml") {
        let docs = parse_documents(&text, SourceId::SYNTHETIC)
            .map_err(|e| Failure::Message(format!("{}: {e}", path.display())))?;
        let mut out = String::new();
        for doc in docs {
            let mut value = doc.value;
            rc.reveal_value(&mut value, reads).map_err(fail)?;
            // `yaml.dump_all(..., Dumper=PrettyDumper, explicit_start=True)`
            out.push_str("---\n");
            out.push_str(&dump_yaml(&Node::synthetic(value), &DumpOptions::default()));
        }
        return Ok((out, "yaml"));
    }
    if name.ends_with(".json") {
        let json: Json = serde_json::from_str(&text)
            .map_err(|e| Failure::Message(format!("{}: {e}", path.display())))?;
        let mut value = Value::from(json);
        rc.reveal_value(&mut value, reads).map_err(fail)?;
        return Ok((dumps_pretty(&value, 4, true), "json"));
    }
    Ok((reveal_raw(rc, &text, reads)?, "raw"))
}

/// The plaintext of `r` as bytes to re-encrypt (kapitan re-encodes base64
/// refs the same way, so the stored payload is unchanged).
fn payload_of(rc: &RefController, r: &Ref) -> Result<Vec<u8>, Failure> {
    Ok(rc.reveal_ref(r).map_err(fail)?.into_bytes())
}

fn update(app: &App, args: &RefsArgs, rc: &RefController, token: &str) -> Result<(), Failure> {
    let (type_name, path) = split_token(token)?;
    let ts = match &args.target_name {
        Some(name) => target_secrets(app, name)?,
        None => TargetSecrets::default(),
    };
    let tag = format!("?{{{type_name}:{path}}}");
    let mut reads = Reads::default();
    match type_name {
        RefType::Gpg => {
            let recipients = gpg_recipients(args, &ts)?;
            let fingerprints = gpg::lookup_fingerprints(&recipients).map_err(fail)?;
            let r = rc.get(&tag, &mut reads).map_err(fail)?;
            if fingerprints == r.recipients {
                return Ok(());
            }
            let payload = payload_of(rc, &r)?;
            let new = rc
                .encrypt_gpg(&payload, &r.encoding, &recipients)
                .map_err(fail)?;
            rc.write(path, &new).map_err(fail)?;
        }
        RefType::Gkms | RefType::AwsKms | RefType::AzKms => {
            let key = kms_key(args, &ts, type_name, true)?;
            let r = rc.get(&tag, &mut reads).map_err(fail)?;
            if r.key.as_deref() == Some(key.as_str()) {
                return Ok(());
            }
            let payload = payload_of(rc, &r)?;
            let new = rc
                .encrypt_kms(type_name, &payload, &r.encoding, &key)
                .map_err(fail)?;
            rc.write(path, &new).map_err(fail)?;
        }
        _ => {
            return Err(Failure::Message(format!(
                "Invalid token: {token}. Try using gpg/gkms/awskms/azkms:{path}"
            )));
        }
    }
    Ok(())
}

/// kapitan's `secret_update_validate`: every ref under
/// `<refs-path>/<target>/...` checked (or re-encrypted) against the
/// target's `parameters.kapitan.secrets`.
fn update_validate(app: &App, args: &RefsArgs, rc: &RefController) -> Result<(), Failure> {
    let validate = args.validate_targets;
    let targets = target_names(app)?;
    let mut files = Vec::new();
    if rc.refs_path.is_dir() {
        collect_files(&rc.refs_path, &mut files)?;
    }
    let mut by_target: std::collections::BTreeMap<String, Vec<(String, Ref)>> = Default::default();
    for file in files {
        let rel = file
            .strip_prefix(&rc.refs_path)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let target = rel.split('/').next().unwrap_or("").to_string();
        if !targets.contains(&target) {
            continue;
        }
        let r = rc.ref_from_file(&file).map_err(fail)?;
        by_target.entry(target).or_default().push((rel, r));
    }
    let mut mismatches = 0;
    let mut err = std::io::stderr().lock();
    for (target, refs) in by_target {
        let ts = target_secrets(app, &target)?;
        for (rel, r) in refs {
            let tag = format!("?{{{}:{rel}}}", r.type_name);
            match r.type_name {
                RefType::Gpg => {
                    let Some(recipients) = ts
                        .section("gpg")
                        .and_then(|g| g.get("recipients"))
                        .and_then(Json::as_array)
                    else {
                        continue;
                    };
                    let wanted = gpg::lookup_fingerprints(recipients).map_err(fail)?;
                    if wanted == r.recipients {
                        continue;
                    }
                    if validate {
                        let _ = writeln!(err, "{tag} recipient mismatch");
                        let remove: Vec<&String> = r
                            .recipients
                            .iter()
                            .filter(|f| !wanted.contains(f))
                            .collect();
                        let add: Vec<&String> = wanted
                            .iter()
                            .filter(|f| !r.recipients.contains(f))
                            .collect();
                        if !remove.is_empty() {
                            let _ = writeln!(err, "{remove:?} needs removal");
                        }
                        if !add.is_empty() {
                            let _ = writeln!(err, "{add:?} needs addition");
                        }
                        mismatches += 1;
                    } else {
                        let payload = payload_of(rc, &r)?;
                        let new = rc
                            .encrypt_gpg(&payload, &r.encoding, recipients)
                            .map_err(fail)?;
                        rc.write(&rel, &new).map_err(fail)?;
                    }
                }
                RefType::Gkms | RefType::AwsKms | RefType::AzKms => {
                    let Some(key) = ts
                        .section(r.type_name.name())
                        .and_then(|s| s.get("key"))
                        .and_then(Json::as_str)
                    else {
                        continue;
                    };
                    if r.key.as_deref() == Some(key) {
                        continue;
                    }
                    if validate {
                        let _ = writeln!(err, "{tag} key mismatch");
                        mismatches += 1;
                    } else {
                        let payload = payload_of(rc, &r)?;
                        let new = rc
                            .encrypt_kms(r.type_name, &payload, &r.encoding, key)
                            .map_err(fail)?;
                        rc.write(&rel, &new).map_err(fail)?;
                    }
                }
                RefType::VaultTransit => {
                    let Some(section) = ts.section("vaulttransit") else {
                        continue;
                    };
                    let key = section.get("key").and_then(Json::as_str);
                    let current = r
                        .vault_params
                        .as_ref()
                        .and_then(|p| vault::param_str(p, "crypto_key"));
                    if key.is_none() || key == current.as_deref() {
                        continue;
                    }
                    if validate {
                        let _ = writeln!(err, "{tag} key mismatch");
                        mismatches += 1;
                    } else {
                        let payload = payload_of(rc, &r)?;
                        let mut params =
                            vault::normalize_params(Some(section), RefType::VaultTransit);
                        params["crypto_key"] = Json::String(key.unwrap().to_string());
                        let new = rc
                            .encrypt_vaulttransit(
                                &params,
                                vault::skip_verify_explicit(Some(section)),
                                &payload,
                                &r.encoding,
                            )
                            .map_err(fail)?;
                        rc.write(&rel, &new).map_err(fail)?;
                    }
                }
                _ => {
                    let _ = writeln!(err, "Invalid secret {tag}, could not get type, skipping");
                }
            }
        }
    }
    if mismatches > 0 {
        return Err(Failure::Message(format!(
            "{mismatches} ref(s) do not match their target's parameters.kapitan.secrets"
        )));
    }
    Ok(())
}
