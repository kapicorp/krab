//! Pulling OCI artifacts (`type: oci` dependencies) with the registry
//! distribution API, the way oras does: the manifest's layers are saved
//! under their `org.opencontainers.image.title` annotation (the digest when
//! there is none). Anonymous and token (bearer) or basic authentication;
//! credentials come from `OCI_USERNAME` / `OCI_PASSWORD`.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

pub const TITLE_ANNOTATION: &str = "org.opencontainers.image.title";
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// `registry/repository[:tag][@digest]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    /// A tag or a `sha256:...` digest.
    pub reference: String,
}

pub fn parse_reference(source: &str) -> Result<Reference, String> {
    let (host, rest) = source.split_once('/').ok_or_else(|| {
        format!("OCI source `{source}` must be `registry/repository[:tag|@digest]`")
    })?;
    if !(host.contains('.') || host.contains(':') || host == "localhost") {
        return Err(format!(
            "OCI source `{source}` must start with the registry host (e.g. ghcr.io/org/repo:tag)"
        ));
    }
    let (rest, digest) = match rest.rsplit_once('@') {
        Some((r, d)) => (r, Some(d)),
        None => (rest, None),
    };
    let (repository, tag) = match rest.rsplit_once(':') {
        Some((r, t)) if !t.contains('/') => (r, Some(t)),
        _ => (rest, None),
    };
    if repository.is_empty() {
        return Err(format!("OCI source `{source}` has no repository"));
    }
    let registry = match host {
        "docker.io" | "index.docker.io" => "registry-1.docker.io".to_string(),
        h => h.to_string(),
    };
    let repository = if registry == "registry-1.docker.io" && !repository.contains('/') {
        format!("library/{repository}")
    } else {
        repository.to_string()
    };
    Ok(Reference {
        registry,
        repository,
        reference: digest
            .map(str::to_string)
            .or_else(|| tag.map(str::to_string))
            .unwrap_or_else(|| "latest".into()),
    })
}

/// kapitan's `tls_verify`: a boolean, or the path of a CA bundle.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum TlsVerify {
    Bool(bool),
    CaBundle(String),
}

pub struct PullOptions<'a> {
    /// Plain http instead of https.
    pub insecure: bool,
    pub tls_verify: &'a TlsVerify,
    /// Only layers of these media types; `None` for all of them.
    pub allowed_media_types: Option<&'a [String]>,
    pub credentials: Option<(String, String)>,
}

/// A layer written to disk.
#[derive(Clone, Debug)]
pub struct Pulled {
    pub path: PathBuf,
    pub media_type: String,
}

/// Pull every (allowed) layer of the artifact into `outdir`.
pub fn pull(source: &str, outdir: &Path, opts: &PullOptions) -> Result<Vec<Pulled>, String> {
    let reference = parse_reference(source)?;
    let mut registry = Registry::new(&reference, opts)?;
    let manifest_path = format!(
        "/v2/{}/manifests/{}",
        reference.repository, reference.reference
    );
    let (bytes, _) = registry.get(&manifest_path, MANIFEST_ACCEPT)?;
    // A tag cannot contain `:`, so this is a digest pin.
    if reference.reference.contains(':') {
        verify(&reference.reference, &bytes).map_err(|e| format!("manifest of {source}: {e}"))?;
    }
    let mut manifest: Json = serde_json::from_slice(&bytes)
        .map_err(|e| format!("manifest of {source} is not JSON: {e}"))?;
    if let Some(entries) = manifest.get("manifests").and_then(Json::as_array) {
        // An index: oras artifacts have one manifest; any other count would
        // need a platform guess.
        let [entry] = entries.as_slice() else {
            let listed: Vec<String> = entries
                .iter()
                .map(|e| {
                    let digest = e.get("digest").and_then(Json::as_str).unwrap_or("?");
                    match e.get("platform") {
                        Some(p) => format!("{digest} {p}"),
                        None => digest.to_string(),
                    }
                })
                .collect();
            return Err(format!(
                "index of {source} lists {} manifests, pin one with @<digest>: {}",
                entries.len(),
                listed.join(", ")
            ));
        };
        let digest = entry
            .get("digest")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("index of {source} has a manifest without digest"))?;
        let (bytes, _) = registry.get(
            &format!("/v2/{}/manifests/{digest}", reference.repository),
            MANIFEST_ACCEPT,
        )?;
        verify(digest, &bytes).map_err(|e| format!("manifest of {source}: {e}"))?;
        manifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("manifest {digest} of {source} is not JSON: {e}"))?;
    }
    let layers = manifest
        .get("layers")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("manifest of {source} has no layers"))?;
    std::fs::create_dir_all(outdir)
        .map_err(|e| format!("cannot create {}: {e}", outdir.display()))?;
    let mut pulled = Vec::new();
    for layer in layers {
        let media_type = layer
            .get("mediaType")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(allowed) = opts.allowed_media_types
            && !allowed.contains(&media_type)
        {
            continue;
        }
        let digest = layer
            .get("digest")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("a layer of {source} has no digest"))?;
        let title = layer
            .get("annotations")
            .and_then(|a| a.get(TITLE_ANNOTATION))
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| digest.replace(':', "-"));
        let path = safe_join(outdir, &title).ok_or_else(|| {
            format!("layer title `{title}` of {source} escapes the output directory")
        })?;
        let (bytes, _) = registry.get(
            &format!("/v2/{}/blobs/{digest}", reference.repository),
            "application/octet-stream, */*",
        )?;
        verify(digest, &bytes).map_err(|e| format!("layer of {source}: {e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, &bytes)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        pulled.push(Pulled { path, media_type });
    }
    Ok(pulled)
}

