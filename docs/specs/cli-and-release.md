# CLI, CI and releases

```
Status: As-built
Verified against: main @ a4fb9c4
Code: crates/krab/src/main.rs, crates/krab/src/app.rs, crates/krab/src/report.rs,
      crates/krab/src/completions.rs, crates/krab/src/explain.rs,
      crates/krab/tests/{cli_docs,docs_consistency,readme_version,completions,explain_missing}.rs,
      .github/, Cargo.toml, deny.toml, rust-toolchain.toml, cliff.toml, lychee.toml
```

## Problem

krab is driven by people at a shell, by scripts and CI pipelines that parse its
output, by the VS Code extension through `krab lsp`, and by coding agents. A
script needs three things from a command line: results on one stream and
complaints on the other, an exit status it can branch on, and a machine-readable
form of both. krab gives every command a global `--json` switch for the last of
these and keeps the human rendering (miette snippets) for the default. The
command and flag reference is the hand-written [CLI.md](../CLI.md); this spec
states the contract around it and does not repeat the flags.

The second half of the area is how the repository proves its claims before a
change lands and how it ships. The central claim is byte parity with kapitan
0.36.3 on the omegaconf backend. The fixture expectations that encode it are
regenerated in CI by the reference implementation itself, so a hand-written
expectation cannot pass. Releases are the only distribution channel: the
workspace patches `saphyr-parser` through `[patch.crates-io]`, cargo drops that
patch when packaging, and a published crate would silently lose the PyYAML
compatibility fixes. Prebuilt binaries for Linux and macOS, with checksums and
a build provenance attestation, are therefore what users install.

Several facts are written down in more than one place: the reference version,
the release version, the flag list. Each duplicate either has a test comparing
it to its source or is derived at the point of use. Where neither holds, the
gap is listed under Open deviations.

