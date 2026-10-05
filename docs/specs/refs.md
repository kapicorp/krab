# Refs

Status: As-built
Verified against: main @ a4fb9c4
Code: `crates/krab-compile/src/refs/` (`mod.rs`, `functions.rs`, `gpg.rs`, `gkms.rs`, `kms_cli.rs`, `vault.rs`), `crates/krab-compile/src/output.rs` (`Writer::refs_str`, `Writer::refs_value`), the `reveal_maybe` filter in `crates/krab-compile/src/inputs/jinja.rs`, `crates/krab/src/cmd_refs.rs`, the ref options in `crates/krab/src/cmd_compile.rs`

## Problem

Kapitan inventories carry secrets as tags of the form `?{type:path}` instead
of as plaintext. The tag names a ref file under the refs path, and the ref
file holds the secret either encrypted (gpg, gkms, awskms, azkms,
vaulttransit), stored elsewhere (vaultkv keeps a pointer into Vault), or in
the clear (plain, base64, env). Compiling a target replaces each tag with a
short hashed form, with the whole ref embedded, or with the plaintext under
`--reveal`. A tag that names a missing ref and carries functions
(`?{base64:t/pw||random:str:12}`) creates that ref during compile.

The constraint is byte parity with kapitan 0.36.3. The hash, the embedded
payload and the ref file layout must match what kapitan writes, so that a
repository compiled by either tool yields the same compiled tree and either
tool can read the other's ref files. Revealed output and `krab refs` results
must match `kapitan refs` for the same input.

The native backend handles refs in Rust. `--backend python` passes `--reveal`
and `--embed-refs` through to kapitan's own refs code, which this spec does
not cover. External credentials come from the same places kapitan reads them
(application-default credentials, the gpg keyring, `VAULT_*`), except that
`awskms` and `azkms` call the `aws` and `az` command line clients where
kapitan uses boto3 and the Azure SDK.

## Requirements

### Tag syntax and compile

```
REF-1  A ref tag MUST be recognised by kapitan's tag pattern
       `(\?\{(\w+:[\w\-\.\@\=\/\:]+)(\|(?:(?:\|\w+)(?::\S*)*)+)?\=*\})`:
       a type, `:`, a path that may carry further `:`-separated parts and
       an `@sub.var` suffix, and an optional `||func:arg|func2` chain.
       Every match in a string MUST be replaced; text between tags MUST be
       kept unchanged.
       Test: crates/krab-compile/src/refs/mod.rs::plain_compiles_to_its_data_and_env_to_a_hash
       Since: 1b9c9c5

REF-2  Without --embed-refs, an existing ref of a type other than `plain`
       MUST compile to `?{type:path:hash}`, where hash is the first 8 hex
       digits of sha256 over the ref file's path relative to the refs path
       followed by its `data` field.
       Test: crates/krab-compile/src/refs/mod.rs::embeds_and_hashes
       Since: 1b9c9c5

REF-3  With --embed-refs (or `compile.embed-refs`), an existing ref of a
       type other than `plain` and `env` MUST compile to
       `?{type:<payload>:embedded}`, where payload is the standard base64 of
       the Python-style `json.dumps` of the ref file fields (REF-26), plus
       `embedded_subvar_path` when the tag names a sub-variable.
       Test: crates/krab-compile/src/refs/mod.rs::embeds_and_hashes
       Since: 1b9c9c5

REF-4  A `plain` ref MUST compile to its stored data, with or without
       --embed-refs. A `plain` tag with `@a.b` MUST compile to the value at
       that dotted path of the data parsed as YAML.
       Test: crates/krab-compile/src/refs/mod.rs::plain_compiles_to_its_data_and_env_to_a_hash
       Since: #71

REF-5  An `env` ref MUST compile to the hashed form of REF-2 even under
       --embed-refs.
       Test: crates/krab-compile/src/refs/mod.rs::plain_compiles_to_its_data_and_env_to_a_hash
       Since: 1b9c9c5

REF-6  A tag that carries a hash MUST be checked against the stored ref;
       a mismatch MUST fail with "token hash does not match with stored
       reference hash". A tag `?{type:payload:embedded}` MUST be read from
       its payload without touching the refs path.
       Test: crates/krab-compile/src/refs/mod.rs::embeds_and_hashes
       Since: #71

REF-8  Tag processing MUST apply to plain, YAML and JSON output of every
       native input type and to every file a `jinja2` input renders, after
       the content is produced and before it is written.
       Test: crates/krab/tests/helm_input.rs::helm_input_matches_the_reference
       Since: #71

REF-9  Every ref file a compile reads or creates MUST be recorded as a read
       of the target, so a change to it makes the target stale.
       Test: crates/krab-compile/src/refs/mod.rs::embeds_and_hashes
       Since: 1b9c9c5

REF-10 The compile refs path MUST be `compile.refs-path` from `.kapitan`,
       else `./refs`, relative to the repository root.
       Test: crates/krab/tests/helm_input.rs::helm_input_matches_the_reference
       Since: #71

REF-11 A ref file MUST be loaded through the backend the tag names, whatever
       `type` the file records. A missing ref file MUST be reported as a
       miss, not as an I/O error; other read errors MUST fail.
       Test: none
       Since: #71
```

