# Contributing

## Build

Rust 1.85 or newer. The workspace builds with no system dependencies.

```sh
cargo build --release            # target/release/krab
cargo fmt --all
cargo clippy --all-targets --release
cargo test --release
```

Put `target/release/krab` on your `PATH` (a symlink is fine). The name
does not collide with the Python `kapitan`, so both stay installed side by
side. A rebuilt binary uses a socket of its own, so there is nothing to stop
after `cargo build`; the previous daemon idles out (or `krab server stop`
stops every build's daemon for the inventory).

## Tests

* `cargo test --release` runs the unit tests and the fixture test.
  `tests/fixtures/inventory` is a small inventory exercising class
  resolution, list merging, merge-time dereferencing, every shipped
  resolver, YAML 1.1 scalars and PyYAML emitter quirks;
  `tests/fixtures/expected/*.yaml` is what kapitan 0.36.3 prints for it, and
  `crates/krab-inventory/tests/fixture.rs` compares byte for byte. Add a
  case there for every engine behaviour you change or fix, then regenerate
  the expected output with the reference implementation
  (`tests/fixtures/README.md`). CI regenerates it too, with
  `kapitan[omegaconf]==0.36.3` and `omegaconf==2.4.0.dev3`, and fails if the
  committed files differ, so a hand-written expectation cannot pass.
* `crates/krab-compile/tests/kadet_runner.rs` evaluates the component in
  `tests/fixtures/kadet` through the kadet evaluator and its bundled
  `kapitan` package (`crates/krab-compile/runner/kapitan`), checking the
  output and the recorded dependencies. It needs a `python3` with `kadet`
  and `jinja2` importable and skips otherwise. Extend the fixture when you
  add to the package's API.
* The corpus test (`crates/krab-inventory/tests/corpus.rs`) checks the
  emitters against a directory of compiled files written by the reference
  implementation. It runs only when `KRAB_CORPUS` and `KRAB_COMPILED`
  are set.

## Parity against the reference implementation

The rule for the engine is *byte-identical to kapitan 0.36.3 with the
omegaconf inventory backend*. After any change to loading, merging,
interpolation, resolvers, the emitters or compile, check a real inventory:

```sh
cd path/to/an/inventory/repo
krab inventory -t some.target > /tmp/new.yml
kapitan  inventory -t some.target > /tmp/ref.yml     # the Python kapitan
diff /tmp/ref.yml /tmp/new.yml                       # must be empty

krab compile --force && git status --short compiled   # must print nothing
```

`krab inventory check` renders every target and is the quickest way to
find a regression that only one target triggers. Where behaviour differs on
purpose, say so in `docs/DESIGN.md`.

## Language server

`scripts/lsp-smoke.py` drives `krab lsp` over stdio without an editor and
prints hover and definition results for the positions you give it;
`scripts/lsp-smoke-live.py` additionally exercises completion and live
diagnostics by editing a class on disk and restoring it. Both expect a
`krab` binary on `PATH`:

```sh
python3 scripts/lsp-smoke.py path/to/inventory/repo inventory/targets/some/target.yml 1:10 20:24
python3 scripts/lsp-smoke-live.py path/to/inventory/repo
```

The VS Code extension lives in `editors/vscode`; its README explains how to
package and install it.

## CI and releases

`.github/workflows/ci.yml` runs on every pull request and push to `main`:
`cargo fmt --check`, `cargo clippy --all-targets` and `cargo test` with
warnings denied, the reference-parity job described under Tests, and
`npm run package` in `editors/vscode` (the `.vsix` is kept as a workflow
artifact). The corpus test does not run there; it needs
a real inventory and the reference implementation.

To release, bump `version` in the workspace `Cargo.toml` (and the extension's
`package.json` when it changed), merge, then tag `main`:

```sh
git tag v2.0.0-alpha.4
git push origin v2.0.0-alpha.4
```

`.github/workflows/release.yml` refuses a tag that does not match the
workspace version, builds `krab` for Linux x86_64 and aarch64 (on
Ubuntu 22.04, so glibc 2.35 or newer) and for macOS Intel and Apple silicon,
packages the extension, and creates the GitHub release with generated notes,
the four `krab-<version>-<target>.tar.gz` archives, the `.vsix` and a
`SHA256SUMS` file. A tag with a pre-release suffix (`-alpha.1`) becomes a
pre-release. If the release already exists (created from the GitHub UI, for
example) the assets are uploaded to it instead. Running the workflow by hand
from the Actions tab builds the same artifacts from any branch without
publishing anything.

## Layout and conventions

* `krab-inventory` is the library entry point and must stay free of
  daemon, compile and CLI concerns. Everything the CLI prints is computed
  there or in `krab-compile`; the CLI only formats.
* The daemon and the local path run the same library code. A feature that
  works only with (or only without) the daemon is a bug.
* Every error is a `Diagnostic` with a code, a message, origins and a `help`
  text, so it renders the same with miette, as JSON lines and in the editor.
* `vendor/saphyr-parser` is a copy of the crate with two small patches
  (`vendor/README.md`). Keep the diff against upstream minimal.
* Commit messages: `area: what changed` (`lsp: accept the --stdio flag`).

## Docs

`README.md` is the overview, `docs/GETTING-STARTED.md` the walkthrough,
`docs/CLI.md` the flag reference (keep it in sync with `--help`),
`docs/DESIGN.md` the semantics. Open work is tracked as issues on the
krab roadmap project board (https://github.com/orgs/kapicorp/projects/5);
`docs/ROADMAP.md` points there.