Constraints that shaped the area: contributors work from forks and cannot
merge; the `main` ruleset is not enforced (#171), so CI informs a maintainer
rather than blocking a merge; and a pull request should fail only for what it
changed. Link checking therefore runs weekly instead of per pull request (link
rot happens on other servers), the dependency policy runs only when the
dependency set or the policy changed, and a newly published advisory is
reported without failing the job.

## Requirements

### Command surface

```text
CLI-1  The workspace MUST build one binary, named krab, from crates/krab.
       Test: crates/krab/tests/cli_docs.rs::every_command_and_flag_is_in_the_cli_reference
             (every integration test runs env!("CARGO_BIN_EXE_krab"))
       Since: #95

CLI-2  krab MUST provide the top-level commands inventory (alias i), compile
       (alias c), refs, server, lsp and completions, and MUST accept the
       global options --inventory-path, --json, --raw and --no-daemon before
       or after the subcommand. -V/--version MUST print "krab <workspace
       version>".
       Test: none
       Since: 319ca84 (inventory), b3c122e (server), 58f8ddd (compile,
              completions), 627c332 (lsp), #71 (refs)

CLI-3  When KRAB_PYTHON, KRAB_PYTHON_REQUIREMENTS, KRAB_NO_DAEMON or
       KRAB_INVENTORY_PATH is unset and the matching KAPITAN_* variable is
       set, krab MUST copy the value to the KRAB_* name before parsing
       arguments and print "note: <old> is now <new>; the old name still
       works in this release" on stderr.
       Test: none
       Since: #95

CLI-4  KRAB_NO_DAEMON MUST parse with clap's falsey parser: 0, false, no,
       off, n, f and the empty string mean off, any other value means on.
       Test: none
       Since: #95

CLI-5  krab lsp MUST refuse to start under --no-daemon or --raw, with
       "error: the language server needs the inventory daemon; drop
       --no-daemon / --raw" and exit status 1.
       Test: none
       Since: 627c332
```

### Output streams and exit status

```text
CLI-6  Command results MUST go to stdout. Without --json, diagnostics MUST be
       rendered with miette on stderr (warnings prefixed "warning: "), and a
       failing command MUST end with "<n> error" or "<n> errors" on stderr.
       Test: none
       Since: 319ca84

CLI-7  Warnings about the configuration itself (the .kapitan backend warning)
       MUST be printed once per invocation on stderr as plain text, also
       under --json, and MUST NOT change the exit status.
       Test: crates/krab/tests/backend_warning.rs::warns_when_the_backend_is_not_omegaconf
       Since: #161

CLI-8  With --json, every diagnostic, error or warning, MUST be printed to
       stdout as one compact JSON object per line in the shape given under
       Interfaces, and the "<n> errors" summary MUST NOT be printed.
       Test: none
       Since: 319ca84

CLI-9  With --json, command results MUST be JSON on stdout in the forms
       listed in CLI.md, section "JSON output".
       Test: none
       Since: 319ca84

CLI-10 A failure that is not a diagnostic (an invalid --labels value, an I/O
       error, an LSP start refusal) MUST print "error: <message>" on stderr as
       plain text, with or without --json.
       Test: none
       Since: 319ca84

CLI-11 The exit status MUST be 0 on success and 1 when the command fails or
       any target it rendered or compiled has an error. Argument errors
       detected by clap exit with 2; --help and --version exit with 0.
       Test: crates/krab/tests/incremental.rs::a_target_that_failed_fails_again
             (asserts a non-zero status only)
       Since: 319ca84

CLI-12 Log output MUST go to stderr, filtered by RUST_LOG, with the default
       filter "warn", or "info" for krab server run.
       Test: none
       Since: 319ca84, b3c122e

CLI-13 krab MUST restore the default SIGPIPE disposition at the start of main,
       so that writing to a closed pipe ends the process by the signal
       instead of a panic.
       Test: none
       Since: 000035c
```

### inventory explain

```text
CLI-14 krab inventory explain -t <target> <path> MUST take <path> relative to
       parameters (a leading "parameters." is stripped) and print the value,
       its type, where it was written (file:line:col, or "written by kapitan
       (synthetic)"), the interpolation it was resolved from with the
       referenced path and its origin, the override, list-append and
       dereference history oldest first, and the number of events under the
       path that are not shown.
       Test: crates/krab/tests/explain_missing.rs::explain_shows_where_a_missing_value_came_from
       Since: 319ca84, #175 (values that became ???)

CLI-15 With --json, inventory explain MUST print the Explanation object
       described under Interfaces, pretty-printed.
       Test: none
       Since: 319ca84

CLI-16 A path with no value MUST fail with the diagnostic
       inventory::path_not_found and exit status 1.
       Test: none
       Since: 319ca84
```

### Shell completions

```text
CLI-17 krab completions <bash|zsh|fish|elvish|powershell> MUST print a
       registration script on stdout. Subcommands, flags and target names
       are completed at run time by the binary through the COMPLETE
       environment variable, so the script does not go stale when the CLI
       changes.
       Test: crates/krab/tests/completions.rs::registration_uses_the_invoked_name
       Since: 58f8ddd

CLI-18 The script MUST register the name the binary was invoked as (argv[0]),
       not the target of a symlink. A bare name MUST be left for PATH lookup,
       a relative path MUST be made absolute against the current directory,
       and an absolute path MUST be kept.
       Test: crates/krab/tests/completions.rs::registration_uses_the_invoked_name,
             crates/krab/tests/completions.rs::registration_with_a_bare_name_leaves_lookup_to_path,
             crates/krab/src/completions.rs::tests::{bare_name_is_kept_for_path_lookup,
             relative_path_is_anchored_to_cwd, absolute_path_is_unchanged}
       Since: #2

CLI-19 Target names for -t/--target-name MUST be completed from a directory
       walk of the inventory named by .kapitan in the current directory (else
       ./inventory), without rendering and without contacting or starting
       the inventory server. Each candidate carries the target's file path as
       its description.
       Test: none
       Since: 58f8ddd
```

### Documentation consistency

```text
CLI-20 docs/CLI.md MUST mention every command path and every long flag that
       --help prints for the built binary, recursively over all subcommands
       (--help and --version excepted). The check is one-directional: a flag
       documented but no longer present does not fail it.
       Test: crates/krab/tests/cli_docs.rs::every_command_and_flag_is_in_the_cli_reference
       Since: #102

CLI-21 Every "kapitan X.Y.Z" in a markdown file of the repository (outside
       target, vendor, node_modules and .git) MUST name the same version.
       Mentions without a patch component are not checked.
       Test: crates/krab/tests/docs_consistency.rs::every_document_names_the_same_reference_version
       Since: #110

CLI-22 The version= line of the install snippet in README.md MUST equal the
       workspace version.
       Test: crates/krab/tests/readme_version.rs::the_readme_install_snippet_names_the_current_version
       Since: #105

CLI-23 Any other release version in the documentation MUST be derived at the
       point of use (the tag command in CONTRIBUTING.md reads it with cargo
       metadata) or left out.
       Test: none
       Since: #105
```

### Continuous integration

```text
CLI-24 .github/workflows/ci.yml MUST run on every pull request and on every
       push to main. A newer push to a pull request MUST cancel its run in
       flight; runs on main MUST NOT be cancelled.
       Test: none
       Since: #72, #107 (concurrency)

CLI-25 The lint job MUST run cargo fmt --all --check and cargo clippy
       --all-targets --locked with CARGO_BUILD_WARNINGS=deny, so that any
       compiler or clippy warning fails it.
       Test: ci.yml: lint
       Since: #72, #107 (CARGO_BUILD_WARNINGS instead of RUSTFLAGS)

CLI-26 The test job MUST run cargo test --locked on ubuntu-latest and
       macos-latest without fail-fast, with Python 3.12 and pyyaml,
       omegaconf==2.4.0.dev3, kadet and jinja2 installed.
       Test: ci.yml: test
       Since: #72, #107 (macOS), #97 (omegaconf pin)

CLI-27 The parity job MUST install kapitan[omegaconf]==0.36.3 with
       omegaconf==2.4.0.dev3, regenerate tests/fixtures/expected with
       tests/fixtures/generate_expected.py, and fail when the result differs
       from the committed files, including files that were never committed.
       Test: ci.yml: parity
       Since: #97

CLI-28 The msrv job MUST read rust-version of package krab from cargo
       metadata and run cargo check --locked --all-targets on that toolchain,
       with warnings not denied.
       Test: ci.yml: msrv
       Since: #107

CLI-29 The deny job MUST run cargo deny check licenses bans sources as a
       blocking step and cargo deny check advisories as a non-blocking step.
       On pull requests both steps MUST run only when Cargo.toml, Cargo.lock,
       deny.toml or a crates/*/Cargo.toml changed; on push they always run.
       Test: ci.yml: deny
       Since: #107

CLI-30 deny.toml MUST allow only the licences it lists, deny wildcard
       dependencies, warn on duplicate versions, and deny any registry other
       than crates.io and any git source.
       Test: ci.yml: deny
       Since: #107

CLI-31 The zizmor job MUST lint every workflow file in .github/workflows.
       Test: ci.yml: zizmor
       Since: #107

CLI-32 The vscode job MUST run npm ci and npm run package in editors/vscode
       and keep the .vsix as the workflow artifact krab-vscode.
       Test: ci.yml: vscode
       Since: #72

CLI-33 The job "CI passed" MUST depend on lint, test, parity, msrv, deny,
       zizmor and vscode, run even when one of them failed, and fail unless
       every result is success or skipped.
       Test: ci.yml: required
       Since: #107

CLI-34 Every action in every workflow MUST be referenced by a full commit SHA
       with the version as a trailing comment. dtolnay/rust-toolchain MUST be
       pinned to a commit on its master branch with the toolchain named in
       the toolchain input, because its per-toolchain branches are rebuilt
       and a commit pinned from one drops out of the history (zizmor
       impostor-commit).
       Test: ci.yml: zizmor
       Since: #103, #107, #198

CLI-35 ci.yml, release.yml and links.yml MUST set permissions: {} at the
       workflow level and grant each job only what it uses, and every
       checkout MUST set persist-credentials: false. Values from the
       triggering event (ref names, pull request data) MUST reach run:
       scripts through env:, never by ${{ }} interpolation into the script.
       Test: ci.yml: zizmor
       Since: #103, #107

CLI-36 The Rust build cache MUST be saved only from runs on main.
       Test: none
       Since: #107

CLI-37 Dependabot MUST propose cargo and github-actions updates weekly, each
       with a cooldown of 7 days, commit prefixes "deps:" and "ci:", and the
       label "area: packaging"; at most 3 cargo pull requests are open at a
       time.
       Test: none
       Since: #96

CLI-38 .github/workflows/links.yml MUST run lychee with lychee.toml over the
       repository every Monday at 06:00 UTC and on manual dispatch, and fail
       on a broken link. It MUST NOT run on pull requests.
       Test: none
       Since: #109

CLI-39 Pull requests MUST be labelled by the paths they touch, per
       .github/labeler.yml (area: inventory, area: compile, area: lsp,
       area: packaging, documentation), on opened, reopened and synchronize.
       The workflow MUST NOT remove labels and MUST NOT check out pull
       request code.
       Test: none
       Since: #96
```

### Releases

```text
CLI-40 Pushing a tag matching v* MUST start .github/workflows/release.yml.
       Its version job MUST read the version of package krab with cargo
       metadata and fail before any build when the tag is not "v" followed
       by that version.
       Test: release.yml: version
       Since: #72

CLI-41 The release MUST build krab with cargo build --release --locked for
       x86_64-unknown-linux-gnu (ubuntu-22.04), aarch64-unknown-linux-gnu
       (ubuntu-22.04-arm), x86_64-apple-darwin and aarch64-apple-darwin
       (macos-latest), and package each as the archive named under
       Interfaces. The Linux runners fix the glibc floor at 2.35.
       Test: none
       Since: #72, #103 (native arm runner)

CLI-42 The release MUST package the VS Code extension without a dependency
       cache.
       Test: none
       Since: #72, #103 (no cache)

CLI-43 The release job MUST publish SHA256SUMS over every asset and a build
       provenance attestation (Sigstore, keyless) for every krab-* asset.
       Test: none
       Since: #72 (checksums), #103 (attestation)

CLI-44 Release notes MUST come from git cliff --latest with cliff.toml, which
       groups commits by their "area:" prefix, takes the subject line only
       and skips merge commits and version bumps.
       Test: none
       Since: #103

CLI-45 A tag containing "-" MUST produce a pre-release. When a release for the
       tag already exists, the assets MUST be uploaded to it, replacing
       assets of the same name, instead of creating a release.
       Test: none
       Since: #72

CLI-46 A manual run (workflow_dispatch) MUST build every artifact from the
       selected branch and keep them as workflow artifacts without creating
       or changing a release.
       Test: none
       Since: #72

CLI-47 The release version MUST have one source, version in
       [workspace.package] of Cargo.toml. The binary reports it through
       CARGO_PKG_VERSION and the release workflow through cargo metadata.
       Test: crates/krab/tests/readme_version.rs::the_readme_install_snippet_names_the_current_version
             (the one documented copy), release.yml: version
       Since: #72, #105
```

### Toolchain and lints

```text
CLI-48 rust-toolchain.toml MUST pin the toolchain (channel 1.98.1) with the
       rustfmt and clippy components. Every cargo invocation in CI and in
       the release build runs that toolchain; the stable toolchain the
       dtolnay action installs is overridden by the file.
       Test: none
       Since: #101

CLI-49 rust-version in [workspace.package] MUST name the oldest toolchain the
       workspace builds on (1.90) and MUST NOT be repeated in prose.
       Test: ci.yml: msrv
       Since: #101

CLI-50 [workspace.lints] MUST select the clippy lints dbg_macro, todo,
       unimplemented, get_unwrap, print_stdout, print_stderr and
       undocumented_unsafe_blocks at warn, and rustdoc
       broken_intra_doc_links at deny, and every crate MUST inherit them with
       [lints] workspace = true. The manifest chooses the lints; CI decides
       how hard they bite (CLI-25).
       Test: ci.yml: lint (clippy lints only; no CI job runs rustdoc)
       Since: #101

CLI-51 Only the krab binary crate MAY print to stdout or stderr. main.rs opts
       out of print_stdout and print_stderr for the whole crate; a test file
       or test function opts out of print_stderr only to report why it
       skipped.
       Test: ci.yml: lint
       Since: #101, #192

CLI-52 Every crate MUST be publish = false while [patch.crates-io] replaces
       saphyr-parser with vendor/saphyr-parser.
       Test: none
       Since: #101
```

### Not met yet

```
CLI-53 Every diagnostic code krab can emit MUST be documented in
       docs/diagnostics.md, with what triggers it.
       Test: crates/krab/tests/docs_consistency.rs::every_diagnostic_code_is_documented
       Since: #240

CLI-60 Every registered resolver SHOULD be documented under docs/, with
       its arguments and result.
       Test: none
       Since: not met yet (#140)

CLI-54 The release MUST publish a `krab` wheel to PyPI, built with maturin
       `bindings = "bin"` and uploaded by trusted publishing, from which
       `python -m krab` runs the binary.
       Test: none
       Since: not met yet (#226)

CLI-55 The Linux release binaries MUST need at most glibc 2.17, the release
       job MUST fail when a binary needs a newer symbol version, and
       README.md MUST state the floor.
       Test: none
       Since: not met yet (#225)

CLI-57 docs/ARCHITECTURE.md MUST name in its code map every file an
       extension goes through, and MUST list, for each kind of extension
       (resolver, input type, output type, fetch kind, RPC method, LSP
       capability, subcommand, `.kapitan` key, diagnostic code), the files
       to change.
       Test: none (reviewed, no automated check)
       Since: #240

CLI-58 A test that skips because Python packages or helm are missing MUST
       fail instead when the environment variable `CI` is set, and
       CONTRIBUTING.md MUST give the pip command that installs what these
       tests need, with the pins ci.yml uses.
       Test: none (the skip helpers in crates/krab/tests/helm_input.rs, kadet_isolation.rs, crates/krab-compile/tests/kadet_runner.rs and crates/krab-inventory/tests/python_resolvers.rs; checked by hand with CI=1)
       Since: #240

CLI-59 Each hand-maintained registration list MUST be checked against the
       code by a test: resolvers used in the fixture inventory, RPC methods
       in docs/CLI.md, embedded runner files, and fetch kinds in the
       inventory model.
       Test: crates/krab-inventory/tests/fixture.rs::every_registered_resolver_is_called_by_the_fixture, crates/krab/tests/docs_consistency.rs::every_rpc_method_is_documented, crates/krab-compile/src/inputs/kadet.rs::every_bundled_kapitan_module_is_listed, crates/krab-compile/src/fetch.rs::every_kind_is_accepted_by_the_inventory_model
       Since: #240
```

## Acceptance criteria

AC-1 Flag reference drift (CLI-20). Given a new long flag added to a clap
command and not to docs/CLI.md, when `cargo test --locked` runs, then
`cli_docs.rs::every_command_and_flag_is_in_the_cli_reference` fails and names
the flag. Exercised by that test.

AC-2 Version bump (CLI-22, CLI-40, CLI-47). Given the workspace version bumped
in Cargo.toml and the README install snippet not, when `cargo test --locked`
runs, then `readme_version.rs` fails naming both versions. Given a tag pushed
that differs from the workspace version, when release.yml runs, then the
version job fails and no binary is built. Exercised by
`readme_version.rs::the_readme_install_snippet_names_the_current_version` and
`release.yml: version`.

AC-3 Hand-edited expectation (CLI-27, CLI-33). Given a file under
`tests/fixtures/expected` that kapitan 0.36.3 would not print, when CI runs
on the pull request, then the parity job fails with the regeneration
instruction and "CI passed" fails. Exercised by `ci.yml: parity`.

AC-4 Scripted check (CLI-6, CLI-8, CLI-11). Given an inventory where one target
has an unresolvable `${...}`, when `krab --no-daemon --json inventory check`
runs, then stdout holds one diagnostic object per line with code
`interpolation::key_not_found`, stderr holds the render summary line and no
"<n> error" line, and the exit status is 1. Without `--json` the same
diagnostic is a miette snippet on stderr followed by "1 error". Test: none;
reproduced by hand at a4fb9c4.

AC-5 Completion under another name (CLI-17, CLI-18). Given the binary
symlinked as `krab-dev`, when `krab-dev completions <shell>` runs for each of
the five shells, then the script registers `krab-dev` with the symlink path as
completer and never registers `krab`. Exercised by
`completions.rs::registration_uses_the_invoked_name`.

AC-6 Pipe closed early (CLI-13). Given an inventory with many targets, when
`krab --no-daemon inventory | head -1` runs, then krab ends on SIGPIPE (shell
status 141) without a panic message. Test: none; reproduced by hand at
a4fb9c4.

AC-7 Release (CLI-41, CLI-43, CLI-45). Given tag `v2.0.0-alpha.5` on main
matching the workspace version, when it is pushed, then the GitHub release is
a pre-release with the four archives, the `.vsix` and `SHA256SUMS`. Test:
none; observed on the published v2.0.0-alpha.5 release.

## Edge cases

CLI-EC-1 No inventory. When `<inventory-path>/targets` is not a directory,
every command except `completions` fails with `inventory::no_targets_dir`
("no inventory at <path>") and a help text naming `--inventory-path`; exit
status 1.

CLI-EC-2 Invalid arguments. An unknown flag, a missing value or an unsupported
shell for `completions` is reported by clap on stderr with exit status 2,
before any inventory is read.

CLI-EC-3 Closed stdout. A reader that closes the pipe early ends krab by
SIGPIPE (CLI-13); nothing is printed on stderr.

CLI-EC-4 Inventory server unavailable. When the server cannot be reached or
started, krab logs a warning naming the socket and renders locally; the
output and exit status are those of `--no-daemon`.

CLI-EC-5 Completion outside an inventory. When the current directory has no
`.kapitan` and no `./inventory`, target completion offers no candidates and
reports no error.

CLI-EC-6 Explain on a missing path. See CLI-16; the help text gives
`cluster.name` and `kapitan.compile[0].name` as examples of valid paths.

CLI-EC-7 Dependency policy on an unrelated pull request. The deny job's check
steps are skipped and the job succeeds; "CI passed" counts skipped as passing.

CLI-EC-8 New advisory. A RustSec advisory published against a locked
dependency makes the advisories step fail without failing the deny job.

CLI-EC-9 Release re-run or pre-created release. Assets are uploaded with
replacement to the existing release (CLI-45); its notes are not rewritten.

CLI-EC-10 Rate-limited or bot-blocking links. The link check accepts HTTP 200,
206, 403 and 429 and retries 3 times with a 20 s timeout. It excludes vendored
and generated paths, release download URLs, crates.io and the private roadmap
board.

## Interfaces

### Diagnostic object

With `--json` each diagnostic is one line on stdout:

```json
{"severity":"error","code":"interpolation::key_not_found","message":"interpolation key 'nope' not found","target":"bad","path":"b","labels":[{"location":{"file":"/abs/inventory/targets/bad.yml","line":3,"col":6},"text":"in this value"}],"help":"check the spelling, or provide a default with ${oc.select:key,default}"}
```

`severity` is `error` or `warning`. `code` is a stable `<area>::<name>`
string. `labels` is always present and may be empty; `target`, `path`,
`help` and a label's `location` are omitted when absent. `path` is relative
to `parameters`. `line` and `col` are 1-based. The inventory server returns
the same objects in the `data` of a JSON-RPC error, and the CLI prints them
unchanged.

### Explanation object

`inventory explain --json` prints:

| field | content |
|---|---|
| `target`, `path` | as requested; `path` relative to `parameters` |
| `value`, `type` | the final value and its Python type name |
| `origin` | `{file, line, col}`, or `null` for a value kapitan synthesises |
| `resolved_from` | omitted unless interpolated: `{expr, source_path?, source_origin?}` |
| `history` | oldest first; objects tagged by `kind`: `override` (`old_location`, `old_value`?, `old_type`, `new_location`, `new_type`), `list_append` (`index`, `location`), `dereference` (`expr`, `location`) |
| `children_events` | merge events under this path that are not shown |

### Other output

The JSON forms of the remaining commands, the environment variables and the
`.kapitan` keys are in [CLI.md](../CLI.md).

### Completion protocol

`krab completions <shell>` emits a script that calls the binary back with
`COMPLETE=<shell>` set (clap_complete's dynamic protocol). The binary answers
before parsing arguments.

### Release assets

| asset | content |
|---|---|
| `krab-<version>-<target>.tar.gz` | directory `krab-<version>-<target>/` with `krab`, `LICENSE`, `README.md` |
| `krab-vscode-<extension version>.vsix` | the VS Code extension |
| `SHA256SUMS` | `sha256sum` output over every asset above |

The provenance of an archive is checked with
`gh attestation verify <archive> --repo kapicorp/krab`.

### Required check

Branch rules that gate on CI name the single check `CI passed` (CLI-33).

## Out of scope

| Item | Reason | Ref |
|---|---|---|
| LLM-oriented commands and output | Planned on the roadmap, not part of the current contract. #41 is the exception and is listed under Open deviations | #23 to #42, label `area: llm` |
| Tracking upstream kapitan releases for behaviour changes | A maintenance activity; parity is pinned to 0.36.3 (CLI-27) | #46 |
| Generating docs/CLI.md from clap | CLI.md carries explanation that generated output would replace; CLI-20 asserts coverage instead | #102 |
| cargo-dist, Conventional Commits, blanket clippy::pedantic, cargo-vet, cargo-msrv, sccache, OpenSSF Scorecard, cargo-audit, coverage as a merge gate, criterion in CI, a beta/nightly matrix | Each rejected with its reason in the survey ledger | #108 |

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| CLI-7, CLI-8, CLI-10, CLI-11 | #41 | There is no `--help-json` (clap rejects it with exit status 2). Exit status distinguishes only success (0), any failure (1) and an argument error (2). Under `--json` diagnostics go to stdout, and non-diagnostic errors and configuration warnings stay plain text on stderr. |
| CLI-60 | #140 | The registered resolvers are documented only by name in DESIGN.md and in the specs' resolver table |
| CLI-21 | #141 | The reference version 0.36.3 also appears in Rust sources (`crates/krab-inventory/tests/fixture.rs`, `crates/krab-compile/src/refs/mod.rs`, two tests in `crates/krab/tests`) and in `ci.yml`, outside the markdown-only check, and README.md lines 201 to 202 wrap between "kapitan" and "0.36.3", so the line-based scan misses it. The check compares copies with each other, not with a source. Stale `kapitan` names remain: `crates/krab-inventory/src/python.rs:142` documents `~/.cache/kapitan` where the code uses `krab`, and `crates/krab-compile/src/pyenv.rs:16` documents `$XDG_CACHE_HOME/kapitan/python/<digest>`. |
| CLI-52 | #112 | `cargo install` cannot work while `[patch.crates-io]` (Cargo.toml:75) replaces `saphyr-parser` with the vendored fork. Needs a decision: upstream the patches, publish the fork under its own name, or stay binary-only. |
| CLI-33 | #171 | The ruleset for `main` has enforcement disabled and there is no classic branch protection, so no check, `CI passed` included, is required before a merge. Needs an admin action. |
| CLI-52 | #172 | The crate name `krab` on crates.io belongs to an unrelated project. Needs a decision on the published name, if any. |
| CLI-52 | #43 | The crates are not published (`publish = false`), so there is no library release. |
| CLI-54 | #226 | No wheel is published; krab ships as release archives only |
| CLI-55 | #225 | The Linux binaries are built on ubuntu-22.04 (CLI-41) and need `GLIBC_2.34`; README.md says 2.35 |
| CLI-41 | #44 | No Homebrew formula and no `cargo binstall` metadata; release archives are the only install path. |