### Reveal

```
REF-12 With --reveal (or `compile.reveal`), every tag in the output MUST be
       replaced by the ref's plaintext instead of its compiled form, and the
       jinja2 filter `reveal_maybe` MUST reveal its argument. Without
       --reveal, `reveal_maybe` MUST return its argument unchanged.
       Test: none
       Since: #71

REF-13 Revealing `?{type:path@a.b}` MUST reveal the ref, parse the plaintext
       as YAML and return the value at the dotted path; for a ref with
       `encoding: base64` the plaintext MUST be decoded before parsing and
       the value re-encoded after. An embedded payload with
       `embedded_subvar_path` MUST reveal the same way.
       Test: crates/krab-compile/src/refs/mod.rs::reveals_embedded_and_subvars
       Since: #71

REF-14 An `env` ref MUST reveal to the environment variable
       `KAPITAN_VAR_<name>`, where name is the last path segment without
       `@sub`, then to its upper-cased spelling, then to the stored data.
       Test: crates/krab-compile/src/refs/mod.rs::plain_compiles_to_its_data_and_env_to_a_hash
       Since: #71

REF-15 A `base64` ref MUST reveal to its decoded data, or to the stored field
       unchanged when the decoded bytes are not UTF-8.
       Test: crates/krab-compile/src/refs/mod.rs::reveals_embedded_and_subvars
       Since: #71

REF-16 Revealed plaintext MUST be cached per tag for the life of one ref
       controller, and every ref write MUST clear that cache.
       Test: none
       Since: #71
```

### Creating refs during compile

```
REF-17 A tag whose ref file is missing and that carries functions MUST
       create the ref: evaluate the chain (REF-22), store the result through
       the tag's backend with the target's `parameters.kapitan.secrets`,
       write the ref file under the refs path and compile the new ref. A
       missing ref without functions MUST fail with an error that names the
       refs path and suggests `krab refs --write <type>:<path> -f <file>`.
       Test: crates/krab-compile/src/refs/mod.rs::creates_refs_from_functions
       Since: #71

REF-18 An existing ref MUST be used as it is even when the tag carries
       functions; a second compile of the same tag MUST yield the same
       output.
       Test: crates/krab-compile/src/refs/mod.rs::creates_refs_from_functions
       Since: #71

REF-19 A tag with functions and an `@sub` path MUST fail with "references
       with sub-variables must be created manually".
       Test: crates/krab-compile/src/refs/mod.rs::creates_refs_from_functions
       Since: #71

REF-20 Ref creation MUST be serialised across the targets of one compile,
       and a ref another target created while this one waited MUST be used
       instead of being created again.
       Test: none
       Since: #71

REF-21 Inside a mapping, values containing `||reveal:` that fail MUST be
       retried after their sibling keys, for as many passes as make
       progress, so a ref MAY depend on a ref a sibling creates. A value that
       still fails after the last pass MUST fail the compile.
       Test: crates/krab-compile/src/refs/mod.rs::reveal_dependencies_between_keys_resolve_in_any_order
       Since: #71
```

### Functions

The chain after `||` is split on `|`; each element is `name:arg:arg`.
`base64` is a marker, not a function: it stores the final result base64
encoded with `encoding: base64`. All randomness comes from the operating
system generator (`OsRng`).