/// Check `bytes` against a `sha256:` digest; other algorithms are refused.
fn verify(digest: &str, bytes: &[u8]) -> Result<(), String> {
    let expected = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("unsupported digest {digest}, only sha256 is verified"))?;
    let actual = hex::encode(Sha256::digest(bytes));
    if actual != expected {
        return Err(format!("expected {digest}, downloaded sha256:{actual}"));
    }
    Ok(())
}

/// `root/rel` when `rel` is relative and stays inside `root`.
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    let mut out = root.to_path_buf();
    let mut depth = 0usize;
    for c in rel.components() {
        match c {
            Component::Normal(n) => {
                out.push(n);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                out.pop();
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if depth == 0 { None } else { Some(out) }
}

struct Registry {
    agent: ureq::Agent,
    base: String,
    credentials: Option<(String, String)>,
    /// `tls_verify` is not `false`.
    tls_verified: bool,
    /// `Authorization` header value once a challenge was answered.
    auth: Option<String>,
}

impl Registry {
    fn new(reference: &Reference, opts: &PullOptions) -> Result<Registry, String> {
        let mut tls = ureq::tls::TlsConfig::builder();
        match opts.tls_verify {
            TlsVerify::Bool(false) => tls = tls.disable_verification(true),
            TlsVerify::Bool(true) => {}
            TlsVerify::CaBundle(path) => {
                let pem = std::fs::read(path)
                    .map_err(|e| format!("cannot read CA bundle {path}: {e}"))?;
                let certs: Vec<ureq::tls::Certificate<'static>> = ureq::tls::parse_pem(&pem)
                    .filter_map(|item| match item {
                        Ok(ureq::tls::PemItem::Certificate(c)) => Some(c),
                        _ => None,
                    })
                    .collect();
                if certs.is_empty() {
                    return Err(format!("CA bundle {path} holds no certificate"));
                }
                tls = tls.root_certs(ureq::tls::RootCerts::new_with_certs(&certs));
            }
        }
        let agent = ureq::Agent::config_builder()
            .tls_config(tls.build())
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(crate::fetch::FETCH_TIMEOUT))
            .user_agent(format!("kapitan/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Ok(Registry {
            agent,
            base: format!(
                "{}://{}",
                if opts.insecure { "http" } else { "https" },
                reference.registry
            ),
            credentials: opts.credentials.clone(),
            tls_verified: *opts.tls_verify != TlsVerify::Bool(false),
            auth: None,
        })
    }

    /// GET `path` on the registry, answering one authentication challenge.
    fn get(
        &mut self,
        path: &str,
        accept: &str,
    ) -> Result<(Vec<u8>, ureq::http::HeaderMap), String> {
        let url = format!("{}{path}", self.base);
        let mut resp = self.send(&url, accept)?;
        if resp.status() == 401 {
            let challenge = resp
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            self.authenticate(&challenge)?;
            resp = self.send(&url, accept)?;
        }
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .body_mut()
            .with_config()
            .limit(crate::fetch::FETCH_MAX_BYTES)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))?;
        if !status.is_success() {
            let detail = String::from_utf8_lossy(&body);
            let detail = detail.trim();
            return Err(if detail.is_empty() {
                format!("{url}: HTTP {status}")
            } else {
                format!("{url}: HTTP {status}: {detail}")
            });
        }
        Ok((body, headers))
    }

    fn send(&self, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let mut req = self.agent.get(url).header("Accept", accept);
        if let Some(auth) = &self.auth {
            req = req.header("Authorization", auth);
        }
        req.call().map_err(|e| format!("{url}: {e}"))
    }

    fn basic(&self) -> Option<String> {
        self.credentials.as_ref().map(|(u, p)| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
            )
        })
    }

    /// Answer a `WWW-Authenticate` challenge: fetch a bearer token from the
    /// realm (with basic credentials when there are any) or fall back to
    /// basic authentication. Credentials never go over plain http, and to a
    /// realm on another origin only with TLS verification.
    fn authenticate(&mut self, challenge: &str) -> Result<(), String> {
        if self.credentials.is_some() && self.base.starts_with("http://") {
            return Err(format!(
                "{} asks for authentication; OCI_USERNAME / OCI_PASSWORD are not sent over plain http (`insecure: true`)",
                self.base
            ));
        }
        let (scheme, params) = challenge.split_once(' ').unwrap_or((challenge, ""));
        if scheme.eq_ignore_ascii_case("basic") {
            self.auth = Some(self.basic().ok_or_else(|| {
                format!(
                    "{} requires basic authentication; set OCI_USERNAME and OCI_PASSWORD",
                    self.base
                )
            })?);
            return Ok(());
        }
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(format!(
                "{} answered 401 with an unsupported challenge `{challenge}`",
                self.base
            ));
        }
        let params = parse_challenge(params);
        let realm = params
            .get("realm")
            .ok_or_else(|| format!("{} sent a bearer challenge without realm", self.base))?;
        let same_origin = realm
            .strip_prefix(&self.base)
            .is_some_and(|r| r.is_empty() || r.starts_with(['/', '?']));
        let refused = if same_origin {
            None
        } else if !realm.starts_with("https://") {
            Some("a realm on another origin must use https")
        } else if self.credentials.is_some() && !self.tls_verified {
            Some("credentials are not sent to another origin with `tls_verify: false`")
        } else {
            None
        };
        if let Some(reason) = refused {
            return Err(format!("{} names token realm {realm}: {reason}", self.base));
        }
        let mut url = realm.clone();
        let mut sep = if url.contains('?') { '&' } else { '?' };
        for key in ["service", "scope"] {
            if let Some(v) = params.get(key) {
                url.push(sep);
                url.push_str(key);
                url.push('=');
                url.push_str(&percent_encode(v));
                sep = '&';
            }
        }
        let mut req = self.agent.get(&url).header("Accept", "application/json");
        if let Some(basic) = self.basic() {
            req = req.header("Authorization", &basic);
        }
        let mut resp = req
            .call()
            .map_err(|e| format!("token request {url}: {e}"))?;
        let status = resp.status();
        let body = resp
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_vec()
            .map_err(|e| format!("token request {url}: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "token request {url}: HTTP {status}{}",
                if self.credentials.is_none() {
                    " (set OCI_USERNAME and OCI_PASSWORD for private artifacts)"
                } else {
                    ""
                }
            ));
        }
        let json: Json = serde_json::from_slice(&body)
            .map_err(|e| format!("token request {url}: response is not JSON: {e}"))?;
        let token = json
            .get("token")
            .or_else(|| json.get("access_token"))
            .and_then(Json::as_str)
            .ok_or_else(|| format!("token request {url}: no token in the response"))?;
        self.auth = Some(format!("Bearer {token}"));
        Ok(())
    }
}

