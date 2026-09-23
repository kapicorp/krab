# ExecPlan: every secrets backend obeys one tested contract

Status: Draft, awaiting review. Issue filed.
Issue: [#144](https://github.com/kapicorp/krab/issues/144)
Baseline: `6e9e55ba18597b34ab056cfe29cb28698fa97993`

## Purpose and acceptance criteria

`RefController` dispatches to eight ref backends (`plain`, `base64`, `env`,
`gpg`, `gkms`, `vault` for KV and transit, `awskms`, `azkms`). Each invents its
own plaintext handling and its own write path. That inconsistency has produced
four defects of one shape:

| | Defect | Location |
|---|---|---|
| F7 | `azkms` passes base64 of the **plaintext** as the `--value` argv element to `az`, so it is readable via `ps` and `/proc/<pid>/cmdline` by any local user | `refs/kms_cli.rs`, `az_crypto` / `az_encrypt` |
| F8 | `awskms` writes the plaintext to a temp file in `TMPDIR`. Created `create_new` mode `0600` and unlinked on `Drop`, but never overwritten, and `Drop` does not run on `SIGKILL` or `panic=abort` | `refs/kms_cli.rs`, `aws_encrypt` |
| F13 | Ref files are written with a plain `std::fs::write`. `krab refs --update` and `--update-targets` decrypt and rewrite in place, so an interruption truncates the only copy of an encrypted secret | `refs/mod.rs`, `RefController::write` |
| F15 | `vaultkv` pushes the secret to Vault **before** writing the ref file. If the file write fails the Vault value is orphaned, and the next run generates a fresh random secret and overwrites that Vault key | `refs/mod.rs`, `create()` |

The reason all four exist in the same place is the same: three of the eight
backends have **no tests at all**. `gpg` and `vault` have them because a fake was
possible from outside (a temporary keyring, a mock HTTP server). `gkms`,
`awskms` and `azkms` have none, because nothing in the code offers a seam.

The outcome is one internal `Backend` trait used purely as a contract-test seam,
one suite every backend passes, and those four defects closed by construction
rather than one at a time.

**This is not a plugin API.** No dynamic loading, nothing published, no stability
promise to outside implementers. If the change starts growing toward one, stop
and re-read this paragraph.

Acceptance criteria, all observable:

1. A single parameterised test suite runs against every backend, including an
   in-memory one, and passes without `gpg`, `aws`, `az`, `gcloud` or network
   access being present.
2. No backend places plaintext in a subprocess argument. A test asserts this by
   inspecting the constructed argv, not by reading the code.
3. Creating or updating a ref file is atomic: an interruption leaves either the
   previous file or the new one, never a truncated one. A test asserts it.
4. `vaultkv` writes the ref file before the remote value is treated as
   committed, or fails in a way that does not orphan it. A test asserts the
   ordering.
5. `refs/` contains no backend with zero tests.
6. **The ref file format is byte-identical to before the change.**
   `ref_file_format_matches_pyyaml` passes unmodified.
7. Each of F7, F8, F13, F15 has a test that fails on the baseline commit and
   passes after.

## Context and constraints

### Code paths

* `crates/krab-compile/src/refs/mod.rs`: `RefController`, the ref-type
  dispatch, `write`, `create`, the parsed-file and revealed caches.
* `crates/krab-compile/src/refs/kms_cli.rs`: `aws_encrypt`/`aws_decrypt` via a
  temp file, `az_crypto` via `--value`, the shared `run_json` and `TempFile`.
* `crates/krab-compile/src/refs/gpg.rs`: the one backend that already does it
  right: plaintext over **stdin** through a writer thread.
* `crates/krab-compile/src/refs/gkms.rs`, `vault.rs`, `functions.rs`.

### Contracts to reuse rather than reinvent

* **`gpg.rs`'s stdin handling is the model** for getting plaintext out of argv.
  Copy its shape rather than inventing another.
* **`Manifest::save` already does tmp-write-plus-rename** in
  `crates/krab-compile/src/manifest.rs`. Atomic ref writes should use the same
  pattern, not a new one.
* `RefError` and the existing error strings. Several deliberately match the
  reference's wording; do not reword them while moving code.
* The `mock` key the reference honours for tests is already respected
  (`az_encrypt` and friends short-circuit on `key == "mock"`). The in-memory
  backend should build on that rather than adding a parallel concept.

### Compatibility obligations

* **The ref file format must not change.** It is `yaml.safe_dump`'s layout so
  that kapitan and krab can read each other's files. This milestone moves *when
  and how* a file is written, never *what* is in it.
* The tag grammar (`?{type:path:hash}`, embedded payloads, `||reveal:`) is
  untouched.
* `parameters.kapitan.secrets` handling and ref creation from functions
  (`random`, `sha256`, `rsa`, `ed25519`, `publickey`, `reveal`, `basicauth`,
  `base64`) keep their current semantics.

### A compatibility question that is already settled

**Fixing F7 carries no parity obligation.** I checked the reference: it uses
SDKs, not CLIs. `kapitan/refs/secrets/awskms.py` uses `boto3`;
`azkms.py` uses `azure.keyvault.keys.crypto.CryptographyClient` with
`DefaultAzureCredential`. A grep for `subprocess`/`Popen`/`check_output` across
`kapitan/refs/` returns nothing.

So the reference has no argv behaviour to be compatible with, and krab's use of
the `aws` and `az` binaries is *already* a deliberate divergence, one that
`refs/kms_cli.rs`'s own module doc acknowledges ("which carry the credential
handling kapitan gets from boto3 and `DefaultAzureCredential`"). Two consequences:

1. Changing how the plaintext reaches the CLI needs no `docs/DECISIONS.md` row,
   because it is not a new divergence.
2. The CLI-versus-SDK divergence itself probably *should* have a row and appears
   not to have one. Adding it is in scope for this plan; replacing the CLIs with
   SDKs is not.

### Exclusions, deliberate

* **Replacing the CLI backends with Rust SDKs.** That is a large dependency
  addition and a separate decision. Out of scope.
* **F14** (`krab refs --write` overwriting an existing ref with no prompt and no
  `--force`). It is a CLI policy question, lives in `cmd_refs.rs`, and belongs
  in its own change.
* **F9** (no HTTP body timeouts or size caps). It affects `fetch.rs` and
  `oci.rs` far more than `refs/`; fixing it only in `vault.rs`/`gkms.rs` would
  be half a fix.
* **Zeroizing plaintext in memory.** Real, but a different and larger change,
  and the reference does not do it either. Record it as a limitation.
* Vault's `skip_verify` defaulting to true. Verified as inherited parity with
  the reference's own model default; changing it is a deliberate divergence and
  needs its own decision.
* Any public or dynamically loaded plugin interface.

### Prerequisites

* None. The four defects are public in
  [#144](https://github.com/kapicorp/krab/issues/144). **This plan touches no file that any currently open pull request
  touches**, so it can run in parallel with the PR queue. Re-check that at start:
  `gh pr list --repo kapicorp/krab --state open` and confirm none touches
  `crates/krab-compile/src/refs/`.

## Milestones and work

### Milestone 1: the seam exists and every backend is behind it

Reviewable outcome: a `Backend` trait over the existing modules, with the
dispatch in `RefController` going through it, and **no behaviour change**. All
existing tests pass untouched.

Shape to aim for, not a specification:

```rust
trait Backend {
    fn encrypt(&self, plaintext: &[u8], key: &str) -> Result<Vec<u8>, RefError>;
    fn decrypt(&self, ciphertext: &[u8], key: &str) -> Result<Vec<u8>, RefError>;
}
```

Two uncertainties to resolve here, before writing much code:

* **Do all eight backends actually fit one trait?** `plain` and `base64` are
  in-process and keyless. `env` cannot fail and falls back to stored data.
  `vault` has two modes (KV and transit) with different parameters. If the trait
  needs more than two methods, or if a backend needs an escape hatch, that is a
  signal the seam is in the wrong place. **Record the answer in Discoveries
  before proceeding to Milestone 2.** A narrower trait covering only the four
  backends that actually encrypt is an acceptable outcome; say so if that is
  what happens.
* **Where does the key parameter belong?** Today key handling is spread across
  `cmd_refs.rs` (precedence rules), `mod.rs` and each backend. Do not try to fix
  that here; take the key as a parameter and leave the precedence rules where
  they are.

### Milestone 2: an in-memory backend, and the suite runs everywhere

Reviewable outcome: a test backend plus a parameterised suite that runs without
any external binary or network. `cargo test -p krab-compile refs` covers every
backend.

Non-negotiable: **the suite must not skip.** This repository already has three
tests that report pass while doing nothing and a fourth whose guard does not
match its name. Adding a fifth would defeat the acceptance criteria. If a
backend genuinely cannot be exercised without credentials, its *argv
construction* and *write path* must still be covered, because those are where
F7, F8 and F13 live.

### Milestone 3: plaintext leaves argv and the temp file (F7, F8)

Reviewable outcome: `azkms` no longer passes the secret as an argument, and
`awskms` no longer writes it to a file it does not overwrite.

**The main implementation uncertainty in this plan.** `az keyvault key encrypt`
is invoked with `--value <base64>`. Whether the Azure CLI accepts that value
from a file or stdin at all needs checking first: `az` supports an `@file`
convention for some parameters, and if `--value` is one of them the fix is
small. If it is not, the options are a temp file with the F8 treatment applied
properly, or accepting the exposure and documenting it. **Resolve this before
estimating the rest of the milestone**, and record the finding either way.

For `awskms`, the temp file can stay if it is overwritten before unlink and the
process installs a handler so abnormal termination does not leave it. Note that
F16 (no signal handling anywhere in the compile path) is a separate finding; do
not build a general signal framework here.

### Milestone 4: writes are atomic and correctly ordered (F13, F15)

Reviewable outcome: `RefController::write` writes to a temporary file in the
same directory and renames; `vaultkv` creation writes the ref file before the
remote value is considered committed.

For F15 specifically, decide and record which of these is wanted:

* write the ref file first, then push to Vault, and delete the file if the push
  fails; or
* push to Vault, write the file, and on write failure attempt to remove the
  Vault value.

Neither is atomic across two systems. The first fails safer. Whichever is
chosen, the failure mode that must not survive is the current one, where a
later run silently overwrites a live Vault key with a fresh random secret.

### Milestone 5: record what changed

Reviewable outcome: a `docs/DECISIONS.md` row for the CLI-versus-SDK divergence
if one does not already exist, and a note in `docs/DESIGN.md` only if the
backend semantics changed, which they should not have.

## Validation and recovery

### Commands

```sh
cargo test -p krab-compile refs      # must go from 21 tests to more, none skipping
cargo test --locked --no-fail-fast   # whole suite, no regression
cargo clippy --all-targets --locked  # warnings are denied
cargo fmt --all --check
```

Required environment: none beyond a Rust toolchain. No network, no cloud
credentials, no `gpg`. That is the point of Milestone 2.

Note for anyone running the full suite locally on a machine with commit signing
enabled: `cargo test` currently fails in `fetch.rs` for an unrelated reason
(issue #98). PR #99 fixes it. Until that lands, use
`GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo test --locked`.

### Expected results

* Every acceptance criterion in the first section has a named test.
* The four defect tests fail when run against the baseline commit. **Verify that
  direction explicitly**: a test that passes before and after proves nothing.
* `ref_file_format_matches_pyyaml` passes without modification. If it needed
  changing, the format moved and the change is wrong.

### Coverage check

Print or assert the number of backends the suite covered, and fail if it is
lower than expected. The specific failure to prevent is a guard that silently
excludes a backend, which is how `gkms`, `awskms` and `azkms` ended up untested.

### Recovery

The trait and the tests revert cleanly in one commit. Three effects do not:

1. **Ref files written during a partially applied change** keep whatever layout
   they were written with. This is why the format must not move in this
   milestone; if it does, a revert leaves unreadable files behind.
2. **A changed `az` invocation** is a change in how krab drives an external
   binary. If it turns out wrong for some Azure CLI version, the symptom is a
   failed encrypt at compile time, not silent corruption. Check the invocation
   against at least one real `az` version before merge, and say which in
   Outcomes.
3. **Any Vault value already orphaned by the F15 bug** is not cleaned up by this
   change. Fixing the code does not fix data. Say so in the issue so operators
   can check for stray keys.

### Fallback

If Milestone 1 shows the eight backends do not fit one trait, **do not force
it**. Ship the narrower version: the atomic write (F13), the argv fix (F7), the
`vaultkv` ordering (F15), and tests for whichever backends the seam does reach.
Record the trait's failure in Discoveries with the reason. A partial fix with an
honest note is better than an abstraction bent to fit.

## Progress

- [ ] M1: confirm no open PR touches `refs/`
- [ ] M1: decide whether one trait fits all eight backends; record the answer
- [ ] M1: trait in place, dispatch routed through it, no behaviour change
- [ ] M2: in-memory backend
- [ ] M2: parameterised suite, runs with no external binary or network
- [ ] M2: assert the suite cannot silently skip a backend
- [ ] M3: resolve whether `az` can take `--value` off the command line
- [ ] M3: F7 closed, with a test asserting no plaintext in argv
- [ ] M3: F8 closed
- [ ] M4: F13 closed, atomic write, with an interruption test
- [ ] M4: decide and record the F15 ordering; close it
- [ ] M5: `docs/DECISIONS.md` row for CLI versus SDK, if absent
- [ ] Each of the four defect tests verified to fail on the baseline commit
- [ ] Full suite, clippy, fmt clean

## Discoveries and decisions

*Nothing recorded yet. This plan has not been executed.*

What is already known is in Purpose and Context above; it is not repeated
here.

Record here: whether one trait fitted; what `az` turned out to support; the F15
ordering choice and why; and anything that made the seam look wrong.

## Outcomes and handoff

*Not started. Nothing to record.*

On completion, record: which backends the suite covers and which it does not;
the `az` version the new invocation was checked against; the exact test names
added and their output; whether each of the four defect tests was confirmed to
fail on the baseline; and the remaining limitations, which at minimum include
plaintext not being zeroized in memory and the process-lifetime `revealed`
cache in `RefController`.

The natural next work after this plan is F14 (`refs --write` overwrite
protection) and F9 (HTTP timeouts and size caps), both in M7 of the assessment.
Neither is blocked by this plan.