| Function | Arguments | Result |
|---|---|---|
| `random` | type (default `str`), length, special characters (`special` only) | random string from the pool of the type; default length 43 for `str`, 16 for `int`, 8 otherwise |
| `randomstr` | length | `random:str` |
| `loweralphanum` | length (default 8) | `random:loweralphanum` |
| `sha256` | salt | hex sha256 of `salt:data` |
| `ed25519` | none | PKCS#8 v1 PEM private key |
| `rsa` | key size (default 4096) | PKCS#8 PEM private key |
| `publickey`, `rsapublic` | none | SubjectPublicKeyInfo PEM of the RSA (PKCS#8 or PKCS#1) or Ed25519 private key in the data |
| `reveal` | ref path | plaintext of the ref of the same type at that path |
| `basicauth` | user, password (random 8 lowercase / 8 alphanumeric when empty) | base64 of `user:password` |

Random types and pools: `str` (letters, digits, `-_`), `int` (digits),
`loweralpha`, `upperalpha`, `loweralphanum`, `upperalphanum`, `special`
(letters, digits and the given characters, default Python's
`string.punctuation`). Pools keep only letters, digits and punctuation, each
once.

```
REF-22 The functions MUST behave as the table above states, with kapitan's
       error texts for an unknown function, an unknown random type, a
       non-integer length, special characters on a type other than
       `special`, too many arguments, and `sha256`/`publickey`/`rsapublic`
       with no data before them.
       Test: crates/krab-compile/src/refs/functions.rs::random_pools_and_lengths
       Since: #71

REF-23 `sha256`, `basicauth` and the `base64` marker MUST produce the values
       stated above.
       Test: crates/krab-compile/src/refs/functions.rs::sha256_and_basicauth_and_base64_marker
       Since: #71

REF-24 `rsa` and `ed25519` MUST produce PEM private keys starting
       `-----BEGIN PRIVATE KEY-----`, an Ed25519 key as a three-line PEM
       (PKCS#8 v1, as Python's `cryptography` writes it), and `publickey`
       MUST derive `-----BEGIN PUBLIC KEY-----` PEM from either.
       Test: crates/krab-compile/src/refs/functions.rs::keys_and_public_keys
       Since: #71

REF-25 `reveal:path` MUST read the ref at path with the tag's own type, and
       MUST fail naming the path when it does not exist. `publickey` after
       `reveal` of a ref with `encoding: base64` MUST decode it first.
       Test: crates/krab-compile/src/refs/mod.rs::reveal_dependencies_between_keys_resolve_in_any_order
       Since: #71
```

### Ref files

```
REF-26 A ref file MUST be a YAML mapping written with `yaml.safe_dump`'s
       layout, keys sorted: `data`, `encoding` (`original` or `base64`),
       `key` (KMS types, null when unset), `recipients` (gpg, a list of
       `{fingerprint: F}`), `type`, `vault_params` (Vault types).
       Test: crates/krab-compile/src/refs/mod.rs::ref_file_format_matches_pyyaml
       Since: #71

REF-27 A ref file without `type` MUST be read as `gpg`, as kapitan reads files
       written before it recorded the type.
       Test: none
       Since: #71
```

### Backends

The tag type selects the backend. What each stores and what it calls:

| Type | Stores in `data` | Encrypts or stores through | Creation input from `parameters.kapitan.secrets` |
|---|---|---|---|
| `plain` | text as is | nothing | none |
| `env` | default text as is | nothing | none |
| `base64` | base64 of the payload | nothing | none |
| `gpg` | base64 ciphertext | `gpg` binary (`GPGBINARY`), python-gnupg's flags, `--encrypt --sign` | `gpg.recipients` |
| `gkms` | base64 ciphertext | Cloud KMS REST API | `gkms.key` |
| `awskms` | base64 ciphertext | `aws kms encrypt/decrypt` | `awskms.key` |
| `azkms` | base64 ciphertext | `az keyvault key encrypt/decrypt`, RSA-OAEP-256 | `azkms.key` |
| `vaultkv` | base64 of `path/in/vault:key` | Vault KV v1 or v2 HTTP API | `vaultkv` |
| `vaulttransit` | base64 of the `vault:v1:...` ciphertext | Vault transit HTTP API | `vaulttransit` (`crypto_key` required) |

Credential sources for gkms, gpg and Vault are listed in
[CLI.md](../CLI.md#krab-refs).

```
REF-28 Every type in the table MUST be accepted in tags, ref files and
       `krab refs`; any other type MUST fail with "no backend for ref type"
       in a tag and with "Invalid token type" in `krab refs`.
       Test: none
       Since: #71

REF-29 With the KMS key `mock`, `gkms`, `awskms` and `azkms` MUST reveal to
       `mock` without a network call or subprocess, as kapitan's test double
       does.
       Test: crates/krab-compile/src/refs/mod.rs::reveals_embedded_and_subvars
       Since: #71

REF-30 `gpg` MUST encrypt for every recipient fingerprint and sign with the
       default key in the same `gpg` call, and MUST pass plaintext and
       ciphertext over stdin and stdout. Recipients given as `{name: N}`
       MUST resolve to the first non-expired key's fingerprint; recipients
       MUST be stored sorted and without duplicates.
       Test: crates/krab-compile/src/refs/gpg.rs::round_trip_with_a_temporary_keyring
       Since: #71

REF-31 `gkms` MUST obtain an access token from, in order,
       `GOOGLE_OAUTH_ACCESS_TOKEN`, the application-default credentials file
       (`authorized_user` refresh or `service_account` JWT), the GCE
       metadata server (only when there is no credentials file), and
       `gcloud auth application-default print-access-token`, and MUST reuse
       a token until 60 seconds before it expires. Requests MUST time out
       after 60 seconds, the metadata probe after 2.
       Test: none
       Since: #71

REF-32 `vault_params` MUST hold all thirteen fields of kapitan's model in its
       order, filled from the inventory, then from `VAULT_*` variables, then
       from kapitan's defaults (`skip_verify: true`, `engine` and `mount`
       `kv-v2`/`secret` for vaultkv and `transit`/`transit` for
       vaulttransit, `always_latest: false`).
       Test: crates/krab-compile/src/refs/vault.rs::normalizes_params_like_kapitans_model
       Since: #71

REF-33 Vault MUST authenticate with `auth` set to `token`, `github`, `ldap`,
       `userpass` or `approle`, MUST confirm the token with
       `auth/token/lookup-self`, and MUST refuse a `~/.vault-token` that is a
       symbolic link. With `skip_verify` false a CA bundle (`cacert` or
       `capath`) MUST be given.
       Test: crates/krab-compile/src/refs/vault.rs::kv_and_transit_round_trip_against_a_mock_server
       Since: #71

REF-34 Creating a `vaultkv` ref MUST take a five-part token
       `vaultkv:path:mount:path/in/vault:key`, MUST fail when key is empty,
       MUST default mount to `vault_params.mount` and the Vault path to the
       ref path, and MUST merge the new key into the existing secret.
       Test: crates/krab-compile/src/refs/vault.rs::kv_and_transit_round_trip_against_a_mock_server
       Since: #71

REF-35 Revealing `vaulttransit` with `always_latest: true` MUST rewrap the
       ciphertext before decrypting it.
       Test: none
       Since: #71
```

### `krab refs`

Flags are listed in [CLI.md](../CLI.md#krab-refs).

```
REF-36 The refs path for `krab refs` MUST be --refs-path, else
       `refs.refs-path` from `.kapitan`, else `./refs`, relative to the
       working directory.
       Test: crates/krab/tests/refs_write.rs::write_refuses_to_overwrite_without_force
       Since: #71

REF-7  `krab refs --write` MUST NOT replace an existing ref file unless
       --force is given.
       Test: crates/krab/tests/refs_write.rs::write_refuses_to_overwrite_without_force
       Since: #158

REF-37 A symbolic link at the ref path, dangling or not, MUST count as an
       existing ref for REF-7, and nothing MUST be written through it.
       Test: crates/krab/tests/refs_write.rs::write_refuses_a_dangling_symlink_without_force
       Since: #158

REF-38 The REF-7 check MUST run before any encryption, KMS, GPG or Vault
       call.
       Test: none
       Since: #158

REF-39 `--write` MUST read --file (`-` for stdin), MUST refuse content that
       is not UTF-8 unless --binary is given, and with --base64 MUST store
       the base64 of the content with `encoding: base64`.
       Test: none
       Since: #71

REF-40 With --target-name the target's `parameters.kapitan.secrets` MUST
       exist. For --write the KMS key MUST be --key, else the target's; for
       --update the target's key MUST win over --key. GPG recipients MUST be
       the target's when it declares any, else --recipients. For `vaultkv`
       the target's mount MUST win over --vault-mount, --vault-path MUST
       default to the ref path, --vault-key MUST be given, and an `auth`
       from the target or --vault-auth MUST be present.
       Test: none
       Since: #71

REF-41 `--reveal` MUST reveal, in this order of precedence: stdin when
       --file or --ref-file is `-` (line by line, as text with tags), the
       file or directory in --file, the ref file in --ref-file, the tag in
       --tag. A `.yml`/`.yaml` file MUST be written back as YAML documents
       each starting `---`, a `.json` file as indented JSON, anything else
       as text. A directory MUST yield the concatenation of its YAML files,
       else of its JSON files, else of its other files, in sorted path
       order.
       Test: none
       Since: #71

REF-42 `--update <type:path>` MUST re-encrypt a `gpg`, `gkms`, `awskms` or
       `azkms` ref with the new recipients or key, keeping its encoding, MUST
       leave the file untouched when they are unchanged, and MUST refuse
       every other type.
       Test: none
       Since: #71

REF-43 `--update-targets` and `--validate-targets` MUST consider the ref files
       under `<refs-path>/<target>/` of every target, comparing gpg
       recipients, KMS keys and the vaulttransit `key` against the target's
       `parameters.kapitan.secrets`. `--update-targets` MUST re-encrypt the
       mismatches; `--validate-targets` MUST report each one on stderr and
       exit with code 1 when there is at least one. Refs of other types
       MUST be reported on stderr as skipped. A gpg or KMS ref whose target
       declares no recipients or key MUST be left alone.
       Test: none
       Since: #71

REF-44 `krab refs` without --write, --reveal, --update, --update-targets or
       --validate-targets MUST fail with a message naming them.
       Test: none
       Since: #71
```

### Not met yet

These requirements describe the behaviour krab is meant to have. `main` does
not meet them yet; the open deviations below say what it does instead.

```
REF-45 Creating or updating a ref file MUST be atomic: an interruption
       leaves the previous file or the new one.
       Test: none
       Since: not met yet (#144)

REF-46 No backend MUST place plaintext in a subprocess argument.
       Test: none
       Since: not met yet (#144)

REF-47 Plaintext MUST NOT be left in a file a killed process cannot
       remove.
       Test: none
       Since: not met yet (#144)

REF-48 `vaultkv` creation MUST NOT leave a Vault value without a ref
       file.
       Test: none
       Since: not met yet (#144)

REF-49 Every backend MUST pass one shared contract test suite that runs
       without `gpg`, `aws`, `az`, `gcloud` or network access.
       Test: none
       Since: not met yet (#144)

REF-50 When neither the inventory nor `VAULT_SKIP_VERIFY` sets
       `skip_verify`, Vault requests MUST verify the server certificate
       against the system roots and, when verification fails, warn and
       continue unverified. An explicit `skip_verify: true` MUST skip
       verification without a warning. The ref file keeps the
       `skip_verify` default of REF-32.
       Test: none
       Since: not met yet (#216)
```

## Acceptance criteria

1. Given a ref file `refs/h/s` of type `base64` and a helm input whose values
   contain `?{base64:h/s}`, when the target compiles with default settings,
   then the rendered manifest contains `?{base64:h/s:d7299a4d}`. Covers
   REF-2, REF-8, REF-10. Test:
   `crates/krab/tests/helm_input.rs::helm_input_matches_the_reference`.
2. Given an empty refs path, when `?{base64:t/pw||random:str:12}` is compiled
   twice, then the first compile writes `t/pw` with `encoding: original` and
   `type: base64`, both compiles produce the same hashed tag, and revealing it
   yields 12 characters. Covers REF-17, REF-18, REF-26. Test:
   `crates/krab-compile/src/refs/mod.rs::creates_refs_from_functions`.
3. Given a mapping with `pub: ?{base64:k/pub||reveal:k/priv|publickey}`
   listed before `priv: ?{base64:k/priv||rsa:1024}`, when it compiles, then
   both refs exist and `pub` reveals to the public key of `priv`. Covers
   REF-21, REF-24, REF-25. Test:
   `crates/krab-compile/src/refs/mod.rs::reveal_dependencies_between_keys_resolve_in_any_order`.
4. Given a mock Vault with token auth, when
   `?{vaultkv:t/db/pw:secret:app/db:password||random:str:10}` and a second
   key of the same secret are created, then the ref file stores
   `app/db:password`, both keys reveal, and a bad `VAULT_TOKEN` fails with
   "Authentication Error". Covers REF-32, REF-33, REF-34. Test:
   `crates/krab-compile/src/refs/vault.rs::kv_and_transit_round_trip_against_a_mock_server`.
5. Given `refs/x` written by `krab refs --write plain:x -f a`, when the same
   command runs with `-f b`, then it fails naming --force and `refs/x` still
   holds `a`'s content; with --force it holds `b`'s. Covers REF-7, REF-36.
   Test: `crates/krab/tests/refs_write.rs::write_refuses_to_overwrite_without_force`.

## Edge cases

- REF-EC-1 Ref file missing, tag without functions: the compile fails with
  the REF-17 message.
- REF-EC-2 Tag hash differs from the stored ref: the compile fails (REF-6).
- REF-EC-3 Ref file is not a YAML mapping, has no `data`, or records an
  unknown `type`: the compile fails naming the file.
- REF-EC-4 Functions produce no data (for example only `|base64`): the
  compile fails with "generated no data; try something like ||random:str".
- REF-EC-5 Sub-variable path absent or the revealed secret is not a YAML
  mapping: reveal fails with "cannot access ... sub-variable key".
- REF-EC-6 `gpg` not installed: "cannot run gpg ... (is GnuPG installed?)".
  `aws` or `az` absent: "cannot run". No Google credentials anywhere: gkms
  fails listing each source it tried and suggesting
  `gcloud auth application-default login`.
- REF-EC-7 A gpg recipient name with only expired keys: "Could not find
  valid key for recipient".
- REF-EC-8 Two targets compile in parallel and both need the same missing
  ref: it is created once (REF-20).
- REF-EC-9 The process is interrupted while a ref file is written: the file
  can be left truncated (REF-45, open).
- REF-EC-10 Writing the ref file fails after a `vaultkv` creation stored the
  value in Vault: the Vault value is orphaned and the next compile writes a
  new random value over it (REF-48, open).

## Interfaces

- Tag grammar: REF-1. Compiled forms: `?{type:path:hash8}` (REF-2) and
  `?{type:base64(json):embedded}` (REF-3).
- Ref file format: REF-26. kapitan 0.36.3 reads files krab writes and the
  reverse.
- `KAPITAN_VAR_<name>` for `env` refs (REF-14); `GPGBINARY` selects the gpg
  binary; `GOOGLE_OAUTH_ACCESS_TOKEN`, `GOOGLE_APPLICATION_CREDENTIALS`,
  `CLOUDSDK_CONFIG` for gkms; `VAULT_ADDR`, `VAULT_TOKEN`,
  `VAULT_USERNAME`, `VAULT_PASSWORD`, `VAULT_ROLE_ID`, `VAULT_SECRET_ID`,
  `VAULT_SKIP_VERIFY`, `VAULT_CACERT`, `VAULT_CAPATH`, `VAULT_CLIENT_CERT`,
  `VAULT_CLIENT_KEY`, `VAULT_NAMESPACE` for Vault.
- `.kapitan` keys `compile.refs-path`, `compile.embed-refs`,
  `compile.reveal`, `refs.refs-path`: see [CLI.md](../CLI.md#kapitan).
- Exit codes: `krab refs` exits 1 on any error and on a mismatch found by
  `--validate-targets`.

## Out of scope

- Refs under `--backend python`: kapitan's own code handles them.
- Replacing the `aws` and `az` CLIs with SDKs. The CLIs carry the
  credential handling, and an SDK is a separate dependency decision
  ([exec plan for #144](../exec-plans/144-secrets-backend-contract.md),
  "Exclusions").
- Zeroizing plaintext in memory. kapitan does not do it either.

## Open deviations

The plan for the #144 rows is
[docs/exec-plans/144-secrets-backend-contract.md](../exec-plans/144-secrets-backend-contract.md).

| Requirement | Issue | What `main` does |
|---|---|---|
| REF-45 | #144 | `RefController::write` calls `std::fs::write` on the final path (`refs/mod.rs:963`), so `--update` and `--update-targets` can truncate the only copy of an encrypted secret. (F13) |
| REF-46 | #144 | `az_crypto` passes the base64 plaintext as the `--value` argument of `az keyvault key encrypt` (`refs/kms_cli.rs:112`), visible in the process list. (F7) |
| REF-47 | #144 | `aws_encrypt` writes the plaintext to a mode 0600 temp file in `TMPDIR`, removed on `Drop`, which does not run on `SIGKILL` or `panic=abort`. (F8) |
| REF-48 | #144 | `write_vaultkv` writes to Vault, then `create()` writes the ref file; a failed file write orphans the Vault value. (F15) |
| REF-49 | #144 | `gkms`, `awskms` and `azkms` have no tests beyond the `mock` key; `gpg` and Vault tests depend on a local keyring and a mock server. |
| REF-50 | #216 | An unset `skip_verify` takes kapitan's default `true` (`refs/vault.rs:41-46,142`), so every Vault request skips certificate verification without a word; the system roots are never used |
