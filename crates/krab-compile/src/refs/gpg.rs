//! GPG refs through the `gpg` binary, invoked the way python-gnupg does
//! (`--status-fd 2 --no-tty --no-verbose --fixed-list-mode --batch
//! --with-colons`), so the same keyring, agent and trust settings apply.

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::Value as Json;

use super::RefError;

fn gpg() -> Command {
    let bin = std::env::var("GPGBINARY").unwrap_or_else(|_| "gpg".into());
    let mut cmd = Command::new(bin);
    cmd.args([
        "--status-fd",
        "2",
        "--no-tty",
        "--no-verbose",
        "--fixed-list-mode",
        "--batch",
        "--with-colons",
    ]);
    cmd
}

fn run(mut cmd: Command, input: &[u8], what: &str) -> Result<Vec<u8>, RefError> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            RefError(format!(
                "gpg: cannot run gpg to {what}: {e} (is GnuPG installed?)"
            ))
        })?;
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child
        .wait_with_output()
        .map_err(|e| RefError(format!("gpg: {what}: {e}")))?;
    let _ = writer.join();
    if !out.status.success() {
        let status = String::from_utf8_lossy(&out.stderr);
        let summary: Vec<&str> = status
            .lines()
            .filter(|l| {
                !l.starts_with("[GNUPG:]") || l.contains("FAILURE") || l.contains("INV_RECP")
            })
            .collect();
        return Err(RefError(format!(
            "gpg: {what} failed: {}",
            summary.join(" | ").trim()
        )));
    }
    Ok(out.stdout)
}

/// Encrypt (and sign) `data` for `fingerprints`, binary output.
pub fn encrypt(data: &[u8], fingerprints: &[String]) -> Result<Vec<u8>, RefError> {
    let mut cmd = gpg();
    cmd.arg("--encrypt");
    for f in fingerprints {
        cmd.args(["--recipient", f]);
    }
    cmd.arg("--sign");
    run(cmd, data, "encrypt")
}

pub fn decrypt(data: &[u8]) -> Result<Vec<u8>, RefError> {
    let mut cmd = gpg();
    cmd.arg("--decrypt");
    run(cmd, data, "decrypt")
}

/// kapitan's `lookup_fingerprints`: recipients given as `{fingerprint: F}`
/// or `{name: N}` (looked up in the keyring), sorted and deduplicated.
pub fn lookup_fingerprints(recipients: &[Json]) -> Result<Vec<String>, RefError> {
    let mut out = Vec::new();
    for r in recipients {
        let fingerprint = r.get("fingerprint").and_then(Json::as_str);
        let name = r.get("name").and_then(Json::as_str);
        match (fingerprint, name) {
            (Some(f), _) => out.push(f.to_string()),
            (None, Some(n)) => out.push(fingerprint_non_expired(n)?),
            (None, None) => {
                return Err(RefError(format!(
                    "gpg recipient {r} has neither `fingerprint` nor `name`"
                )));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// One key as `--list-keys --with-colons` prints it.
struct ListedKey {
    fingerprint: String,
    /// Expiry as seconds since the epoch; empty when the key does not expire.
    expires: String,
}

fn list_keys(name: &str) -> Result<Vec<ListedKey>, RefError> {
    let mut cmd = gpg();
    cmd.args(["--list-keys", "--fingerprint", "--fingerprint", name]);
    let out = run(cmd, b"", &format!("list keys for {name}"))?;
    let text = String::from_utf8_lossy(&out);
    let mut keys = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.first().copied() {
            Some("pub") => pending = Some(fields.get(6).copied().unwrap_or("").to_string()),
            Some("sub") => pending = None,
            Some("fpr") => {
                if let Some(expires) = pending.take() {
                    keys.push(ListedKey {
                        fingerprint: fields.get(9).copied().unwrap_or("").to_string(),
                        expires,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(keys)
}

/// The first non-expired key fingerprint for `recipient_name`.
fn fingerprint_non_expired(recipient_name: &str) -> Result<String, RefError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for key in list_keys(recipient_name)? {
        if key.expires.is_empty() {
            return Ok(key.fingerprint);
        }
        if let Ok(expires) = key.expires.parse::<u64>()
            && now < expires
        {
            return Ok(key.fingerprint);
        }
    }
    Err(RefError(format!(
        "Could not find valid key for recipient: {recipient_name}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway keyring with one key; `None` when gpg is not installed.
    fn keyring() -> Option<std::path::PathBuf> {
        let home = super::super::tests::temp_refs("gnupg");
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let out = Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args([
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-gen-key",
                "kapitan-test <kapitan-test@example.com>",
                "ed25519",
                "cert,sign",
                "0",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        // An encryption subkey.
        let ok = Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args([
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-add-key",
                &fingerprint_non_expired_in(&home, "kapitan-test"),
                "cv25519",
                "encr",
                "0",
            ])
            .status()
            .ok()?
            .success();
        ok.then_some(home)
    }

    fn fingerprint_non_expired_in(home: &std::path::Path, name: &str) -> String {
        // SAFETY: tests in this module run single threaded with respect to GNUPGHOME.
        unsafe { std::env::set_var("GNUPGHOME", home) };
        fingerprint_non_expired(name).unwrap()
    }

    #[test]
    // Saying why the test did nothing is the point; a skipped test that stays
    // silent looks like a passing one.
    #[allow(clippy::print_stderr)]
    fn round_trip_with_a_temporary_keyring() {
        let Some(home) = keyring() else {
            eprintln!("gpg not available; skipping");
            return;
        };
        // SAFETY: tests in this module run single threaded with respect to GNUPGHOME.
        unsafe { std::env::set_var("GNUPGHOME", &home) };
        let recipients = vec![serde_json::json!({"name": "kapitan-test"})];
        let fps = lookup_fingerprints(&recipients).unwrap();
        assert_eq!(fps.len(), 1);
        assert_eq!(fps[0].len(), 40);
        let ciphertext = encrypt(b"top secret\n", &fps).unwrap();
        assert!(!ciphertext.is_empty());
        assert_eq!(decrypt(&ciphertext).unwrap(), b"top secret\n");
        assert!(fingerprint_non_expired("nobody@example.com").is_err());
        let _ = Command::new("gpgconf")
            .env("GNUPGHOME", &home)
            .args(["--kill", "all"])
            .status();
        let _ = std::fs::remove_dir_all(home);
    }
}