/// `realm="https://x/token",service="x",scope="repository:a/b:pull"`.
fn parse_challenge(params: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut rest = params.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_ascii_lowercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            value = r[..end].to_string();
            rest = r[end..].strip_prefix('"').unwrap_or("");
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        out.insert(key, value);
        rest = rest
            .trim_start()
            .strip_prefix(',')
            .unwrap_or(rest)
            .trim_start();
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        let r = parse_reference("ghcr.io/org/art:1.2").unwrap();
        assert_eq!(
            r,
            Reference {
                registry: "ghcr.io".into(),
                repository: "org/art".into(),
                reference: "1.2".into()
            }
        );
        assert_eq!(
            parse_reference("ghcr.io/org/art").unwrap().reference,
            "latest"
        );
        assert_eq!(
            parse_reference("localhost:5000/art:v1@sha256:abc").unwrap(),
            Reference {
                registry: "localhost:5000".into(),
                repository: "art".into(),
                reference: "sha256:abc".into()
            }
        );
        let r = parse_reference("docker.io/alpine:3").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "library/alpine");
        assert!(parse_reference("org/art:1").is_err());
        assert!(parse_reference("ghcr.io").is_err());
    }

    #[test]
    fn challenges_and_paths() {
        let p = parse_challenge(
            r#"realm="https://ghcr.io/token",service="ghcr.io",scope="repository:org/art:pull""#,
        );
        assert_eq!(p["realm"], "https://ghcr.io/token");
        assert_eq!(p["scope"], "repository:org/art:pull");
        let p = parse_challenge("realm=http://x/t, service=x");
        assert_eq!(p["realm"], "http://x/t");
        assert_eq!(p["service"], "x");
        assert_eq!(
            percent_encode("repository:a/b:pull c"),
            "repository:a/b:pull%20c"
        );

        let root = Path::new("/out");
        assert_eq!(safe_join(root, "a/b"), Some(PathBuf::from("/out/a/b")));
        assert_eq!(safe_join(root, "./a/../b"), Some(PathBuf::from("/out/b")));
        assert_eq!(safe_join(root, "../x"), None);
        assert_eq!(safe_join(root, "/etc/passwd"), None);
        assert_eq!(safe_join(root, "."), None);
    }

    /// A listener on 127.0.0.1 answering every connection with `response`;
    /// returns its address and what it received.
    fn listen(response: String) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
                let _ = s.write_all(response.as_bytes());
            }
        });
        (addr, log)
    }

    const TOKEN: &str =
        "HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"token\":\"t\"}";

    fn with_credentials(insecure: bool, tls_verify: &TlsVerify) -> PullOptions<'_> {
        PullOptions {
            insecure,
            tls_verify,
            allowed_media_types: None,
            credentials: Some(("alice".into(), "s3cret".into())),
        }
    }

    #[test]
    fn credentials_are_not_sent_over_plain_http() {
        let (realm, realm_log) = listen(TOKEN.into());
        let (registry, registry_log) = listen(format!(
            "HTTP/1.1 401 Unauthorized\r\nWww-Authenticate: Bearer realm=\"http://{realm}/token\",service=\"x\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        let err = pull(
            &format!("{registry}/art:v1"),
            &std::env::temp_dir().join("krab-oci-never-created"),
            &with_credentials(true, &TlsVerify::Bool(true)),
        )
        .unwrap_err();
        let realm_log = realm_log.lock().unwrap();
        assert!(realm_log.is_empty(), "{realm_log:?}");
        let registry_log = registry_log.lock().unwrap();
        assert!(
            !registry_log
                .iter()
                .any(|r| r.contains("authorization: basic")),
            "{registry_log:?}"
        );
        assert!(
            err.contains("insecure") && err.contains("OCI_USERNAME"),
            "{err}"
        );
    }

    #[test]
    fn token_realms_on_another_origin() {
        let reference = parse_reference("127.0.0.1:1/art").unwrap();
        // (realm scheme, tls_verify, credentials, request expected)
        for (scheme, verify, credentials, sent) in [
            ("http", true, true, false),
            ("https", false, true, false),
            ("https", true, true, true),
            ("http", true, false, false),
            ("https", false, false, true),
        ] {
            let (addr, log) = listen(TOKEN.into());
            let tls_verify = TlsVerify::Bool(verify);
            let mut opts = with_credentials(false, &tls_verify);
            if !credentials {
                opts.credentials = None;
            }
            let realm = format!("{scheme}://{addr}/token");
            let result = Registry::new(&reference, &opts)
                .unwrap()
                .authenticate(&format!("Bearer realm=\"{realm}\",service=\"x\""));
            let log = log.lock().unwrap();
            let case = format!("{realm} tls_verify={verify} credentials={credentials}");
            assert_eq!(!log.is_empty(), sent, "{case}: {log:?}");
            if !sent {
                let err = result.unwrap_err();
                assert!(
                    err.contains(&realm) && err.contains("127.0.0.1:1"),
                    "{case}: {err}"
                );
            }
        }
    }
}
