# Krab: architecture assessment and implementation plan

Status: **ready for review**.

Since this assessment was written, its findings have been filed:
[#133](https://github.com/kapicorp/krab/issues/133)-[#143](https://github.com/kapicorp/krab/issues/143)
(correctness, testing and documentation) and
[#144](https://github.com/kapicorp/krab/issues/144)-[#150](https://github.com/kapicorp/krab/issues/150)
(side effects and secrets, reported publicly by maintainer decision). Execution
plans live in `docs/exec-plans/`. Decision D1 is answered; D2 to D5 are open.

Inspected revision: `6e9e55ba18597b34ab056cfe29cb28698fa97993` (`main`, working tree clean).
This is the same revision the prior review inspected, so there are no intervening
commits to reconcile. There are, however, **16 open pull requests and a 15-issue
audit filed against this revision**, and they change the answer to most of the
questions in the assignment. See [Section 2](#2-what-is-already-in-flight).

Assessment date: 2026-09-19. Analysis and planning only. No production code was
changed, no issue or pull request was created, the project board was not touched,
and nothing was staged or committed. This document is an untracked file in the
working tree.

---

## 1. Recommendation and decisions needed

### 1.1 Recommendation

**Keep the current architecture. Do not refactor it.**

Section 1.2 states what that architecture is, because no document in the
repository does and the recommendation is meaningless without it.

The architectural hypothesis in the assignment, a compiler-oriented modular
monolith with hexagonal boundaries, a functional core and selective executable
integrations, is not a target for Krab. It is a reasonable description of what
Krab already is. Concretely:

* `krab-inventory` depends on no workspace crate. `krab-compile` depends only on
  `krab-inventory`. `krab-compile` contains no socket or JSON-RPC code, and the
  daemon dependency reaches it as an opaque `Option<PathBuf>`. The dependency
  graph already points one way.
* The functional core exists and is large: the value model, merge, the
  interpolation parser and evaluator, the emitters, the kapitan model and
  `explain` are pure. I/O is concentrated in `inventory.rs`, `yaml::load_file`,
  `dotkapitan.rs` and the Python worker layer.
* The ports that matter are already traits: `DocSource` and `DocProvider` for
  document sourcing, `PythonProbe` for interpreter selection. Rendering is
  invoked identically by the CLI, the daemon and the compile engine, and I
  verified that the daemon and local paths agree byte for byte.
* The "selective executable integration" is the Python worker protocol, which is
  already a process boundary with a defined line-delimited JSON contract, shared
  between the resolver runner and the kadet evaluator.

This applies to the crate graph and the render pipeline. It is not a blanket
answer about every variation point. Section 6 assesses four candidate layers,
secrets, inputs, inventory backend and schema, on their own evidence and reaches
four different verdicts: one new internal boundary is justified (secrets, on
testability grounds, milestone M6), one is decision-gated (inventory backend),
one is a small local validation rather than a layer (`.kapitan` schema), and one
should stay a `match` with a stricter contract rather than becoming a trait
(inputs).

Adding traits, crates or a plugin protocol on top of this would be motion, not
progress. I found no substitution requirement that the current seams cannot
serve. The one genuinely closed extension point, the `match` on `input_type` in
`native.rs`, is closed on purpose and should stay closed: every input type needs
bespoke dependency recording to participate in incremental compile, a trait
would not supply that, and `--backend python` is the existing escape hatch for
unsupported types.

The real risk in this codebase is not its shape. It is that its two least tested
subsystems, the compile engine and the daemon, are also the two that can produce
silently wrong output or expose data. That is where the proposed work goes.

### 1.2 The architecture being kept

The recommendation above is worth little without naming what it refers to, and
neither `docs/DESIGN.md` nor `CONTRIBUTING.md` states it in one place. DESIGN.md
describes the semantics module by module; there is no document that says what the
shape is. PR [#96](https://github.com/kapicorp/krab/pull/96) adds a
`docs/ARCHITECTURE.md`, which is the right home for a corrected version of this
section.

Synthesised from the code at the inspected revision:

**One data structure, three stages, two consumers.**

Everything in Krab is a transformation of `Node { value: Value, origin: Origin }`
(`crates/krab-inventory/src/value.rs`): a JSON-like tree in which every node
carries the file, line and column it came from, with files interned in `Sources`.
Provenance is not a feature bolted on beside the data; it is in the node type, so
every stage preserves it for free and `explain`, LSP hover and diagnostics all
read the same record.

The stages:

```
  YAML files                  yaml.rs            saphyr-parser (vendored, patched)
      |                                          + PyYAML 1.1 scalar rules
      v
  [1] LOAD        ClassDoc { classes, parameters, applications, exports }
      |
      v                       inventory.rs       class resolution, memoised
  [2] RESOLVE     ClassClosure per class file    per class file, shared across
      |           merge.rs                       targets
      |           interp/ (parse + 3-pass eval)
      |           resolvers/ (oc | builtin | contrib | python)
      v
             RenderedTarget { parameters, classes, applications, exports,
                              files, probes, digest, doc_digest,
                              provenance, warnings }
      |
      +----------------------------+
      |                            |
      v                            v
  [3a] EMIT                   [3b] COMPILE
  emit/yaml.rs (PyYAML port)  krab-compile: input types over the document
  emit/ryml.rs (rapidyaml)    -> a compiled file tree
  emit/pyjson.rs
```

Stages 1 and 2 are the front end and live entirely in `krab-inventory`, which
depends on no other workspace crate. Stage 3a is also in `krab-inventory`. Stage
3b is `krab-compile`, which **never renders anything**: it receives finished
documents through a trait it defines itself. That is the compiler-oriented split,
and it is real rather than nominal.

**Four adapters over one engine.** The CLI, the daemon, the LSP and the compile
engine are consumers of the same `Inventory::render*` calls, not four
implementations:

* `krab` (CLI) wires configuration and formats output.
* `krab-server` keeps an `Inventory` in memory, watches files, and serves the
  rendered result over line-delimited JSON-RPC on a Unix socket.
* `krab-lsp` renders nothing at all. It translates LSP requests into daemon RPCs
  and formats the answers. Its only local computation is class-path probing, a
  YAML position index, and formatting a value the daemon already produced.
* `krab-compile` obtains documents through `DocSource`/`DocProvider`, which the
  CLI implements twice: once over the daemon socket, once by rendering locally.

This is why daemon and local agree: not by contract but because there is one
implementation. I verified the agreement rather than assuming it.

**The ports that exist, and they are few.** `DocSource`/`DocProvider` for
document sourcing, `PythonProbe` for interpreter selection, `Registry` with
boxed `Fn` resolvers for interpolation, and the line-delimited JSON process
protocol for Python. That is the whole set. There is no input-type trait, no
plugin loading and no public extension protocol.

Section 6 argues that this is correct for inputs and for public extensibility in
general, and that it is **not** correct for secrets backends, where the absence
of a seam is why three of eight have no tests at all. Adding one internal
boundary there is the single architectural change this document recommends.

**The pattern that actually unifies the system: every stage reports its own
dependency footprint, and the stage above indexes it.** This is the part that is
easy to miss and is the thing most worth not breaking.

| Stage | Reports | Consumed by |
|---|---|---|
| A render | `files` (what it read) and `probes` (paths checked while resolving class names, hits and misses) | The daemon's path-to-targets index, so creating a file at a probed path invalidates exactly the right targets |
| A render | `digest` over its inputs and `doc_digest` over the rendered document | `krab-compile`'s staleness decision. The front end computes the back end's cache key |
| A kadet or jinja2 item | files read, directories listed, modules imported, other targets read, document keys read | `compiled/.krab-manifest.json`, for per-item reuse inside a stale target |

Three levels, one shape. It is also where the defects are: F1 is an input type
that reports nothing, and F6 is the daemon failing to notice that its index has
gone stale.

**State ownership is explicit and there are almost no globals.** `Inventory`
owns two caches, parsed files and class closures, plus the path interner, behind
interior mutability, and is invalidated by explicit calls rather than by time or
mtime. The daemon owns everything else behind one `RwLock` plus a generation
counter. The compile engine owns only per-run state. The sole process-wide
mutable state is a helm version cache and a thread-local re-entrancy counter for
nested Python calls.

**What the architecture deliberately is not:** there is no task graph
(`TargetPlan` is per-target metadata and targets are independent), no
domain/application/infrastructure layering by name, no dependency injection
container, and no stable published library surface.

**The two constraints that explain every choice**, and the reason most
"improvements" would be regressions:

1. *Byte-identical to kapitan 0.36.3 with the OmegaConf backend.* This is why the
   value model reproduces Python equality and `repr`, why the emitter is a port
   of PyYAML's rather than a use of a YAML library, why the parser is vendored
   and patched, and why a resolver that returns a string containing `${` is
   re-evaluated on the next pass.
2. *Exact invalidation or nothing.* This is why dependency recording is woven
   into each input type instead of sitting behind a generic hook, and why a
   generic input-type trait would be an active loss.

### 1.3 What the assessment actually found

Three things, in order of how much they should change your plans:

1. **A thorough audit already exists and is more authoritative than this one.**
   Issue [#128](https://github.com/kapicorp/krab/issues/128) and its children
   [#113](https://github.com/kapicorp/krab/issues/113)-[#127](https://github.com/kapicorp/krab/issues/127),
   filed against this same revision, tested Krab against five real inventories
   and a 111-target generator corpus. It found blockers that no synthetic fixture
   reaches, including four that stop whole repositories rendering. Nothing in
   this document supersedes it, and the milestones below are ordered to sit
   beside it rather than compete with it.
2. **Most of the small findings a fresh review produces are already tracked or
   already fixed in an open pull request.** I reproduced the failing `fetch`
   tests, the `compose-target-name` default divergence and the unpinned
   `omegaconf` resolution independently, and all three already have a fix in
   flight. That is a good signal about the project, and a reason not to add a
   parallel plan.
3. **One cluster of findings is entirely untracked: side effects and secrets.**
   Nine defects in `refs/`, `fetch.rs` and `oci.rs`, plus a daemon socket that is
   world-connectable in one common environment, have no issue and no pull
   request. This is where a fresh pair of eyes was worth something.

### 1.4 Decisions needed from you

| # | Decision | Why it blocks | Affects |
|---|---|---|---|
| ~~D1~~ | *Answered: filed publicly as #144-#150.* How should the untracked security findings be reported? They are local-attacker and untrusted-input issues, not remote code execution, but several are unambiguous defects. `SECURITY.md` arrives with PR [#96](https://github.com/kapicorp/krab/pull/96) and is not on `main` yet. | I did not file them, per the assignment. Until you choose a channel they exist only in this document. | M1, M6, M7 |
| D2 | Is a Unix socket shared across uids a supported configuration? If not, the fix is to create the runtime directory `0700` and the socket `0600`. If it is, the fix is a `SO_PEERCRED` allowlist instead. | Determines the shape of M1. | M1 |
| D3 | For the `external` input type, is the intended behaviour "always stale, like the reference" or "declare your inputs"? Issue [#16](https://github.com/kapicorp/krab/issues/16) names the problem but not the resolution. | Determines whether M3 is a five-line change or a schema addition. | M3 |
| D4 | Is byte-identical to kapitan 0.36.3 with the OmegaConf backend still the sole compatibility target? [#118](https://github.com/kapicorp/krab/issues/118) and [#119](https://github.com/kapicorp/krab/issues/119) report inventories that the reference runs on reclass and a `.kapitan` `version:` key that Krab ignores. | I assumed yes throughout, per `CONTRIBUTING.md`. If the answer is no, the compatibility matrix in Section 5 needs a second column. It also decides whether an inventory-backend boundary is justified at all, or whether the correct fix for [#118](https://github.com/kapicorp/krab/issues/118) is a ten-line guard. | all, and the inventory-backend row in Section 6 |
| D5 | Do you want a compile-level golden fixture at all, or is parity against real inventories (the #128 method) the intended oracle? PR [#97](https://github.com/kapicorp/krab/pull/97) explicitly defers this. | Determines whether M2 is worth doing. | M2 |

### 1.5 Decisions taken since

* **D1: answered.** Reported publicly, as
  [#144](https://github.com/kapicorp/krab/issues/144)-[#150](https://github.com/kapicorp/krab/issues/150).
* **D5: both.** A committed compile fixture running in CI on every change, and
  the real-inventory comparison retained as the periodic deeper check. They
  catch different things.
* **D4: reclass eventually.** Supporting reclass is accepted scope, so a backend
  boundary is justified rather than only a guard. Every compatibility claim in
  Section 5 gains a second column when that work starts. The ten-line refusal
  for [#118](https://github.com/kapicorp/krab/issues/118) remains correct in the
  meantime and should not wait.
* **D2: under investigation.** Whether any deployment shares a daemon across
  accounts decides mode bits versus a `SO_PEERCRED` allowlist. The exposure in
  [#150](https://github.com/kapicorp/krab/issues/150) stays open until then, and
  it matters most on CI and container hosts.
* **D3: evidence gathered, not yet decided.** Asked whether kapitan's other
  opaque input types settle it. They do. `kapitan/inputs/base.py` defines
  `cacheable() -> False` and an abstract `inputs_hash`, and **only `kadet`
  overrides them**. `cuelang`, `kustomize`, `jsonnet`, `helm`, `external`,
  `jinja2` and `copy` are all non-cacheable in the reference. So caching there
  is opt-in, and only a type that can report what it read may opt in. That is
  Option A as a principle, and it extends to cue and kustomize whenever krab
  implements them natively. Worth noting that krab is currently more aggressive
  than the reference: jinja2, copy and external all take part in target-level
  staleness while only kadet produces item records. The `Freshness` field
  proposed in `docs/exec-plans/16-external-freshness.md` is `cacheable()` under
  another name, made mandatory rather than defaulted. A second check points the
  same way: of the six corpora in [#128](https://github.com/kapicorp/krab/issues/128),
  only corpus D uses `external`, and D's entire compile is 0.6 s of real work,
  so always rebuilding those targets costs a fraction of that. **Left open
  deliberately**: the evidence favours Option A, and no decision has been taken.
  F2 and F3 are filed as [#152](https://github.com/kapicorp/krab/issues/152) and
  fold into this work if D3 produces a general definition of an input.

I did not resolve D1-D5 myself because each changes what gets built, not merely
how.

---

## 2. What is already in flight

Read this before acting on anything below. Open pull requests at the inspected
revision:

| PR | What it does | Overlaps something I reproduced independently |
|---|---|---|
| [#97](https://github.com/kapicorp/krab/pull/97) | CI job regenerating `tests/fixtures/expected` with the reference and failing on a diff; pins `omegaconf==2.4.0.dev3` | Yes: the unpinned `omegaconf` resolution breaks the reference environment entirely (Section 11) |
| [#99](https://github.com/kapicorp/krab/pull/99) | Runs the `fetch` test fixtures with `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` neutralised | Yes: `cargo test` fails on a clean checkout with commit signing enabled (3.2) |
| [#132](https://github.com/kapicorp/krab/pull/132) | Flips the `compose-target-name` default to `false`, matching the reference | Yes: reproduced the naming divergence against the reference (Section 5) |
| [#107](https://github.com/kapicorp/krab/pull/107) | Six-job CI: lint, test on ubuntu and macOS, reference parity, MSRV, `cargo-deny`, `zizmor`; actions pinned, `permissions: {}` at root | Partly: it annotates the silently skipping tests but does not make a green run mean they ran (3.1) |
| [#101](https://github.com/kapicorp/krab/pull/101) | `rust-toolchain.toml`, `[workspace.lints]`, `publish = false` | Related to the crates.io hazard in 4.6 |
| [#96](https://github.com/kapicorp/krab/pull/96) | Issue and PR templates, `SECURITY.md`, `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, and **`AGENTS.md` plus a one-line `CLAUDE.md` importing it** | Yes, answers Section 8 |
| [#102](https://github.com/kapicorp/krab/pull/102) | Test that `docs/CLI.md` mentions every command and flag | Partly: fixes the `server logs` row, two other drifts remain (4.6) |
| [#103](https://github.com/kapicorp/krab/pull/103), [#105](https://github.com/kapicorp/krab/pull/105), [#109](https://github.com/kapicorp/krab/pull/109), [#110](https://github.com/kapicorp/krab/pull/110), [#111](https://github.com/kapicorp/krab/pull/111), [#106](https://github.com/kapicorp/krab/pull/106) | Release attestation and notes, derived version, weekly link check, docs consistency | No |
| [#129](https://github.com/kapicorp/krab/pull/129), [#131](https://github.com/kapicorp/krab/pull/131) | `key`/`parentkey`/`fullkey` parity, falsy `classes` parity | No |
| [#75](https://github.com/kapicorp/krab/pull/75) (draft) | Stage compile output inside `compiled/` | Adjacent to F7 |

Issue [#108](https://github.com/kapicorp/krab/issues/108) is the ledger for a
Rust-practice survey and records what was **rejected on purpose**. I checked my
proposals against it. Nothing below re-proposes a rejected item.

**Consequence for this assignment.** The obvious first milestone, "make
compatibility verification reproducible", is PR #97. It exists. Proposing it
again would be redundant work. The milestones in Section 7 start where #97 stops.

---

## 3. Verification actually executed

Everything in this section was run by me on this machine against the inspected
revision. Anything not listed here was not run.

**Environment.** Linux 7.1.9 x86_64, Fedora. `rustc` and `cargo` 1.94.0 stable
(rustup), `rustfmt` 1.9.0. `python3` 3.14.7 for the workspace tests. Reference
implementation in a disposable virtualenv: `kapitan[omegaconf]==0.36.3` on
CPython 3.11.15 with `omegaconf==2.4.0.dev4`. `git` 2.x, `gpg` present, `helm`
present, `kapitan` 0.35.2.dev23 also on `PATH` and **not** used as the reference.

| Check | Command | Result |
|---|---|---|
| Formatting | `cargo fmt --all --check` | Clean |
| Lints | `cargo clippy --all-targets --locked` with `RUSTFLAGS=-D warnings` | Clean, no warnings |
| Tests, default environment | `cargo test --locked` | **Failed.** 2 of 37 `krab-compile` lib tests fail; the run aborts and the remaining seven test binaries never execute |
| Tests, isolated git config | `cargo test --locked --no-fail-fast` with `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null` | Pass: 79 tests across 9 binaries |
| Which tests actually exercised their case | `cargo test -- --nocapture`, grepping for the skip messages | **3 of 79 silently skipped**: `corpus_parity`, and both `kadet_runner` tests |
| Fixture oracle is genuine | Regenerated `tests/fixtures/expected/*.yaml` in a scratch copy with real kapitan 0.36.3 and diffed against the committed files | **Byte-identical**, 3 of 3 targets |
| End-to-end CLI parity, fixture inventory | `kapitan inventory -t X` against `krab inventory -t X`, 3 targets | **Byte-identical**, 3 of 3 |
| End-to-end CLI parity, synthetic 100-target inventory | Same, 100 targets exercising classes, relative interpolation, `EXTEND_UNIQUE` lists, nested resolution | **Byte-identical**, 100 of 100 |
| Compile parity, synthetic | `kapitan compile` against `krab compile` on a `jinja2` plus `copy` inventory | Identical, except `compiled/.krab-manifest.json` which only Krab writes |
| Daemon versus local | `krab inventory -t X` with and without `--no-daemon`, 3 targets | Identical, 3 of 3 |
| Incremental invalidation | Six scenarios, see 4.4 | 5 correct, 1 defect |
| LSP | `scripts/lsp-smoke.py` | **Fails to start** on an unmodified checkout; after a workaround it answers, and leaves behind a `krab lsp` in a daemon-respawn loop (F18) |
| Daemon socket exposure | Started a daemon with `XDG_RUNTIME_DIR` unset, inspected modes, connected with a raw socket and called `inventory.all` | **Full inventory returned with no authentication**; directory `0755`, socket `0755` |
| Speed, synthetic | 100-target full render, single run, Krab in a **debug** build | Krab 0.05 s / 19 MB RSS, reference 3.01 s / 107 MB RSS |

### 3.1 Coverage limits, stated explicitly

* **The corpus test did not run.** `KRAB_CORPUS` and `KRAB_COMPILED` were not
  set, because I have no directory of files written by the reference at the
  required scale. The rapidyaml emitter `emit/ryml.rs` is exercised by nothing
  else, so it was not tested in any run I performed, nor is it in CI.
* **The kadet evaluator was not tested.** `python3` on this machine lacks `kadet`
  and `jinja2`, so both `kadet_runner` tests returned early. CI installs them, so
  they do run there; I did not verify that.
* `python_resolvers::small_fixture_with_the_installed_omegaconf` ran, but its
  guard is only `import sys`. With `omegaconf` absent it silently exercised the
  stand-in module path instead of the installed one. The test name asserts
  something the test does not check.
* **I have no access to a production inventory.** The README and `docs/ROADMAP.md`
  claim byte-identical rendering and compilation of a 160-target inventory. That
  remains an attributed claim. My 100-target synthetic result is consistent with
  it but is a much weaker instrument: no kadet, no helm, no refs, no dependencies.
* **The speed numbers are synthetic, single-run, and unfavourable to Krab**
  (debug build). They establish a direction, not a factor. Do not quote them.
* macOS, Windows and aarch64 were not exercised. CI does not test macOS either at
  this revision; PR #107 adds that leg.
* I did not run `refs` against any live backend. Vault, GCP KMS, AWS KMS and
  Azure Key Vault findings below are from reading the code, not from execution.
  The GPG round-trip test did run and passed.

### 3.2 The failing default test run

On a checkout with no local modifications, `cargo test --locked` fails:

```
thread 'fetch::tests::item_force_fetch_applies_without_fetch_flag' panicked at
crates/krab-compile/src/fetch.rs:1249:49:
git ["tag", "v1"]: fatal: no tag message?
```

Cause: `make_repo` in `fetch.rs` sets `user.email` and `user.name` on the fixture
repository but inherits everything else from the developer's global git config.
With `tag.gpgsign = true` set globally, `git tag v1` becomes an annotated signed
tag and demands a message. This is issue
[#98](https://github.com/kapicorp/krab/issues/98) and PR
[#99](https://github.com/kapicorp/krab/pull/99) fixes it exactly this way.

The part worth adding to that issue: because `cargo test` stops at the first
failing binary, this failure also prevents `krab-inventory`, `krab-lsp`, the
fixture test and the Python resolver tests from running at all. A developer with
commit signing enabled, which is not unusual, sees a red suite and no coverage.

---

## 4. Current-state findings

### 4.1 Crate and module boundaries

```
                    +-------------------+
                    |       krab        |  CLI binary
                    |  (bin, 2.8k LOC)  |
                    +---+-----+-----+---+
       source dep       |     |     |
     +------------------+     |     +-----------------+
     v                        v                       v
+----------+          +---------------+        +-------------+
| krab-lsp |--------->|  krab-server  |        | krab-compile|
|  (1.1k)  |          |    (1.3k)     |        |   (7.3k)    |
+----+-----+          +-------+-------+        +------+------+
     |                        |                       |
     +------------+-----------+-----------------------+
                  v
         +-------------------+
         |  krab-inventory   |  no workspace dependencies
         |      (6.5k)       |
         +-------------------+

Runtime calls that are not source dependencies (dashed in intent):
  krab-compile  --(unix socket, via the Python kadet runner)-->  krab-server
  krab-inventory --(spawns python3 workers)-->  resolver_runner.py
  krab-compile   --(spawns python3 workers)-->  kadet_runner.py, kapitan_runner.py
  krab-compile   --(subprocess)--> git, helm, gpg, aws, az, gcloud
```

Source dependency direction is correct and acyclic. Two observations:

**`krab-inventory` is not a library, it is a namespace.** Every module is
`pub mod`; there is no private core. `lib.rs` declares 14 public modules and
flat-re-exports 15 types. `krab-compile` reaches into
`Node::synthetic`, `SourceId::SYNTHETIC`, `DumpOptions::pyyaml_default` and
`needs_pyyaml_fallback`. That coupling is justified by the goal, byte-exact
PyYAML and rapidyaml reproduction is the product, but it means there is no
supported surface distinct from the implementation, and nothing would flag a
breaking change to a caller. Relevant only if the crates are ever published, and
`[patch.crates-io]` blocks that today anyway. PR #101 sets `publish = false`.

**`CONTRIBUTING.md` overstates how thin the CLI is.** It says "Everything the
CLI prints is computed there or in `krab-compile`; the CLI only formats". Of the
2825 lines in `crates/krab/src`, roughly 234 (`report.rs` and `explain.rs`) are
purely formatting. `cmd_refs.rs` is about 550 lines of domain logic, including
two functions that are explicit reimplementations of kapitan behaviour
(`reveal_path`, and a 130-line `update_validate` mirroring
`secret_update_validate`). Several server RPCs have a second implementation in
the CLI for the `--no-daemon` path: `inventory.targets`, `inventory.deps`,
`inventory.class_usage`, `inventory.explain`, pattern selection. Those pairs are
where daemon and local silently diverge (4.5).

This is a documentation defect, not an architecture defect. The duplication is
real but small, and the alternative, moving `cmd_refs.rs` into `krab-compile`,
buys nothing today because nothing else calls it.

### 4.2 Representative execution trace

`krab compile` on a target with a `jinja2` input and a `kadet` input, daemon
available:

```
 1. krab/main.rs          parse argv, install SIGPIPE default, set up tracing
 2. krab/app.rs           load .kapitan from cwd (not searched upward)
                          InventoryConfig { compose_target_name, normalize }
                          Registry::with_builtins()  -> oc, builtin, contrib
                          PythonResolvers::install() -> python resolvers win
                          connector = (!no_daemon && !raw)
 3. krab/cmd_compile.rs   assemble Settings and NativeOptions from flags + .kapitan
                          build AppSource, an impl of krab_compile::DocSource
 4. krab-compile/engine   load compiled/.krab-manifest.json
                          probe python versions, compute engine identity
 5.   -> DocSource        RPC inventory.targets -> (name, doc_digest) for all
 6.   fetch               parameters.kapitan.dependencies, before staleness,
                          so fetched files are ordinary inputs
 7.   why_stale/target    engine id, doc digest, config digest, every recorded
                          dep path, per-target globals, output tree digest
 8.   -> DocSource        RPC inventory.target for the stale ones only
 9.   TargetPlan::new     extract parameters.kapitan.compile, labels, probes;
                          hash the document
10.   thread::scope       N raw OS threads over a Mutex<VecDeque<TargetPlan>>
11.     native.rs         match item.input_type { "kadet" | "jinja2" | ... }
12.       jinja           minijinja, inventory_global records target reads
13.       kadet           line-delimited JSON to a pooled python3 worker;
                          the worker connects to the daemon socket itself and
                          calls inventory.target on demand;
                          mid-eval "helm" host requests answered by this thread
14.     output.rs         prune_empty, output type, ref embed or reveal,
                          rapidyaml or PyYAML emit
15.     install           delete final_dir contents except nested-target dirs,
                          move the temp tree in, merge nested dirs
16.     manifest.save     tmp write + rename, once per successful target
17.   cleanup             remove output directories no target claims (full runs)
```

What this trace shows that the crate diagram does not:

* The daemon is consulted **three times by two different processes**: twice by
  the Rust engine for digests and documents, and then independently by each
  Python kadet worker over the same socket. The Python side performs no version
  handshake.
* Step 7 depends on step 15 of the previous run (it re-digests the compiled
  tree), and step 6 deliberately precedes step 7 so fetched files become inputs.
  Ordering is load-bearing and is expressed only as statement order in
  `engine.rs`.
* There is no build graph. `TargetPlan` is per-target metadata, not a plan.

### 4.3 `TargetPlan`, assessed before proposing anything else

`crates/krab-compile/src/plan.rs`, 94 lines:

```rust
pub struct TargetPlan {
    pub name: String,
    pub target_path: String,
    pub doc: Value,
    pub doc_digest: String,
    pub compile: Vec<Value>,   // still raw JSON, parsed per item later
    pub labels: Vec<(String, String)>,
    pub probes: Vec<PathBuf>,
}
```

It carries almost no behaviour: a pure constructor and `matches_labels`. It is
**not** a build plan. There are no steps, no edges, no ordering beyond the
declaration order of `compile`, and no relation between targets.

**Do not introduce another build-plan abstraction.** Targets are genuinely
independent units of work with no inter-target ordering. A DAG would model an
empty edge set. What is missing is not a graph, it is that seven distinct phases
(fetch, staleness, plan, execute, install, record, cleanup) exist only as
sequential statements in a 1007-line `engine.rs` with no test calling
`compile()`. The fix for that is a test, not a type. That is milestone M2.

The one thing `TargetPlan` should arguably gain is a *validated* `Vec<Item>`
rather than `Vec<Value>`, so `output_path` is checked once instead of at each
dispatch site. That is a prerequisite of M4, not a change worth making alone.

### 4.4 Incremental correctness

The staleness decision (`engine.rs::why_stale`) checks, in order: no record,
engine identity, document digest, config digest, every recorded dependency path,
per-target `inventory_global` digests, output directory existence, output tree
digest. `--force` bypasses all of it.

I tested six scenarios. Five behave exactly as `docs/DESIGN.md` claims:

| Scenario | Expected | Observed |
|---|---|---|
| Nothing changed | up to date | up to date |
| Template file edited | recompile | `[templates/deploy.j2 changed]` |
| Compiled output edited by hand | recompile | `[compiled output was modified]` |
| Inventory value changed via `${oc.env:...}` and the env var changed | recompile | `[inventory changed]` |
| `.kapitan` key that Krab does not read (`prune`) changed | up to date | up to date |
| `.kapitan` `compile.indent` and `yaml-dump-null-as-empty` changed | recompile | `[compile settings changed]` |

The sixth is a defect, reproduced end to end against the reference:

**F1. An `external` input's data dependencies are not tracked, so compiled output
goes stale silently.** `external` runs a command through `sh -c` and records zero
reads. Reproduction, with a script that copies `source-of-truth.txt` into the
output:

```
krab compile                       -> compiled output contains VERSION-ONE
edit source-of-truth.txt           -> VERSION-TWO
krab compile --explain             -> "up to date ext"
                                      compiled output still contains VERSION-ONE
```

The reference recompiles every run and its output always tracks the file. This
contradicts the stated principle *exact invalidation or nothing*: for `external`
it is neither. `krab compile --force && git status --short compiled`, the parity
check in `CONTRIBUTING.md`, cannot catch it, because `--force` recompiles.

Tracked as [#16](https://github.com/kapicorp/krab/issues/16), which names the
problem but not the resolution. See decision D3.

Two further gaps I did not reproduce but which are visible in the code:

* **F2.** File fingerprints hash content and the executable bit only
  (`digest.rs`). A mode change from `0644` to `0640` on a source file does not
  invalidate, although `copy` preserves the full mode and `jinja2` applies
  `mode & 0o7777`. Untracked.
* **F3.** Symlinks are fingerprinted by the content they resolve to, never by the
  link target path. Repointing a symlink at a byte-identical file is invisible; a
  dangling symlink is indistinguishable from a missing file. Untracked.

Neither is urgent. Both belong in the same issue as F1 if D3 produces a general
answer about what "input" means.

### 4.5 State, lifecycle and daemon/local parity

Ownership is clear and there is no lock-order inversion. `Inventory` owns two
mutex-guarded caches (parsed files, class closures) and an append-only path
interner. The daemon wraps everything else in a single `RwLock<Inner>`. The
compile engine owns a per-run `Digests` memo, a `Mutex<Manifest>`, a work queue
and a kadet worker pool. The only process-wide statics are a helm version cache
and a thread-local re-entrancy counter for nested Python calls.

Three defects, all in `krab-server`, all untracked:

**F4. The daemon socket is world-connectable when `XDG_RUNTIME_DIR` is not set,
and the protocol has no authentication.** `paths.rs::runtime_dir` falls back to
`/tmp/krab-<uid>`, created by `create_dir_all` with default permissions.
`UnixListener::bind` is never followed by a `set_permissions`, and nothing in the
crate reads `SO_PEERCRED`. Observed on this machine with `XDG_RUNTIME_DIR` unset:

```
drwxr-xr-x.  /tmp/krab-<uid>
srwxr-xr-x.  /tmp/krab-<uid>/<inventory>-<build>.sock
```

A raw `connect()` plus one `inventory.all` request returned every rendered target
document. Any local uid can also call `server.shutdown`. With `XDG_RUNTIME_DIR`
set the parent directory is `0700` and contains the exposure, so the severity is
environment-dependent: it bites in containers, cron jobs and ssh sessions with a
minimal PAM stack, which is where CI and automation live.

What is reachable over that socket: every literal inventory value, any file
content pulled in by `${from_file:...}` (which has no path restriction), the
daemon's environment variables via `${oc.env:...}`, and absolute filesystem
paths. Revealed secrets are **not** reachable; reveal lives client-side in
`cmd_refs.rs`, and the inventory holds reference tags only. That limits the
impact but does not remove it.

**F5. Lost wakeup in `State::wait_for`.** The predicate `inner.generation` is read
under the `inner` read lock, which is then dropped, and the wait uses a *different*
mutex, `changed_lock`. The notifier increments the generation under the `inner`
write lock, drops it, and only then takes `changed_lock` to `notify_all`. A
waiter descheduled between dropping `inner` and acquiring `changed_lock` misses
the notification and sleeps to its deadline: up to 30 s for `krab inventory
watch`, up to 60 s for LSP diagnostics. The adjacent `wait_ready` is written
correctly, checking and signalling under the same mutex, which makes this look
like an oversight rather than a design choice. Self-correcting and bounded, but
real.

**F6. A dropped filesystem event is permanent.** `watch.rs` handles neither
notify's rescan/overflow signal (no `Rescan`, `need_rescan` or `Flag::` anywhere
in the crate) nor watcher errors, which are logged and discarded. An overflow
event typically carries no paths, so `changed` stays empty and the callback
returns early. Because `Inventory::load` caches parsed files by path with no
mtime or digest revalidation, and is only invalidated by watcher events, a single
missed event leaves the daemon serving stale content **until it is restarted**.

This is a plausible mechanism for tracked issue
[#59](https://github.com/kapicorp/krab/issues/59), "Daemon kept serving a stale
render after a class file was reverted with git checkout": a `git checkout` can
produce a burst of events large enough to overflow the inotify queue. I did not
reproduce it, so this is a hypothesis with a cheap test, not a conclusion. The
test is in M5.

The log level is part of the same defect. The path that drops a batch reports it
as a warning and returns:

```rust
// watch.rs:26-33
Err(errors) => {
    for e in errors { tracing::warn!("watch error: {e}"); }
    return;                       // the whole batch is discarded
}
```

Losing a batch means the daemon is now serving stale content for the rest of its
life, which is a correctness failure rather than a degraded-but-continuing
condition, and `warn` understates it. The two `"cannot watch {dir}"` sites
(`watch.rs:80` and `:101`) are the same class: a symlinked directory that fails
to register means files under it never invalidate, silently, for the daemon's
lifetime. All three belong at `error`, and the dropped batch should additionally
force a reload rather than only being reported.

**F19. Every daemon of every build shares one log file, and no line says which
one wrote it.** The socket name carries the build, the log name does not:

```
socket_path()  ->  <inventory_key>-<build>.sock      build in the name
log_path()     ->  server-<inventory_key>.log        no build   (paths.rs:67-69)
```

So all daemons for one inventory, across every build, append to one file, which
the *client* opens in append mode. No tracing event carries a pid, a build or the
generation counter. Observed during this assessment: several daemons were alive
for the same inventory at once, spawned by the respawn loop in F18, all writing
to that single file with nothing to tell them apart.

This matters more than it looks because the log is a real consumer surface, not
just a debugging aid: when a daemon fails to start, the client puts the last five
lines of it into the error it shows the user (`client.rs:151-157`). That is
exactly the situation where interleaved output from a previous daemon is most
likely and least welcome.

Scale context: there are 24 `tracing` call sites in the workspace, 6 warn, 10
info, 8 debug, no error and no trace, and 10 of them already carry structured
fields. There is one subscriber, configured at `main.rs:113-119`. The gap is the
missing identity fields, not the machinery. Untracked; I found no issue covering
it.

**F20. The daemon's fatal errors are the only lines in its log that are not log
events.** There are 24 `tracing` call sites in the workspace: 6 warn, 10 info, 8
debug, and **zero error**. That is not a stylistic choice; the fatal paths bypass
`tracing` entirely. `krab_server::run` returns `io::Result<()>`:

```rust
let listener = server.bind()?;                    // AddrInUse: no log event
let _watcher = watch::start(&root, state)?;       // no log event
accept.join().unwrap_or_else(|_| Err(io::Error::other("the accept loop panicked")))
```

Each of these propagates to `main`'s handler and emerges as
`eprintln!("error: {m}")`. For a daemon spawned detached, with stdout and stderr
appended to the log file, that lands as a bare unlevelled line. A panic in the
initial render does the same through the default panic hook.

So the single most important line in a daemon log is the one line that is not
structured. This also blocks the JSON format item under F19: a structured log
with an unstructured fatal line in it is worse than either, so the routing has to
be fixed first.

Two related ergonomics gaps, neither a defect: the filter is read from
`RUST_LOG`, outside the `KRAB_` environment namespace the project otherwise owns
(`KRAB_INVENTORY_PATH`, `KRAB_NO_DAEMON`, `KRAB_PYTHON`, and a `KAPITAN_*` to
`KRAB_*` migration shim at `main.rs:81-97`), which leaks the implementation
language into the user interface; and there is no global `-v`/`--verbose`, so the
knob is undiscoverable without knowing the variable name. With a `warn` default
and only 6 warn sites, an ordinary CLI run emits essentially nothing.

The level taxonomy itself needs no redesign. Warn for degraded-but-continuing,
error for correctness-lost or fatal, info for daemon lifecycle, debug for the
event stream: 21 of the 24 sites already follow it, and the 3 that do not are the
`watch.rs` sites in F6.

**F18. `krab lsp` spins and leaks processes when its daemon goes away.** Observed
live on this machine, not inferred. The LSP's diagnostics thread long-polls
`inventory.wait`; on failure it reconnects, and the reconnect path calls
`connect_or_spawn`, which **spawns a new daemon**. There is a 5 s sleep only when
obtaining the client fails, not when the client is obtained and the call then
fails, so that second path has no back-off at all. The thread is also never
stopped on LSP shutdown.

After the smoke-script run in Section 3 exited without shutting the server down,
the orphaned `krab lsp` was still running some minutes later with two zombie
children and a freshly spawned daemon, which had in turn spawned two Python
resolver workers:

```
 2093882     1979  krab lsp
 2093886  2093882  [krab] <defunct>
 2136118  2093882  [krab] <defunct>
 2136398  2093882  krab server run --inventory-path ... --idle-timeout 1800
 2136424  2136398  python3 .../resolver_runner.py
 2136425  2136398  python3 .../resolver_runner.py
```

Killing the daemon produced another one within seconds. Two consequences worth
separating: an editor whose daemon is stopped gets an unbounded respawn loop
rather than a degraded mode, and `scripts/lsp-smoke.py` leaves that loop behind
every time it is used. Untracked.

Related, from reading the code rather than observation: when the daemon is
unavailable, `publish_diagnostics` returns without clearing, so the editor keeps
showing the last known diagnostics indefinitely, and hover reports "No target
includes this file" which is indistinguishable from a genuinely unused file.

**Daemon and local do agree on rendered output.** I verified that directly. They
do not agree on everything else:

* `--raw` silently disables the daemon as well as normalisation. The flag's help
  text mentions only normalisation. There is no way to get raw output from a
  daemon.
* `App::client()` falls back to local rendering on any connect or spawn failure
  with a `tracing::warn!`, visible on stderr only.
* Warnings are dropped on the daemon path for `inventory.all`; `AllResult`
  carries errors only.
* `inventory targets` reports failing targets from the daemon and cannot from the
  local path.
* `--pattern` uses two different implementations with two different errors.
* `${oc.env:...}` resolves in the **daemon's** environment, inherited from
  whichever CLI first spawned it. This is documented in `docs/CLI.md`.
* The socket key covers the inventory path and the binary's size and mtime, but
  not `.kapitan` or cwd. Two shells with different `.kapitan` settings share one
  daemon, which froze the first one's configuration for its lifetime.

None of these is a rendering divergence, so `CONTRIBUTING.md`'s rule is not
violated in letter. Several are surprises a user would call a bug. There is no
automated test for any of it, because `krab-server` has **zero tests**.

### 4.6 Side effects, secrets and distribution

This cluster is entirely untracked and is the main original contribution of this
review. All line references are to the inspected revision.

**F7. `azkms` puts the plaintext secret in the process command line.**
`refs/kms_cli.rs::az_crypto` passes `--value <base64(plaintext)>` as an argument
to `az`. Command lines are world-readable through `ps` and `/proc/<pid>/cmdline`.
The sibling backends do not do this: `awskms` uses a temp file and `gpg` uses
stdin. Verified by reading the code.

**F8. `awskms` writes the plaintext secret to `TMPDIR`.** Created `create_new`
with mode `0600` and unlinked on `Drop`, which handles the ordinary cases. It is
not overwritten before unlink and `Drop` does not run on `SIGKILL` or on a panic
with `panic=abort`.

**F9. The two HTTP clients that talk to third-party hosts have no global timeout
and no size limit.** `fetch.rs::download` and the OCI `Registry` set
`timeout_connect(30s)` only, and read bodies with `.limit(u64::MAX)`. ureq's
defaults for the global and body timeouts are `None`. A server that completes the
handshake and then stalls hangs the compile forever. Archives are gunzipped into
memory with no cap. There are no retries anywhere. By contrast the Vault and GCP
KMS clients do set `timeout_global(60s)`, so this is an inconsistency, not a
house style.

**F10. Archive symlink entries are dereferenced by the copy step.** Path
traversal itself is prevented: the `tar` and `zip` crates both reject `..` and
absolute entry paths. But both *create* symlink entries with attacker-chosen
targets, and Krab then copies the extracted tree with `safe_copy_tree` or
`copy_tree`, which use `std::fs::copy` and therefore follow symlinks. An
untrusted archive fetched through `type: http` with `unpack: true` can contain
`creds -> /home/<user>/.aws/credentials` and have that file's *contents* copied
into the compiled output tree, which is typically committed.

**F11. OCI manifests are never verified against a pinned digest.** Layer blobs
are verified before being written, correctly and before the write. The manifest
is not: `oci.rs::pull` fetches `/manifests/<reference>` and parses it without
checking that the bytes hash to the digest, even when the reference *is* a
`sha256:` digest. The index path follows the first child manifest, also
unverified and with no platform matching. A substituted manifest defeats the
layer checks, because the layer digests come from it.

**F12. OCI credentials are sent to a challenge-controlled host.**
`OCI_USERNAME`/`OCI_PASSWORD` go as HTTP Basic to the realm URL taken from the
registry's own `WWW-Authenticate` header, with no host or scheme restriction.
With `insecure: true` the first request is plaintext HTTP, so an on-path attacker
can inject a 401 whose realm points anywhere. Note also that `insecure` and
`tls_verify` are orthogonal, so `insecure: true` alone downgrades the whole
exchange to cleartext.

**F13. Ref files are written non-atomically.** `RefController::write` uses a plain
`std::fs::write`. `krab refs --update` and `--update-targets` decrypt and rewrite
in place, so an interruption or a full disk truncates the only copy of an
encrypted secret. `Manifest::save` in the same workspace already does tmp-write
plus rename, so the pattern is available.

**F14. `krab refs --write` overwrites an existing ref with no prompt, no
`--force` and no backup.** For a `gpg` or KMS secret with no other copy that is
unrecoverable.

**F15. `vaultkv` pushes the secret to Vault before writing the ref file.** If the
file write then fails, the Vault value is orphaned and the next run generates a
fresh random secret and overwrites that Vault key.

**F16. No signal handling anywhere in the compile path.** The only signal code in
the workspace is `libc::signal(SIGPIPE, SIG_DFL)`. Ctrl-C during `install`, which
deletes the old contents of the target directory before moving the new tree in,
leaves a half-written output tree. It self-heals on the next run only because no
manifest record is written on that path. The per-run temp tree under `TMPDIR` is
always leaked, and pooled Python workers are orphaned until their stdin reaches
EOF.

Not a defect, worth knowing: `skip_verify` defaults to **true** for Vault, so TLS
verification is off by default. I checked the reference and
`kapitan/inventory/model/references.py` has the same default. This is inherited
parity, not a Krab regression, and changing it would be a deliberate divergence.

Distribution and interface observations:

* The library crates have no `publish` key, so they default to publishable, while
  `[patch.crates-io]` on the vendored `saphyr-parser` means a published
  `krab-inventory` would silently resolve to the unpatched upstream parser and
  lose both PyYAML compatibility patches. Tracked as
  [#112](https://github.com/kapicorp/krab/issues/112); PR #101 sets
  `publish = false`.
* The vendored `saphyr-parser` carries two hand-applied patches to `scanner.rs`
  with no `.patch` file, no vendoring script and **no test asserting either
  patched behaviour**. A regression in the block-scalar patch silently changes
  rendered values. The workspace `Cargo.toml` comment still says "a one-line
  patch" while `vendor/README.md` documents two.
* `scripts/lsp-smoke.py` and `scripts/lsp-smoke-live.py` open a hard-coded
  `/tmp/claude/lsp_stderr.txt` and fail immediately on any machine where that
  directory does not exist. These are the project's only LSP verification tools
  and neither runs in CI. Untracked.
* `.claude/skills/krab/SKILL.md` still names the VS Code extension
  `kapicorp.kapitan` with settings `kapitan.path` and `kapitan.python`;
  `editors/vscode/package.json` uses `kapicorp.krab` and `krab.*`. Leftover from
  the rename in #95. Untracked.
* `docs/CLI.md` drift: `server logs` is documented as "print its log file" but
  tails 50 lines and has an undocumented `-n/--lines`; `.kapitan` keys
  `compile.yaml-use-rapidyaml` and `compile.yaml-dump-null-as-empty` are read but
  undocumented; the daemon method list omits `inventory.class_usage`. PR #102
  fixes the first, the other two are untracked.

**F21. Two enumerable contract surfaces are entirely undocumented.** Measured, not
estimated:

* **37 distinct diagnostic codes** exist in the tree (`interpolation::` 11,
  `inventory::` 15, `yaml::` 9, `server::` 1, `resolver::` 1). **None** appears
  in `docs/` or `README.md`. These are not internal: `CONTRIBUTING.md` makes them
  a contract, they ship in `--json` output, and they reach editors through the
  LSP. Two tracked issues presuppose an enumerable set that does not exist:
  [#36](https://github.com/kapicorp/krab/issues/36) (`--explain-code <code>`) and
  [#41](https://github.com/kapicorp/krab/issues/41) (`--help-json`).
* **43 resolvers are registered, 14 of them appear in no document**:
  `from_file`, `fullkey`, `oc.create`, `oc.decode`, `oc.deprecated`,
  `oc.dict.keys`, `oc.select`, `to_csv`, `join_quoted`,
  `nested_dict_to_list_of_dicts`, `worker_cluster_gpu_configs`,
  `gcp_artifact_registry_multi_region_location`,
  `gcp_cloud_storage_multi_region_location`,
  `filter_tenants_by_execution_location`. This sits beside
  [#122](https://github.com/kapicorp/krab/issues/122), thirteen resolvers the
  reference does not have accepted silently: different sets, one root cause, the
  registry is the only thing that knows what exists and nothing derives from it.

Untracked. The value is less the documentation than the prerequisite: #36 and #41
both need the codes enumerable in the binary, which is the same work.

**F22. The same fact is written down in several places, and only some of those
copies are checked.** Current duplication surface, excluding `target/`, `vendor/`
and `.git/`:

| Fact | Appears in | Kept true by |
|---|---|---|
| kapitan `0.36.3` | `README.md`, `CONTRIBUTING.md`, `docs/ROADMAP.md`, `tests/fixtures/README.md`, `.claude/skills/krab/SKILL.md`, **`crates/krab-inventory/tests/fixture.rs`**, **`crates/krab-compile/src/refs/mod.rs`** | PR [#110](https://github.com/kapicorp/krab/pull/110), markdown only |
| Rust `1.85` | `CONTRIBUTING.md`, `README.md`, `docs/GETTING-STARTED.md`, `Cargo.toml` | PR [#101](https://github.com/kapicorp/krab/pull/101), removed from prose |
| `2.0.0-alpha.4` | `README.md`, `CONTRIBUTING.md`, `docs/ROADMAP.md`, `.claude/skills/krab/SKILL.md`, `Cargo.toml` | PR [#105](https://github.com/kapicorp/krab/pull/105), derived |
| `ubuntu-22.04` / glibc `2.35` | `CONTRIBUTING.md`, `README.md`, `.github/workflows/release.yml` | **nothing** |

Three observations, in order of how much they matter:

1. **The glibc and runner pair is covered by nothing**, and PR
   [#107](https://github.com/kapicorp/krab/pull/107) changes the aarch64 job to a
   native `ubuntu-22.04-arm` runner, which is exactly the edit that invalidates a
   hand-written glibc floor.
2. **#110 scans markdown only.** I read its diff: it walks every `.md` under the
   root skipping `target`, `vendor`, `node_modules` and `.git`, so
   `.claude/skills/krab/SKILL.md` *is* covered. The two Rust files naming the
   parity version are not.
3. **It checks consistency, not correctness.** The first copy found is taken as
   the reference, so all copies being wrong together passes. The test says so
   itself: *"There is no machine-readable source for it, so this checks the
   copies against each other."* For a fact with no authoritative source that is
   the best available, and it is worth knowing rather than fixing.

Three pull requests solve three instances of one class with three different
mechanisms, and nothing states which facts are duplicated or how each is kept
true. Related rename debt of the same shape, found while checking: the workspace
`Cargo.toml` comment says the vendored parser carries "a one-line patch" while
`vendor/README.md` documents two, and `python.rs`'s doc comment says
`~/.cache/kapitan` where the code joins `krab`.

### 4.7 Output collisions

**F17. Output written outside a target's own directory is silently discarded, and
colliding output paths are undetected.** Each target compiles into a private temp
tree and `install` moves only `temp/compiled/<target_path>`. Anything a target
writes outside that subtree is dropped without a warning.

Reproduced against the reference with two targets sharing
`output_path: ../shared`:

```
krab compile     -> "2 compiled"; compiled/ contains only .krab-manifest.json
kapitan compile  -> "Compiled 2 targets"; compiled/shared/t.j2 exists
```

Krab reports success and writes nothing. Separately, for a genuine collision
inside the compiled tree, `install` deletes every non-child entry of the target
directory, so whichever target installs last wins, with no warning. Nothing
checks for duplicate `target_path` values either.

Only [#120](https://github.com/kapicorp/krab/issues/120) touches `output_path`,
and it is the different failure `output_path: .`. This is untracked.

---

## 5. Compatibility matrix

Authoritative requirement throughout is `CONTRIBUTING.md`: byte-identical to
kapitan 0.36.3 with the OmegaConf inventory backend. See decision D4.

Treatments: **preserve** (must stay identical), **bridge** (differs, adapter
exists), **explicitly change** (deliberate divergence, must be documented),
**unresolved**.

| Behaviour | Authoritative evidence | Current implementation | Treatment | Validation |
|---|---|---|---|---|
| Rendered inventory YAML, `kapitan inventory -t` | Reference output; `tests/fixtures/expected/*.yaml` | `emit/yaml.rs`, a port of PyYAML's emitter | preserve, byte-for-byte | **Executed.** Regenerated the 3 expected files with real kapitan 0.36.3: identical. 3/3 and a separate 100/100 synthetic CLI comparison identical. PR #97 makes this a CI job |
| Merge semantics, `EXTEND_UNIQUE`, merge-time dereferencing | `OmegaConf.unsafe_merge` | `merge.rs` | preserve, semantic plus byte | Covered by the fixture test; 3 unit tests |
| Interpolation grammar and three-pass evaluation | OmegaConf ANTLR grammar | `interp/parse.rs`, `interp/eval.rs` | preserve, semantic | Fixture test; 5 parser unit tests. `eval.rs` has no unit tests |
| Class resolution, including the two reclass fallbacks | Reference `resolve_class_file` | `inventory.rs` | preserve, semantic | Fixture test only |
| Shipped resolvers `oc.*`, builtins, contrib | Reference plus the `resolvers.py` contrib set was ported from | `resolvers/` | preserve, semantic | Fixture test. **13 resolvers exist that the reference lacks**, accepted silently: [#122](https://github.com/kapicorp/krab/issues/122) |
| User `resolvers.py` | The file the OmegaConf backend imported | `resolvers/python.rs` plus `resolver_runner.py` | bridge, process boundary | 5 tests, 1 of which does not assert the path its name claims |
| `_root_` handed to a Python resolver | Reference passes an OmegaConf node | Proxy object | **unresolved** | [#117](https://github.com/kapicorp/krab/issues/117): `OmegaConf.select(_root_, ...)` raises |
| `write` resolver | Reference supports it | Hard error | **unresolved** | [#116](https://github.com/kapicorp/krab/issues/116): used by every audited inventory |
| Target naming, `compose-target-name` | Reference defaults to `false` | Defaulted to `true`; no CLI override | explicitly change, being corrected | **Executed.** Same inventory, no `.kapitan`: reference names it `dev`, Krab `env.dev`, and `${_kapitan_.name.full}` differs accordingly. [#124](https://github.com/kapicorp/krab/issues/124), PR [#132](https://github.com/kapicorp/krab/pull/132) |
| Compiled output bytes | Reference `compiled/` tree | `output.rs`, `emit/ryml.rs` | preserve, byte-for-byte | **Executed** for `jinja2` plus `copy` on a synthetic inventory: identical. No committed compiled fixture; `emit/ryml.rs` is covered only by the skipped corpus test |
| Compiled tree contents | Reference writes only compiled files | Krab also writes `compiled/.krab-manifest.json` | explicitly change, documented | Users must gitignore it; `CONTRIBUTING.md`'s `git status --short compiled` check assumes they have |
| Output written outside the target directory | Reference writes it | Krab discards it silently | **unresolved** | **Executed**, see F17. Untracked |
| `external` input freshness | Reference recompiles every run | Krab reports up to date | **unresolved** | **Executed**, see F1. [#16](https://github.com/kapicorp/krab/issues/16) |
| Input types `jsonnet`, `helm`, `kustomize`, `cuelang`, `toml` output | Reference supports them | Not implemented natively | bridge, `--backend python` | Documented in `docs/DESIGN.md` and the skill file |
| Ref file format | `yaml.safe_dump` layout | `refs/mod.rs` | preserve, byte-for-byte | `ref_file_format_matches_pyyaml` |
| Ref backends `plain`, `base64`, `env`, `gpg`, `vault` | Reference `kapitan/refs` | Ported | preserve, semantic | Tested, including a GPG round-trip and a Vault mock server |
| Ref backends `gkms`, `awskms`, `azkms` | Reference | Ported | preserve, semantic | **No tests at all** |
| Vault `skip_verify` default | Reference defaults to `True` | Also `true` | preserve | Verified against `kapitan/inventory/model/references.py`. Inherited weakness, not a regression |
| Dependency fetching | Reference `fetch_dependencies` | `fetch.rs` | preserve, with two documented divergences | 10 tests. Both divergences confirmed implemented as documented |
| Daemon and local produce the same result | `CONTRIBUTING.md` calls divergence a bug | Same library code | preserve | **Executed** for rendering, 3/3. **No automated test exists** |
| CLI flags | `docs/CLI.md` | `clap` | preserve | PR #102 adds a coverage test. Three drifts remain |
| Daemon JSON-RPC | `protocol.rs`, `PROTOCOL_VERSION = 2` | 11 methods plus `server.*` | preserve | **No tests.** Build identity is size plus mtime, not content |
| `krab lsp` over stdio | `editors/vscode/extension.js` | `main.rs` | preserve | Smoke scripts only, and they do not start on a clean checkout |

---

## 6. Architecture gap table

| Recommendation or concern | Current code and evidence | Assessment | Consequence | Proposed action |
|---|---|---|---|---|
| Hexagonal boundary between domain and I/O | `krab-inventory` has no workspace dependency; pure modules are `value`, `merge`, `path`, `classfile`, `model`, `interp`, `emit`, `pyfmt`, `explain`; I/O is confined to `inventory.rs`, `yaml::load_file`, `dotkapitan.rs`, `python.rs` | **Already satisfied** | None | Keep |
| Functional core, imperative shell | Same, with two leaks: `${oc.env:...}` reads the process environment and `${from_file:...}` reads arbitrary paths mid-evaluation | **Partly satisfied, and correctly so** | Both are required for reference parity, and both are already documented as unobservable by the daemon | Keep. Do not purify |
| Replaceable resolver integration | `Registry::register` is public and takes a closure; four sets compose with a documented precedence and a `prefer-native` override | **Already satisfied** | Registration must happen before the `Arc`, so there is no post-construction path. No caller needs one | Keep |
| Replaceable input types or renderers | Hard-coded `match item.input_type` in `native.rs`; no trait. But `Reads` (`inputs/mod.rs:59-72`) is already a shared accumulator passed to every input, so a data-level contract exists | **The indirection is unnecessary; the contract is too weak** | A trait would not have prevented F1: `external` would simply not call `reads.file()`, exactly as it does now. The real gap is that nothing obliges an input to say how it is invalidated, and only `kadet` produces `ItemRecord`s, so a stale target re-runs jinja2, copy and external in full | Do not add a trait. Make invalidation a declared part of `Item` (see the row below). `--backend python` covers unsupported types |
| Declared invalidation for each input type | No input declares anything; `external` silently records nothing (F1) | **Missing, and it is the safety property** | F1 is silently wrong compiled output. A `Freshness { Tracked, AlwaysStale }` field that every input must fill makes the `external` case impossible to reintroduce and matches the reference's behaviour | Add the field and fill it at the five match arms. Folded into M3 |
| Replaceable reference providers | `RefController` dispatches on ref type to `plain`/`base64`/`env`/`gkms`/`gpg`/`vault`/`kms_cli`; each backend invents its own plaintext handling and its own write path | **Missing, and it matters** | Not an extensibility gap: an internal testability and safety gap. `gpg` and `vault` have tests because a fake was possible from outside (a temp keyring, a mock HTTP server); `gkms`, `awskms` and `azkms` have **zero tests** because no seam exists. F7, F8, F13 and F15 are four instances of the same defect, each backend handling plaintext and writes differently | Introduce an internal `Backend` trait purely as a contract-test seam. **Not** a plugin API, no dynamic loading. M6 |
| Selectable inventory backend | `.kapitan` `global.inventory-backend` is parsed into `DotKapitan::inventory_backend` (`dotkapitan.rs:62`, `:117`) and **read by nothing in the workspace** | **Missing, and currently a silent-wrong-output bug** | This dead field is the mechanism behind [#118](https://github.com/kapicorp/krab/issues/118): an inventory the reference runs on reclass is rendered with OmegaConf semantics and reported as success. The reference has this layer (`kapitan/inventory/backends/`) | **Decision-gated on D4.** If reclass is out of scope, read the field and refuse with a diagnostic, roughly ten lines, and build no layer. Build the boundary only if reclass is committed to. The guard is correct either way and should not wait |
| Schema for `parameters.kapitan` | `model.rs` mirrors the reference's pydantic models: fills defaults, orders fields, forces helm `output_type`, rejects unknown fields | **Already satisfied** | This half of the schema question is done | Keep |
| Schema for `.kapitan` itself | Ad-hoc `get()` calls in `dotkapitan.rs`; unknown keys are silently ignored | **Missing, small** | **Reproduced:** setting `prune: true` in `.kapitan` does nothing and says nothing. Same root cause as two keys that are read but undocumented (`yaml-use-rapidyaml`, `yaml-dump-null-as-empty`) and the `docs/CLI.md` drift PR [#102](https://github.com/kapicorp/krab/pull/102) patches with a test | Validate the file and warn on unknown keys. A contained change in one module; it can also generate the documentation table. M7 |
| A logging layer, with logfmt or JSON output | `tracing` with one subscriber (`main.rs:113-119`), 24 call sites, 10 already carrying structured fields. Separately, `Diagnostic` with a stable `code` is rendered by miette, as JSON lines under `--json`, and as LSP diagnostics | **The indirection is unnecessary; the fields are missing** | `tracing` is already the facade, so a layer would sit in front of a facade. The demonstrated defect is F19: one shared log file with no pid, build or generation on any line, which no output format fixes. Conflating the two channels is the real hazard, because `tracing` event names carry no stability promise while diagnostic codes do, and [#41](https://github.com/kapicorp/krab/issues/41) already owns the machine-readable error channel | Add the identity fields. Then a `--log-format=text\|json` switch defaulting to text, scoped to the daemon. Prefer JSON over logfmt purely on cost: `tracing-subscriber` ships `.json()` behind a feature flag and `serde_json` is already a dependency, whereas logfmt needs a new crate for an output format. Do not route diagnostics through it. M7 |
| Slicing the large files into smaller modules | **Measured, not assumed.** Krab `src`: 73 files, median 229 lines, p90 648, max 1651. The same measurement over the crate sources in the local Cargo registry puts Krab *below* the libraries it depends on: its max is under the max of 8 of 12, its p90 under the p90 of 9 of 12 (full tables in 6.2). The three largest are line-by-line ports of PyYAML's emitter, OmegaConf's grammar and `kapitan/refs`, and much of the apparent bulk is in-file tests: `fetch.rs` is 1651 lines of which ~598 are its test module | **Unnecessary; the premise does not survive measurement** | No authoritative standard exists to appeal to. The Rust Style Guide sets a line *length* of 100 characters and says nothing about file length; the API Guidelines cover API design, not layout. "Over N lines is a smell" claims come from aggregator blogs with no evidence behind the number. Splitting a port would cost the one property that matters for it, being diffable against the original, which is the only maintenance those files will ever get | Keep. Size is the wrong trigger; the defensible ones are two distinct audiences, a privacy boundary a module can enforce and a file cannot, merge contention, or compile time, and none currently applies. The one exception is `engine.rs`, 1007 lines with zero tests and seven phases expressed only as statement order: write the end-to-end test first (M2), then split only if the test proves awkward to write |
| Generating documentation from code | 37 diagnostic codes and 14 of 43 registered resolvers documented nowhere (F21); the same version string written into up to 7 files with only some copies checked (F22). Against that, PR [#102](https://github.com/kapicorp/krab/pull/102) already closes CLI flag drift with a *test*, and [#108](https://github.com/kapicorp/krab/issues/108) has already rejected rustdoc/docs.rs ("krab is not on docs.rs and no crate is consumed standalone") and mdBook ("no search need, no versioning need, five documents") | **Partly missing, and the split matters more than the answer** | Generating prose would destroy the explanation to fix drift a test already catches: `docs/CLI.md` is written to be read, not to list flags. Generating a closed set that has no written counterpart costs nothing, because nothing is lost | **Generate the enumerable, verify the prose.** Generate an index of diagnostic codes and of registered resolvers, which are closed sets with no prose today and are the shared prerequisite for #36 and #41. Do **not** generate `docs/CLI.md`; #102's test is cheaper and better. Do not add a docs pipeline, API docs or a site: already decided against with reasons. For duplicated facts, one declared table and one test in place of three ad-hoc mechanisms. M7 |
| Replaceable document source | `DocSource` and `DocProvider` traits, implemented by the CLI, carrying the daemon socket as an opaque path | **Already satisfied** | This is the one seam that genuinely needed to be a trait, and it is | Keep |
| A build plan or task graph | `TargetPlan` is per-target metadata; seven phases are statement order in `engine.rs` | **Unnecessary** | Targets are independent; a DAG would have no edges. The real problem is that `compile()` has no test | Refactor nothing. Add the test (M2) |
| Independently distributed executable plugins | Not present | **Unnecessary** | No extension requirement has been demonstrated. The Python boundary already provides process isolation for the one integration that needs it | Do not build |
| Native Rust ABI plugins | Not present | **Unnecessary and unwise** | Would couple plugins to the compiler version and offer no isolation. Note that process isolation here is about crash containment, not security: kadet workers already execute project-supplied Python with the user's privileges | Do not build |
| Separate crate per concern | Five crates, 24k lines, largest 7.3k | **Already proportionate** | Further splitting would add manifest and version churn for no boundary that is not already enforced by the module system | Keep |
| A narrow published library surface | Every module in `krab-inventory` is `pub`; `krab-compile` reaches into emitter internals | **Partly satisfied, deliberately** | Only matters if the crates are published, and `[patch.crates-io]` prevents that | Defer to PR #101 and [#112](https://github.com/kapicorp/krab/issues/112) |
| CLI is a thin formatter | About 234 of 2825 lines are formatting; `cmd_refs.rs` holds around 550 lines of ported kapitan logic; five RPCs have a second CLI implementation | **Not satisfied; the documentation is wrong, not the code** | The duplicated pairs are exactly where daemon and local diverge (4.5) | Correct `CONTRIBUTING.md`. Do not move code. Add the equivalence test (M5) |
| Compile engine testability | `engine.rs` is 1007 lines and no test constructs a `DocSource`; `manifest.rs`, `digest.rs`, `plan.rs` have no tests | **Missing, and it matters** | F1, F2, F3 and F17 are all defects a compile-level test would have caught | M2 |
| Daemon testability | `krab-server` has zero tests | **Missing, and it matters** | F4, F5 and F6 all live in untested code | M1 and M5 |

### 6.1 Alternatives considered for each substantial change

For every milestone in Section 7 I compared three options, as the assignment
requires.

**M1, daemon socket hardening.**
*Keep and document*: write "do not run the daemon on a host without
`XDG_RUNTIME_DIR`" in the docs. Rejected: the failure is silent, the environments
where it bites are the automated ones, and the fix is about twenty lines.
*Refactor within the crate*: set the directory to `0700` and the socket to `0600`
in `rpc.rs::bind`. This is the proposal.
*New boundary*: a credential-passing handshake or a token in the socket path.
Rejected: a Unix socket with correct modes already expresses "same uid only", and
a token would have to be readable by the kadet workers anyway.

**M2, compile-level golden fixture.**
*Keep and improve locally*: add more unit tests to `native.rs`. Rejected: the
untested part is the phase ordering in `engine.rs`, which unit tests do not
reach.
*Refactor*: extract the phases into testable functions first. Rejected as
premature. Write the end-to-end test, then refactor only if the test is hard to
write.
*New boundary*: a mock `DocSource` crate. Rejected: `DocSource` is a small trait;
an in-test implementation is enough.

**M3, `external` freshness.**
*Keep*: leave it. Rejected: it silently produces wrong output.
*Local change*: mark targets with an `external` item always stale, matching the
reference. Cheapest, loses incrementality for those targets only.
*New contract*: let an `external` item declare `input_paths` that are recorded as
dependencies. More work, preserves incrementality, adds a schema key the
reference does not have and so a documented divergence. This is decision D3.

**M4, `output_path` escape.**
*Keep*: silently discarding output is not defensible.
*Local change*: validate the joined output path once, in `TargetPlan::new` or
`Item` parsing, and emit a diagnostic. This is the proposal.
*Match the reference*: actually write outside the target directory. Rejected:
that breaks the private-temp-tree install and the output tree digest, which is
load-bearing for incremental correctness.

**M5, daemon and local equivalence.**
*Keep*: rely on manual checks, as #128 did. Rejected: `CONTRIBUTING.md` calls
divergence a bug, and nothing enforces it.
*Local change*: one integration test that starts a daemon and compares both paths
over the fixture inventory. This is the proposal.
*New boundary*: a shared trait both paths implement. Rejected: they already share
the library code; what differs is the CLI-side reimplementations, and a trait
would not remove them without moving `cmd_refs.rs`-sized chunks of code.

**M6, a secrets backend contract.**
*Keep and improve locally*: fix F7, F8, F13 and F15 one at a time where they sit.
Rejected, but only just. It is cheaper today and it leaves `gkms`, `awskms` and
`azkms` untested tomorrow, which is how all four arose.
*Refactor within the crate*: extract the shared write path and a plaintext-handling
helper, without a trait. This fixes the four defects and is the cheapest thing
that does. It does not give the three untested backends a seam, so it does not
stop the fifth instance.
*New boundary*: an internal `Backend` trait plus one contract-test suite run
against every backend. This is the proposal, and it is the one place in this
codebase where a new abstraction is carried by evidence: eight implementations,
three of them untested, four defects of one shape. The boundary stays internal.
Publishing it would create an obligation to keep it stable for implementers who
do not exist.

**The inventory-backend layer.**
*Keep*: leave the dead field. Rejected: it produces wrong output silently.
*Local change*: read the field and refuse when it names a backend Krab does not
implement. This is the recommendation while D4 is unanswered, and it is correct
even after D4.
*New boundary*: a real backend trait with OmegaConf and reclass implementations.
Justified only if D4 commits to reclass. Building it first would produce a trait
with one implementation whose sole purpose is to express a refusal, which the
ten-line guard already does.

**A schema layer.**
*Keep*: `model.rs` already schemas the half that decides output bytes.
*Local change*: validate `.kapitan` and warn on unknown keys. This is the
proposal, and it is small.
*New boundary*: a declarative schema system spanning `.kapitan`,
`parameters.kapitan` and generator parameters ([#38](https://github.com/kapicorp/krab/issues/38)).
Rejected for now: it would replace a working hand-written validator to serve a
generator-schema requirement that has not been specified.

### 6.2 Decision-relevant research, and its limits

I looked for counterexamples rather than confirmation. The strongest one is not
from the literature; it is the reference implementation itself, read from the
pinned `kapitan[omegaconf]==0.36.3` virtualenv described in Section 3.

#### The reference implementation is the counterexample

The most natural objection to Section 6 is that Krab's extension points are too
closed, and that an `InputType` trait, an inventory-backend layer and a secrets
interface would make it extensible. **Python kapitan already has all three, and
extensibility is still reported as falling short.** That is a result worth more
than any argument from principle, because it is the same problem domain, the same
users and the same feature set.

What kapitan has:

* `kapitan/inputs/base.py`, a 443-line `InputType` abstract base class, with
  `compile_obj`, `compile_input_path`, abstract `compile_file`, abstract
  `inputs_hash` and `cacheable`, plus `CompilingFile`/`CompiledFile` context
  managers. Nine input types inherit from it.
* `kapitan/inventory/backends/{omegaconf,reclass}`, a real backend boundary.
* `kapitan/refs/secrets/*`, one module per backend.

Why it did not deliver, from the code:

* **Ambient state defeats the interface.** `kapitan/cached.py` is 142 lines of
  module-level mutable globals, reached from 18 modules. `cached.args`, the
  parsed command-line namespace, is read in **24 places**; `cached.inv` in 19.
  There are handler singletons (`gpg_obj`, `gkms_obj`, `awskms_obj`,
  `azkms_obj`) and a `reset_cache()` that exists because globals must be unwound
  by hand. The base class takes `args` through its constructor *and* the
  subclasses reach for `cached.args` around it. An interface that does not close
  over its dependencies is a naming convention, not an extension point: a
  third-party input type has to understand `cached.*` to function.
* **The contract is optional where it matters.** `inputs_hash` and `cacheable`
  are abstract methods, so every input type decides for itself how it
  participates in caching. Nothing in the interface states what an input *must*
  record. This is the same hole as F1 in Krab, arrived at from the opposite
  direction: kapitan has the trait and an optional contract, Krab has no trait
  and an optional contract, and both produce inputs whose invalidation is
  whatever the implementer remembered.

The deferred-rendering complexity has the same shape. `backends/omegaconf`
resolves by repetition rather than by ordering:

```python
OmegaConf.resolve(p)                              # escaped ones become unescaped
OmegaConf.resolve(p)                              # now resolve those
resolved_params = OmegaConf.to_container(p, resolve=True)
resolved_params = process_literals(resolved_params)
```

No fixpoint, no dependency graph, three passes and a cleanup. Two consequences
are visible in the source: `resolve_targets()` ends on a bare
`map(lambda target: target.resolve(), targets)`, which in Python 3 is lazy and
unconsumed and therefore does nothing as written; and `OmegaConfTarget.resolved`,
an internal laziness flag, **escapes into the output bytes**, which is why
`inventory.rs:183` carries `m.insert("resolved", Bool(false))` with the comment
*"kapitan 0.36 leaks this internal flag into its output; kept for byte
compatibility"*. An implementation detail of the deferral became part of the
compatibility contract permanently.

**What Krab did about each, and the pattern in it:**

| kapitan pain point | Krab's answer |
|---|---|
| `cached.args` read in 24 places | The bundled `kapitan.runtime` reads no parsed command line and keeps no process-wide state; configuration is threaded by argument through `InventoryConfig` and `Settings`. Verified: the workspace has two process-wide statics, a helm version cache and a thread-local re-entrancy counter |
| Resolve until it settles | The same three passes, but named, bounded and documented in `interp/eval.rs` as a deliberate reproduction rather than emergent behaviour |
| Secrets handler singletons | An explicitly passed `RefController` |
| An ABC that leaks globals | A `match`, with the `Reads` accumulator threaded through every arm |

Both of Krab's real fixes are **removals**: remove ambient authority, make
implicit passes explicit. Neither is an added layer. That is the evidence behind
this section's recommendations, and it is why the input-type row in the gap table
proposes a mandatory `Freshness` field rather than the trait kapitan already
tried.

**Limits of this comparison.** I read the reference's source; I did not
interview its maintainers, and "extensibility fell short" is a practitioner
account rather than a measured finding. kapitan is also older and carries
constraints Krab does not. And Krab has not escaped all of it: it must reproduce
`resolved: false` and the three-pass scheme permanently, its input extensibility
is deliberately *narrower* than kapitan's, and it has the same
extension-without-a-contract drift on its own side, 13 resolvers the reference
does not have, accepted silently ([#122](https://github.com/kapicorp/krab/issues/122)).

#### File and module structure

Asked whether Krab's files should be sliced up. Measured rather than asserted.

**Krab, `src` only, tests excluded from the count where they are separate files:**
73 files, median 229 lines, p90 648, max 1651, 23,253 total.

The largest files are mostly ports, and much of the bulk is tests in the same
file:

| File | Total | Code | Tests |
|---|---|---|---|
| `krab-compile/src/fetch.rs` | 1651 | ~1053 | ~598 |
| `krab-compile/src/refs/mod.rs` | 1329 | ~1069 | ~260 |
| `krab-compile/src/engine.rs` | 1007 | 1007 | **none** |
| `krab-inventory/src/emit/yaml.rs` | 1000 | ~938 | ~62 |
| `krab-inventory/src/interp/parse.rs` | 856 | ~697 | ~159 |
| `krab-inventory/src/inventory.rs` | 759 | 759 | **none** |

**The ecosystem, measured from the crate sources in the local Cargo registry.**
These are Krab's own dependencies, so the sample is biased toward widely used
libraries, but it is real code rather than recollection:

| Crate | Files | Median | p90 | Max |
|---|---|---|---|---|
| serde_json 1.0.151 | 37 | 218 | 1181 | 2714 |
| clap 4.6.6 | 22 | 10 | 246 | 541 |
| regex 1.13.1 | 12 | 724 | 2674 | 2775 |
| rayon 1.12.0 | 100 | 147 | 476 | 3628 |
| minijinja 2.24.0 | 44 | 419 | 1299 | 2052 |
| lsp-types 0.95.1 | 35 | 134 | 616 | 2880 |
| notify 8.2.0 | 9 | 602 | 765 | 765 |
| tar 0.4.46 | 8 | 633 | 1766 | 1766 |
| zip 7.2.0 | 27 | 250 | 1768 | 4077 |
| rsa 0.9.10 | 32 | 126 | 588 | 859 |
| ureq 3.4.2 | 39 | 346 | 1067 | 1492 |
| indexmap 2.14.2 | 27 | 316 | 1298 | 1865 |

**Krab's files are smaller than those of the libraries it depends on.** Its
maximum, 1651, is below the maximum of eight of these twelve; its p90, 648, is
below the p90 of nine of twelve. There is no size problem to solve.

**There is also no authoritative standard to appeal to.** The Rust Style Guide
covers formatting and sets a line *length* of 100 characters; it says nothing
about file length. The Rust API Guidelines cover API design, not file layout.
Neither prescribes a file size, and I found no primary source that does. Claims
of the form "over N lines is a code smell" circulate on aggregator blogs with no
evidence behind the number, and I would not act on them.

**So the trigger for splitting should not be size.** The defensible triggers are:
a module acquiring two distinct audiences, needing a privacy boundary that a
module can enforce and a file cannot, merge contention, or compile time. None of
those currently applies to the six files above, and one argument points the other
way: `emit/yaml.rs`, `interp/parse.rs` and `refs/mod.rs` are line-by-line ports
of PyYAML's emitter, OmegaConf's grammar and `kapitan/refs`. Keeping a port
contiguous is what makes it diffable against the original, which is the main
maintenance activity these files will ever see. Splitting them would trade the
one property that matters for tidiness.

The single file worth revisiting is **`engine.rs`**: 1007 lines, zero tests, and
seven phases expressed only as statement order (Section 4.3). Even there the
recommendation is unchanged, and it is a sequencing point rather than a
structural one: write the end-to-end test first (M2), then split only if the test
proves awkward to write. Splitting untested code first optimises for appearance
and loses the chance to prove the split preserved behaviour.

#### Other material consulted

* **Large Rust workspaces.** *Corrected after re-reading the source.* An earlier
  draft of this document attributed to this article an argument that crate splits
  should follow compile-time and API boundaries. **It says no such thing.** The
  article is about workspace layout only: a flat crate structure, a virtual
  manifest at the root, and each crate named after its folder. It does not
  discuss compile times, encapsulation, or file size. What it does support is
  modest: "until you hit a million lines of code, the number of crates in the
  project will probably fit on one screen." Krab has five crates and about 23k
  lines, which is unremarkable by that measure. It offers no evidence for or
  against splitting further, and I should not have implied otherwise.
* **rust-analyzer and LLVM as architecture references.** Both are cited in the
  assignment. Both are libraries with many consumers and a stable IR; Krab has
  one consumer, its own CLI, and its "IR" is a rendered document that must match
  another implementation byte for byte. The architectural pressure that produced
  their layering does not exist here. I did not find a maintainer account
  suggesting otherwise.
* **Skyframe and incremental correctness.** The relevant lesson is the one Krab
  already violates for `external`: an incremental system is only as correct as
  its weakest dependency edge, and an untracked edge is worse than no
  incrementality because it is silent. Bazel's answer is sandboxing, which is
  disproportionate here. The reference implementation's answer, always rebuild,
  is the cheap correct option and is what M3 option two proposes.
* **Rust ABI plugins.** The reference documentation is explicit that no stable
  ABI exists across compiler versions. Combined with the observation that kadet
  workers already run project-supplied Python at full privilege, a native plugin
  boundary would add coupling without adding a security property.
* **Premature API stabilisation and excessive crate splitting** are the two
  failure modes most relevant to this codebase, and the project has avoided both
  so far. Issue #108 shows the maintainers already reject changes on
  adoption-rate evidence.

I am not claiming exhaustive coverage of the literature, and I have attached no
numerical confidence to any of this. Where a decision depends on something I
could not verify, it is listed in Section 10.

---

## 7. Proposed milestones, in dependency order

Ordering principle: establish evidence in the least-tested subsystems before
changing behaviour in them, and do not collide with the 16 open pull requests.

Prerequisite for all of them: PRs #97, #99, #101 and #107 land first. They fix
the test suite on a developer machine, pin the reference environment, and give CI
a toolchain. Building on an unpinned reference would invalidate any parity
evidence M2 produces.

### M1. The inventory daemon is reachable only by its own user

| Field | Content |
|---|---|
| **Problem and outcome** | F4: with `XDG_RUNTIME_DIR` unset, `/tmp/krab-<uid>` is `0755` and the socket is `0755`, and the protocol has no peer check. Reproduced: a raw `connect()` and one `inventory.all` returned every target document, and `server.shutdown` is equally reachable. Outcome: the runtime directory and socket permit only the owning uid, and a test proves both the allowed and the denied path. |
| **Scope** | `crates/krab-server/src/paths.rs`, `crates/krab-server/src/rpc.rs` (`bind`), a new `crates/krab-server/tests/`. Excluded: the log file, `SO_PEERCRED` unless D2 says otherwise, anything in `krab-compile`, and the `${from_file:...}` path restriction, which is a separate question. |
| **Dependencies** | D1 (how to report), D2 (shared-uid support). Nothing else. Does not touch any open PR's files. |
| **Approach** | Create the runtime directory with mode `0700` and `set_permissions` the socket to `0600` immediately after `bind`. Add the first `krab-server` integration test: start a server on a temp inventory, assert the mode bits of both, assert a same-uid client round-trip still succeeds. Simplest alternative, documenting the limitation, is insufficient because the exposure is silent and occurs in exactly the unattended environments. |
| **Validation** | New test asserts `mode & 0o777 == 0o700` for the directory and `0o600` for the socket, and that `inventory.target` still answers. Manual: `ls -l` under an unset `XDG_RUNTIME_DIR` before and after. Exit criterion: a raw `connect()` from a different uid fails with `EACCES`, and the same-uid path is unchanged. Denied and allowed paths are both proven. |
| **Recovery** | Pure permission tightening in one crate; revert is a one-commit revert. Irreversible effect: a running daemon started by the old binary keeps its old socket mode until it exits. Any workflow that deliberately shares a daemon across uids breaks, which is what D2 decides. |
| **Responsibility** | Author: a `krab-server` contributor. Reviewer: a maintainer, plus whoever owns the answer to D2. No named assignees. |
| **Estimate** | Half a day to a day. Low uncertainty for the change; the test harness is new ground for this crate, which is most of the cost. |

### M2. `krab compile` has an end-to-end regression test

| Field | Content |
|---|---|
| **Problem and outcome** | `engine.rs` is 1007 lines and no test calls `compile()`. `manifest.rs`, `digest.rs` and `plan.rs` have no tests. F1, F2, F3 and F17 are all defects this would have caught. PR #97 explicitly defers compile-level parity because no committed compiled snapshot exists. Outcome: a committed compiled fixture and a test that compiles it and diffs byte for byte, plus coverage of the staleness reasons. |
| **Scope** | A new `tests/fixtures/compile/` inventory with `jinja2`, `copy`, `remove` and `external` items and a committed expected output tree; a new `crates/krab-compile/tests/compile.rs` with an in-test `DocSource`. Excluded: `kadet` (needs Python, keep it in `kadet_runner.rs`), `helm`, refs backends, and any change to production code. |
| **Dependencies** | D5. PR #97 landed, so the reference environment is pinned. Should not start before #75 lands or is closed, since it moves the staging directory. |
| **Approach** | Generate the expected tree with the pinned reference, commit it, and add a CI step regenerating and diffing it, mirroring what #97 does for inventory. Then table-drive `why_stale` over its seven reasons. Improving unit tests in `native.rs` instead is insufficient: the untested part is phase ordering in `engine.rs`, not the individual inputs. |
| **Validation** | `cargo test -p krab-compile` compiles the fixture and byte-compares; a reference-parity CI step regenerates and fails on a diff. Exit criterion: the test fails if F17 is reintroduced and fails if any `why_stale` arm stops firing. Coverage check: the test must not silently skip, so it may not gate on any optional binary or module. |
| **Recovery** | Test-only. Revert by deleting two directories and one CI step. No production effect. |
| **Responsibility** | Author: whoever owns `krab-compile`. Reviewer: the author of PR #97, for consistency with the inventory parity job. |
| **Estimate** | Two to four days, mostly fixture design. Uncertainty: medium, because choosing a fixture that is small, deterministic and still covers the interesting install and cleanup paths is the hard part. |

### M3. `external` inputs no longer serve stale output

| Field | Content |
|---|---|
| **Problem and outcome** | F1, reproduced end to end against the reference. Outcome: after the data an `external` command reads changes, the next `krab compile` produces output matching the reference. |
| **Scope** | `crates/krab-compile/src/native.rs` (the `external` arm and the five match arms), `inputs/mod.rs` (`Item`), `inputs/external.rs`; a documented row in `docs/DECISIONS.md` if D3 chooses the declared-inputs option. Excluded: F2 and F3, unless D3 produces a general definition of "input"; and any input-type trait, which Section 6 rejects. |
| **Dependencies** | **D3 blocks this.** M2, so there is a regression test to write the case into. |
| **Approach** | Two parts. First, close F1: option A, marking such targets always stale, is a small change in the staleness decision and matches the reference exactly; option B, honouring declared `input_paths` as recorded dependencies, keeps incrementality and adds a key the reference does not have. Doing nothing is insufficient, because the current behaviour silently produces output that disagrees with the reference. Second, make the choice structural rather than incidental: add a `Freshness { Tracked, AlwaysStale }` field to `Item` that every input type must fill, so an input that cannot report its reads has to say so. That is what stops a second F1 from being introduced, and it is why a trait is not needed here: `Reads` is already threaded through every input, and a trait would leave `external` free to report nothing exactly as it does today. |
| **Validation** | Extend the M2 fixture with the reproduction from 4.4: compile, change the data file, compile again, assert the output changed. Exit criterion: that test fails on the current `main` and passes after. |
| **Recovery** | Option A is a revert. Option B adds a schema key; removing it later is a breaking change for anyone who adopted it, which is an argument for A unless incrementality on `external` targets is known to matter. The `Freshness` field is internal and carries no compatibility obligation. |
| **Responsibility** | Author: `krab-compile`. Reviewer: whoever answers D3. |
| **Estimate** | Option A, half a day. Option B, two to three days plus documentation. The `Freshness` field adds roughly half a day either way. |

### M4. Output written outside a target's directory is an error, not a silence

| Field | Content |
|---|---|
| **Problem and outcome** | F17, reproduced: Krab reports "2 compiled" and writes nothing, where the reference writes the file. Outcome: an `output_path` that escapes the target directory produces a `Diagnostic` with a code, an origin and a `help`, and a non-zero exit. |
| **Scope** | `Item` parsing or `TargetPlan::new`, plus a diagnostic code. Excluded: actually supporting cross-target writes, and duplicate `target_path` detection, which PR #132 already touches with `inventory::conflicting_targets`. |
| **Dependencies** | M2, for a place to test it. PR #132, to avoid colliding on the target-naming diagnostic. |
| **Approach** | Validate the joined path once, at plan construction, rather than at each dispatch site. Matching the reference by writing outside the directory is rejected: it breaks the private-temp-tree install and the output tree digest that incremental correctness depends on. Record it in `docs/DECISIONS.md` as a deliberate divergence. |
| **Validation** | A test asserting the diagnostic code and a non-zero exit for `output_path: ../shared`. Exit criterion: the silent-success case from 4.7 is impossible. |
| **Recovery** | Revert. Irreversible in one sense: an inventory that relied on the silent discard now fails to compile. That is the point, and it is why it needs a `DECISIONS.md` row. |
| **Responsibility** | Author: `krab-compile`. Reviewer: a maintainer. |
| **Estimate** | One day. |

### M5. Daemon and local are tested, not merely asserted, to agree

| Field | Content |
|---|---|
| **Problem and outcome** | `CONTRIBUTING.md` calls divergence a bug; `krab-server` has zero tests and nothing compares the paths. I verified agreement by hand for three targets. Section 4.5 lists seven places where behaviour other than rendering does differ. Outcome: a test that runs the fixture inventory through both paths and compares, and a decision recorded for each known difference. |
| **Scope** | A new integration test using the M1 harness; a documentation correction to `CONTRIBUTING.md`'s "the CLI only formats" claim. Excluded: removing the CLI-side reimplementations, and F5 and F6, which are separate. |
| **Dependencies** | M1, which builds the daemon test harness. |
| **Approach** | Start a daemon on the fixture inventory, run `inventory`, `targets`, `classes`, `deps` and `explain` through both paths, compare. Each difference is then either fixed or recorded. The alternative, continuing to check by hand, is what #128 did and it does not survive a refactor. |
| **Validation** | The new test. Exit criterion: every difference in 4.5 is either eliminated or has a row in `docs/DECISIONS.md`. |
| **Recovery** | Test and docs only. |
| **Responsibility** | Author: `krab-server` or CLI. Reviewer: a maintainer. |
| **Estimate** | Two days. |

### M6. Every secrets backend obeys one tested contract

| Field | Content |
|---|---|
| **Problem and outcome** | Eight ref backends each invent their own plaintext handling and their own write path, and the inconsistency has produced four defects: F7 (`azkms` passes the plaintext as an `az` argv element, visible in `ps`), F8 (`awskms` writes it to a temp file), F13 (ref files written with a non-atomic `fs::write`, so an interrupted `refs --update` truncates the only copy of an encrypted secret), F15 (`vaultkv` writes to Vault before the ref file, orphaning the value on a failed write). `gpg` and `vault` have tests; `gkms`, `awskms` and `azkms` have none, because no seam exists to fake them. Outcome: one internal `Backend` trait, one contract-test suite every backend passes, and those four defects closed by construction rather than one at a time. |
| **Scope** | `crates/krab-compile/src/refs/`: `mod.rs` (`RefController` dispatch, `write`), `gkms.rs`, `gpg.rs`, `kms_cli.rs`, `vault.rs`, `functions.rs`. Excluded: the CLI surface in `cmd_refs.rs`, ref file format (byte parity with the reference, must not change), the tag grammar, adding any new backend, and **any public or dynamically loaded plugin interface**. F14 (`refs --write` overwriting without a prompt) is a CLI policy question, not a backend contract, and stays in M7. |
| **Dependencies** | **D1**, which decides whether the four defects are reported publicly before the fix lands. Nothing else. Touches no file any open pull request touches. |
| **Approach** | Define `trait Backend { fn encrypt(..) -> Result<Vec<u8>>; fn decrypt(..) -> Result<Vec<u8>>; }` over the existing modules, with two rules the contract test enforces: plaintext never appears in a subprocess argument, and creating or updating a ref file is atomic (tmp write plus rename, the pattern `Manifest::save` already uses). Add a `mock`/in-memory backend, which the reference's honoured `mock` key already anticipates, so the suite can run everywhere without credentials. Then order the operations in `create()` so the ref file exists before a remote value is considered committed, closing F15. The cheaper alternative, fixing the four defects in place without a trait, is insufficient in one specific respect: it leaves `gkms`, `awskms` and `azkms` with no way to be tested, which is the condition that produced all four. |
| **Validation** | One parameterised test suite over every backend: a round-trip through the in-memory backend; an assertion that no backend's constructed argv contains the plaintext (F7); an assertion that an interrupted write leaves the previous ref file intact (F13); an ordering assertion for `vaultkv` (F15). Existing tests must keep passing unchanged: `ref_file_format_matches_pyyaml`, the GPG temp-keyring round trip, the Vault mock-server round trip. Exit criterion: `refs/` has no backend with zero tests, and each of F7, F8, F13 and F15 has a test that fails before the change. Coverage check: the suite must not gate on `gpg`, `aws`, `az` or `gcloud` being installed, or it becomes another silent skip. |
| **Recovery** | Internal refactor plus test; revert is a single revert. Two effects do not roll back cleanly. Changing `azkms` from an argv argument to stdin or a file changes how Krab invokes `az`, so it needs checking against the reference's own invocation before merge. And any ref file written during a partially applied change keeps whatever layout it was written with, which is why the format must not move in this milestone. |
| **Responsibility** | Author: whoever owns `refs/`. Reviewer: a maintainer, plus whoever answers D1. |
| **Estimate** | Three to five days. Medium uncertainty, concentrated in the `gkms` and `kms_cli` fakes, which are the reason those backends were never tested. If the fake for one of them proves disproportionate, ship the trait plus the atomic write plus the argv rule, and record the missing fake as a limitation rather than pretending the backend is covered. |

### M7. The remaining side-effect and secret defects

F9, F10, F11, F12, F14, F16, F18, F19, F20, F21, F22, the `.kapitan` unknown-key
validation from the gap table, the `inventory-backend` guard for
[#118](https://github.com/kapicorp/krab/issues/118), and the `lsp-smoke.py`
hard-coded path. F7, F8, F13 and F15 are handled by M6 and are not repeated here.
These are independent of each other and of the above, and each is small. **Do not
batch them into one change.** Suggested order, highest impact first: the
`inventory-backend` guard (silent wrong output, about ten lines), F11 and F12
(OCI manifest verification and credentials), F10 (symlink dereference), F9
(timeouts and size caps), F18 (LSP respawn loop, cheap and observed live), F16
(signal handling), the observability items below (F19, F20), the `.kapitan`
unknown-key validation, F14, then the smoke-script path. Each wants its own issue, and D1
decides through which channel for the security items. I did not file them.

**The observability items, in dependency order.** These belong together in
sequence but not in one change. Items 1 to 3 are defects; 4 to 6 are ergonomics
and documentation, and none of them is worth doing before 1 to 3.

1. **The three `watch.rs` levels, with F6.** The dropped-batch and
   `"cannot watch {dir}"` sites move to `error`, and the dropped batch forces a
   reload. Do this as part of F6, not as a logging change: the level is the
   symptom, the silent permanent staleness is the disease, and raising the level
   without forcing the reload would only make the failure louder.
2. **Route fatal daemon errors through `tracing::error!` (F20)**, and install a
   panic hook that logs, so `bind()` failures, `watch::start` failures, accept-loop
   panics and render panics stop arriving as bare `eprintln!` text. This is a
   prerequisite for item 5, not an independent nicety.
3. **Identity fields on daemon log events (F19).** pid, build, and the generation
   counter for events that have one. Without it the rest is cosmetic. Open
   choice to record: give the log file the same `-<build>` component the socket
   already has, or keep one file and make every line attributable. One file with
   fields is friendlier to `krab server logs`, which tails a single path.
4. **`KRAB_LOG`**, falling back to `RUST_LOG` so nothing in existing use breaks,
   consistent with the `KAPITAN_*` to `KRAB_*` shim already in `main.rs:81-97`.
5. **`--log-format=text|json`**, or `KRAB_LOG_FORMAT`, defaulting to text and
   scoped to `krab server run`. `tracing-subscriber`'s `json` feature plus one
   line at `main.rs:116`. For a one-shot CLI at the `warn` default you usually
   see nothing, so this earns its place only for the daemon. Blocked on item 2:
   shipping it first produces a structured log with an unstructured fatal line
   in it.
6. **A global `-v`/`--verbose`** mapping to info and debug, so the knob is
   discoverable without knowing an environment variable name. Note the existing
   `refs -v` is hidden and deliberately ignored for kapitan compatibility, so
   the short flag needs checking against that before it is reused.

**The documentation items (F21, F22).** One principle decides all of them:
**generate the enumerable, verify the prose.**

1. **An index of the 37 diagnostic codes (F21).** Generate it; there is no prose
   to lose. Do this as the first half of [#36](https://github.com/kapicorp/krab/issues/36)
   and [#41](https://github.com/kapicorp/krab/issues/41) rather than as a
   documentation task, because both need the codes enumerable in the binary and
   that is the same work. A test that the index matches the tree comes free.
2. **An index of the 43 registered resolvers (F21)**, derived from the
   `Registry`. Its real value is making
   [#122](https://github.com/kapicorp/krab/issues/122)'s divergence from the
   reference visible instead of silent.
3. **The glibc and runner pair (F22)**, which nothing currently checks. Smallest
   of the three and the most likely to break, since #107 is changing the aarch64
   runner. Coordinate with that PR rather than racing it.
4. **One declared table of duplicated facts, and one test over it**, replacing
   the three separate mechanisms in #101, #105 and #110 once those land. Extend
   the scan beyond markdown to the two Rust files that name the parity version.
   Do not start this before those three merge; rebasing a consolidation onto
   three moving PRs costs more than it saves.

Explicitly not in scope, with the reason: generating `docs/CLI.md` (PR #102's
test is cheaper and keeps the prose), rustdoc or docs.rs publication, and a
documentation site. The last two are recorded as rejected in #108 and should stay
rejected.

Two things to settle while doing this, both recorded rather than assumed. The
**channel split** is a contract: the operator log carries no stability promise,
`Diagnostic` codes and `--json` do. Say so where `--log-format` is documented, so
nobody builds a parser against `tracing` field names, and coordinate with
[#41](https://github.com/kapicorp/krab/issues/41), which owns structured errors
on stderr. And the **log file's permissions**: it is created by the client with
default permissions (`client.rs:181-184`) under `$XDG_STATE_HOME/krab/` and
contains changed file paths and render errors. Deliberately excluded from the M1
ExecPlan; it belongs in this sweep rather than being forgotten.

### 7.1 Dependency order

```
  PR #97, #99, #101, #107  (already open; land first)
            |
            +---------------------------+-------------------+
            v                           v                   v
           M1 (daemon socket)          M2 (compile test)    M6 (secrets contract)
            |                           |   \                 |  needs D1 only
            v                           |    \                v
           M5 (daemon/local parity)     |     \               M7 (the rest,
                                        |      +--> M4             independent,
                                        v           (output_path)  any order)
                                       M3 (external freshness + declared
                                           invalidation, blocked on D3)
```

M6 is independent of M1 to M5 and of D2 to D5. It needs only D1, so it can run
in parallel with the daemon and compile work rather than queueing behind it.

---

## 8. How the plan is used after acceptance

Proposed location for execution plans: `docs/exec-plans/<issue>-<topic>.md`. No
such directory exists. Only create it when the first plan is accepted; do not
create it speculatively.

The loop, once a plan is accepted:

1. Read `AGENTS.md`, `CONTRIBUTING.md`, the issue and the current ExecPlan. Check
   `git status`, the branch, and whether anything since the plan's baseline
   affects it. With 16 open pull requests this check is not a formality.
2. Implement one milestone through the existing issue, branch, pull request flow.
   Expand scope only where the accepted outcome requires it, and write down what
   changed and why.
3. Run the checks that establish the affected behaviour. For engine changes that
   means the reference parity job and the fixture test; for daemon changes, the
   daemon and local comparison; for compile changes, incremental invalidation and
   the compiled-tree diff. Do not run unrelated checks to inflate a count.
4. Update Progress, Discoveries and Validation at every milestone and before any
   handoff. Keep issue status on the existing board.
5. The reviewer compares the diff and the recorded evidence against the
   acceptance criteria. Treat a completion claim as a claim.
6. Finish with outcomes and remaining limitations. Update `docs/DESIGN.md` only
   if semantics changed, and add a `docs/DECISIONS.md` row for any deliberate
   divergence. Retain the completed plan.

**A check that skipped its workload is not evidence.** This project has three
tests that report pass while doing nothing, and a fourth whose guard does not
match its name. Any milestone whose acceptance rests on such a test must assert
that it ran.

### 8.1 Document responsibilities

| Artifact | Responsibility |
|---|---|
| GitHub issues and the project board | Priority, ownership, status, acceptance criteria. Unchanged. |
| `docs/DESIGN.md` | Accepted architecture and semantics. |
| `docs/DECISIONS.md` (arriving with PR #96) | Deliberate divergences from the reference, one row each. |
| `docs/exec-plans/<issue>-<topic>.md` | Execution detail, decisions, progress, evidence, for changes spanning multiple steps or sessions. |
| `AGENTS.md` (arriving with PR #96) | Required reading and when a plan is needed. |

### 8.2 Agent instructions

**Do not create an `AGENTS.md`.** PR #96 adds one, plus a one-line `CLAUDE.md`
importing it. The only addition worth proposing, once #96 lands, is three lines:

> Changes spanning multiple steps or sessions get an execution plan under
> `docs/exec-plans/<issue>-<topic>.md`, kept up to date as work proceeds. Small,
> self-contained changes use the issue description. See CONTRIBUTING.md for the
> issue, branch and pull request flow.

Do not copy `docs/DESIGN.md` or `CONTRIBUTING.md` into it. Do not add roles,
scripts or enforcement hooks.

Separately, `.claude/skills/krab/SKILL.md` is currently the repository's only
agent-facing file and it is stale in one respect (the VS Code extension and
setting names, superseded by #95). It also embeds machine-specific absolute paths
from one deployment. Once `AGENTS.md` exists, the skill file should either be
corrected or point at it rather than duplicating it.

### 8.3 On GSD and BMAD

Neither is warranted now, and this assignment forbids installing them. The
triggers to watch:

* Reconsider **GSD** if maintaining context across sessions or coordinating
  independent workstreams becomes a repeated, observable problem. It is not one
  today: the issue tracker plus the board plus `docs/DECISIONS.md` are carrying
  the load, and #108 shows the project already keeps a ledger of what it decided
  against.
* Reconsider **BMAD** if unresolved requirements or stakeholder agreement become
  the binding constraint. The binding constraint today is verification coverage,
  not requirements clarity.

Evaluate either on one bounded change before wider adoption, and never run two
plan systems at once.

---

## 9. First ExecPlan

Selected milestone: **M1**. It is a demonstrated defect, it is untracked, the fix
is small and self-contained, it touches no file any open pull request touches,
and it builds the first test harness for the workspace's only untested crate,
which M5 then reuses. M2 is the more obviously "architectural" choice, but it is
blocked on decision D5 and on PR #75, and M1 is not.

---

### ExecPlan: the inventory daemon's socket is reachable only by the user who started it

Status: Draft, awaiting review
Issue: none yet. Proposed, pending decision D1 on the reporting channel. Suggested
title: "the daemon socket is world-connectable when XDG_RUNTIME_DIR is unset".
Suggested labels: `bug`, `area: inventory`.
Baseline: `6e9e55ba18597b34ab056cfe29cb28698fa97993`

#### Purpose and acceptance criteria

`krab-server` creates its runtime directory and its Unix socket with default
permissions and performs no peer check, so on a host where `XDG_RUNTIME_DIR` is
unset the socket lives at `/tmp/krab-<uid>/...` and is connectable by any local
user. Observed at the baseline revision:

```
drwxr-xr-x.  /tmp/krab-<uid>
srwxr-xr-x.  /tmp/krab-<uid>/<inventory>-<build>.sock
```

A raw `connect()` followed by a single `inventory.all` request returns every
rendered target document, and `server.shutdown` is reachable the same way. With
`XDG_RUNTIME_DIR` set, the parent directory is `0700` and contains the exposure,
so this bites containers, cron jobs and minimal ssh sessions, not a typical
desktop.

What is exposed: every literal inventory value, file content pulled in by
`${from_file:...}` (which has no path restriction), the daemon's environment
variables via `${oc.env:...}`, and absolute filesystem paths in diagnostics and
`server.info`. Revealed secrets are **not** exposed; reveal happens client-side
in `cmd_refs.rs` and the inventory holds reference tags only.

Acceptance criteria, all observable:

1. The runtime directory is created with mode `0700`, whichever branch of
   `runtime_dir()` produced it.
2. The socket has mode `0600` from the moment it is connectable.
3. A same-uid client still connects and `inventory.target` still answers. The
   allowed path is proven, not assumed.
4. A connection attempt as a different uid fails with `EACCES`. The denied path is
   proven.
5. `cargo test -p krab-server` runs at least one test. It currently runs zero.
6. No change to the wire protocol, `PROTOCOL_VERSION`, socket naming, or any
   behaviour visible to a same-uid client.

#### Context and constraints

Code paths:

* `crates/krab-server/src/paths.rs` - `runtime_dir()` returns
  `$XDG_RUNTIME_DIR/krab` or `/tmp/krab-<uid>`; `socket_path()` and `sockets()`
  derive names from it.
* `crates/krab-server/src/rpc.rs` - `Server::bind()` does `create_dir_all(parent)`,
  removes a dead socket, unlinks other builds' dead sockets, then
  `UnixListener::bind` and `set_nonblocking`.
* `crates/krab-server/src/client.rs` - `connect_or_spawn`, the build and protocol
  handshake, and `spawn`, which detaches with `setsid`.
* `crates/krab/src/app.rs` - `build_version()` is `version+size-mtime` of the
  executable and feeds the socket name.

Contracts to reuse, not reinvent:

* `std::os::unix::fs::PermissionsExt`, already used in `refs/gpg.rs` to create a
  GPG home at `0700` and in `inputs/copy.rs`. Follow that style.
* `krab_inventory::Diagnostic` for any new user-facing error.
* The existing dead-socket recovery in `bind()`. Do not restructure it.

Compatibility obligations:

* The socket path must not change. `krab server stop` enumerates sockets by
  prefix across builds, and the Python kadet runner is handed a socket path by
  the compile engine.
* `PROTOCOL_VERSION` stays 2. This changes who may connect, not what is spoken.
* The log file at `$XDG_STATE_HOME/krab/server-<hash>.log` is opened by the
  *client*, not the server, and is out of scope.

Exclusions, deliberate:

* `SO_PEERCRED`. Correct modes already express "same uid only". Revisit only if
  decision D2 says a shared daemon must be supported.
* The `${from_file:...}` path restriction. Real, separate, larger.
* F5 (the `wait_for` lost wakeup) and F6 (unhandled watcher rescan). Same crate,
  unrelated causes. Separate issues.
* Windows. The crate is Unix-only already.
* Tightening the log file's permissions.

Prerequisites:

* Decision **D2**: is a daemon shared across uids a supported configuration? If
  yes, this plan changes from mode bits to a `SO_PEERCRED` allowlist and the
  estimate roughly doubles.
* Decision **D1**: the reporting channel, which determines whether the issue is
  public before the pull request.
* No pull request currently open touches `krab-server`. Re-check at start.

#### Milestones and work

**Milestone 1: a test harness for `krab-server` exists.**
Reviewable outcome: `cargo test -p krab-server` runs a test that starts a server
against a temporary inventory, connects, issues one request and shuts it down
cleanly. This is new ground. `krab-server` has no `tests/` directory and no
`[dev-dependencies]`.

Uncertainty to resolve first: whether the test can drive `krab_server` in-process
(`lib.rs::run` plus a `Connector`) or must spawn the built binary. In-process is
preferable, because it avoids depending on build layout, but `run()` blocks on the
accept thread and the idle timeout is the only exit besides `server.shutdown`.
Resolve by reading `lib.rs` and `rpc.rs::serve_on` before writing the test. If
in-process proves awkward, spawning is acceptable; say so in Discoveries.

Second uncertainty: the test must exercise the `/tmp/krab-<uid>` branch, so it has
to control `XDG_RUNTIME_DIR`. Setting environment variables in a Rust test is
process-global and racy with other tests. Prefer a helper that takes the runtime
directory explicitly over mutating the environment. If that means a small
refactor of `runtime_dir()`, keep it to threading a parameter through; do not
restructure `paths.rs`.

**Milestone 2: the directory and socket are created with restrictive modes.**
Reviewable outcome: `create_dir_all` for the runtime directory uses `0700`, and
the socket is set to `0600` in `bind()` immediately after `UnixListener::bind`,
before `set_nonblocking` returns.

Note the ordering constraint: there is a window between `bind()` and
`set_permissions` in which the socket exists with default modes. It is short but
not zero. If closing it matters, bind inside a directory that is already `0700`,
which the same change provides, and say so explicitly rather than claiming the
window does not exist.

Also handle the pre-existing directory case: `create_dir_all` succeeds on a
directory that already exists with the wrong modes, including one an attacker
pre-created at the predictable `/tmp/krab-<uid>` path. Decide and document
whether to tighten it, or to refuse to start. Refusing is safer; tightening is
friendlier. This is the one genuine design question in the plan.

**Milestone 3: both the allowed and the denied path are proven.**
Reviewable outcome: a test asserting the mode bits, a test asserting a same-uid
round-trip, and a check of the cross-uid denial. The last cannot run as an
ordinary unit test without a second uid. Options, in order of preference: assert
the mode bits and document the derivation; or gate a genuine cross-uid test on
being run as root with a `#[cfg]` or an environment guard. **If the gated form is
chosen, it must not be a silent skip.** This repository already has three tests
that report pass while doing nothing, and adding a fourth would undermine the
acceptance criteria. Print the skip and, preferably, fail in CI if the guard did
not fire where it was expected to.

**Milestone 4: documentation.**
Reviewable outcome: a sentence in `docs/CLI.md` where the socket path is
described, stating that the socket is owned by and readable only by the user who
started the daemon. A `docs/DECISIONS.md` row only if D2 makes this a deliberate
divergence from something.

#### Validation and recovery

Commands:

```sh
cargo test -p krab-server                 # must go from 0 tests to >0
cargo test --locked --no-fail-fast        # whole suite, no regression
cargo clippy --all-targets --locked       # warnings are denied
cargo fmt --all --check
```

Required environment: Linux or macOS. No Python, no network, no reference
implementation. This milestone deliberately needs none of the optional
dependencies, so it cannot silently skip.

Manual check, before and after:

```sh
env -u XDG_RUNTIME_DIR krab inventory -t <target> >/dev/null
ls -ld /tmp/krab-$(id -u)
ls -l  /tmp/krab-$(id -u)/
env -u XDG_RUNTIME_DIR krab server stop
```

Before: `drwxr-xr-x` and `srwxr-xr-x`. After: `drwx------` and `srw-------`.

Expected results: all four acceptance criteria that can be machine-checked pass;
the cross-uid denial is either tested under root or derived from the mode bits
and stated as such.

Coverage check: confirm the new tests actually executed. `cargo test -p
krab-server` must not report `0 tests`, and no new test may return early on a
missing optional dependency.

Recovery: the change is confined to one crate and is a permission tightening, so
a revert restores the previous behaviour completely. Two effects do not roll
back. A daemon already running under the old binary keeps its old socket mode
until it exits, so operators should run `krab server stop` after upgrading. And
if Milestone 2 chooses to refuse to start on a wrongly-permissioned pre-existing
directory, users with such a directory will need to remove it once; that is a
one-time manual step and belongs in the pull request description.

Fallback if Milestone 1 stalls: if an in-process harness proves genuinely
impractical within a day, ship Milestones 2 and 4 with a shell-level check in the
pull request description as evidence, and file the harness as its own issue
blocking M5. Do not ship the permission change with no test at all.

#### Progress

- [ ] M1: decide in-process versus spawned harness; record the reason
- [ ] M1: `crates/krab-server/tests/` with one passing round-trip test
- [ ] M2: decide the pre-existing wrongly-permissioned directory policy
- [ ] M2: runtime directory created `0700`
- [ ] M2: socket set to `0600` in `bind()`
- [ ] M3: mode-bit assertions
- [ ] M3: same-uid round-trip assertion
- [ ] M3: cross-uid denial, tested or derived and stated
- [ ] M4: `docs/CLI.md` sentence
- [ ] Full suite, clippy and fmt clean
- [ ] Manual before-and-after recorded in the pull request

#### Discoveries and decisions

*Nothing recorded yet. This plan has not been executed.*

Record here: the harness decision and why; the pre-existing-directory policy and
why; anything learned about `bind()`'s dead-socket recovery that changes the
approach; and whether the bind-to-chmod window turned out to matter.

One item is already known and should be carried into execution: `bind()` contains
an unsynchronised sequence of `exists()`, `socket_alive()`, `remove_file()`,
`bind()`. Two concurrent starters of the same build can interleave such that one
unlinks the other's live socket. That is a separate defect and **is not in scope
here**, but whoever edits `bind()` will see it. File it; do not fix it in this
change.

#### Outcomes and handoff

*Not started. Nothing to record.*

On completion, record: the observed mode bits before and after, the exact test
names added and their output, whether the cross-uid case was tested or derived,
and any remaining exposure (for example the log file, and `${from_file:...}`
content still being readable over the socket by the owning uid). The next action
after this plan is M5, which reuses the harness from Milestone 1.

---

## 10. Material unknowns and evidence limitations

Each blocking unknown, with its decision impact, how to resolve it, the role that
should own it, and the milestone it affects.

| Unknown | Decision impact | Resolution method | Proposed role | Affects |
|---|---|---|---|---|
| Is a daemon shared across uids supported? | Mode bits versus a `SO_PEERCRED` allowlist; roughly doubles M1 | Maintainer decision (D2) | Maintainer | M1 |
| Through which channel should F7-F16 and F18 be reported? | They stay only in this document until answered | Maintainer decision (D1); `SECURITY.md` arrives with PR #96 | Maintainer | M6, M7 |
| Should `external` be always-stale or declare inputs? | Half a day versus three days, and a schema key the reference lacks | Maintainer decision (D3), informed by whether any real inventory has slow `external` items | Maintainer plus whoever owns [#16](https://github.com/kapicorp/krab/issues/16) | M3 |
| Is byte-parity with 0.36.3 OmegaConf still the sole target? | Whether [#118](https://github.com/kapicorp/krab/issues/118) and [#119](https://github.com/kapicorp/krab/issues/119) are bugs or out of scope; adds a column to Section 5 | Maintainer decision (D4) | Maintainer | all |
| Is a compile-level golden fixture wanted? | Whether M2 happens at all | Maintainer decision (D5), given #128's real-inventory method | Maintainer plus the author of PR #97 | M2, and M3 and M4 which depend on it |
| Does an inotify overflow actually cause [#59](https://github.com/kapicorp/krab/issues/59)? | Whether F6 is the root cause or a second bug | Cheap experiment: run the daemon with `RUST_LOG=debug`, create enough events to overflow the queue, then change one class file and check whether the render updates | A `krab-server` contributor | not on the critical path; would inform M5 |
| Do the README's 160-target parity and speed claims hold? | Whether the synthetic evidence here generalises | Run `krab inventory check` and `krab compile --force` against a real inventory with the pinned reference | Whoever has access | none directly; limits every conclusion here |
| Does `emit/ryml.rs` still match the reference? | It is covered by no test that runs anywhere, including CI | Populate `KRAB_CORPUS` and `KRAB_COMPILED` and run `corpus_parity` | Whoever has access to a compiled corpus | would strengthen M2 |
| Do the two vendored `saphyr-parser` patches still hold? | A silent regression changes rendered values | Add two parser unit tests asserting the patched behaviour | `krab-inventory` | not planned above; worth an issue |

Evidence limitations, restated so they are not lost: no production inventory, no
corpus, no kadet execution, no live refs backends, Linux only, single-run
synthetic benchmarks against a debug build, and no reproduction of the audit in
#128. Three of the 79 tests in the suite report pass without doing anything, and
a fourth does not test what its name says.

---

## 11. References

Repository, at the inspected revision `6e9e55ba`:

* [`docs/DESIGN.md`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/docs/DESIGN.md)
* [`CONTRIBUTING.md`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/CONTRIBUTING.md)
* [`docs/ROADMAP.md`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/docs/ROADMAP.md) and the [project board](https://github.com/orgs/kapicorp/projects/5)
* [`crates/krab-compile/src/plan.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-compile/src/plan.rs)
* [`crates/krab-compile/src/engine.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-compile/src/engine.rs)
* [`crates/krab-server/src/paths.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-server/src/paths.rs), [`rpc.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-server/src/rpc.rs), [`state.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-server/src/state.rs), [`watch.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-server/src/watch.rs)
* [`crates/krab-compile/src/refs/kms_cli.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-compile/src/refs/kms_cli.rs), [`fetch.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-compile/src/fetch.rs), [`oci.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-compile/src/oci.rs)
* [`.github/workflows/ci.yml`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/.github/workflows/ci.yml) and [`crates/krab-inventory/tests/corpus.rs`](https://github.com/kapicorp/krab/blob/6e9e55ba18597b34ab056cfe29cb28698fa97993/crates/krab-inventory/tests/corpus.rs)

Issues and pull requests referenced: [#16](https://github.com/kapicorp/krab/issues/16),
[#59](https://github.com/kapicorp/krab/issues/59),
[#96](https://github.com/kapicorp/krab/pull/96),
[#97](https://github.com/kapicorp/krab/pull/97),
[#98](https://github.com/kapicorp/krab/issues/98),
[#99](https://github.com/kapicorp/krab/pull/99),
[#101](https://github.com/kapicorp/krab/pull/101),
[#102](https://github.com/kapicorp/krab/pull/102),
[#107](https://github.com/kapicorp/krab/pull/107),
[#108](https://github.com/kapicorp/krab/issues/108),
[#112](https://github.com/kapicorp/krab/issues/112),
[#113](https://github.com/kapicorp/krab/issues/113)-[#127](https://github.com/kapicorp/krab/issues/127),
[#128](https://github.com/kapicorp/krab/issues/128),
[#132](https://github.com/kapicorp/krab/pull/132).

Reference implementation: `kapitan[omegaconf]==0.36.3` with
`omegaconf==2.4.0.dev4`, CPython 3.11. Note that kapitan 0.36.3 declares
`omegaconf>=2.4.0.dev3,<3`, but `ListMergeMode`, which it imports, exists only in
`2.4.0.dev3` and `2.4.0.dev4`. A fresh unpinned install of the documented
reference environment fails with `ImportError`. PR #97 pins it.

Primary source for the counterexample in Section 6.2: the reference
implementation's own source, read from the pinned virtualenv described in
Section 3. Specifically `kapitan/cached.py`, `kapitan/inputs/base.py`,
`kapitan/inventory/inventory.py` and
`kapitan/inventory/backends/omegaconf/__init__.py`. This is implementation
evidence rather than literature, and it is the material that carried the
input-type decision.

Other external material consulted for Section 6.2: the Hexagonal Architecture
article, the LLVM and rust-analyzer architecture descriptions, Bazel's Skyframe
documentation, the Rust reference on external block ABIs, and
"Large Rust Workspaces". These informed the reasoning; none of them was decisive
on its own, and I have not claimed exhaustive coverage.
