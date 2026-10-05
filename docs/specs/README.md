# Specifications

Each file in this directory states what one area of krab must do, in a form a
reviewer can check and a test can verify. `DESIGN.md` explains why the code is
shaped the way it is, `DECISIONS.md` lists where krab deliberately differs from
kapitan, and `CLI.md` lists the flags. A spec links to those documents instead
of repeating them.

A requirement that copies kapitan's behaviour or departs from it follows the
surfaces rule in [DECISIONS.md](../DECISIONS.md).

The first versions were written after the code, from the commit history, the
pull requests and the tests on `main` at `a4fb9c4`. They record what krab does
and what it is known not to do yet. From here on a change starts by editing the
spec it touches, in the same pull request as the code, and the issue links to
the requirement it adds or changes.

| Spec | Prefix | Covers |
|---|---|---|
| [inventory.md](inventory.md) | INV | Loading, target names, class resolution, merging, `.kapitan` |
| [interpolation.md](interpolation.md) | RES | `${...}` interpolation, built-in and Python resolvers |
| [compile.md](compile.md) | CMP | Compile pipeline, target selection, incremental builds, the manifest |
| [inputs-and-output.md](inputs-and-output.md) | OUT | Native input types, output types, YAML emission |
| [refs.md](refs.md) | REF | Secret refs, backends, `krab refs` |
| [fetch.md](fetch.md) | FET | Dependency fetching: git, http, helm, OCI |
| [daemon.md](daemon.md) | SRV | The inventory server, its socket, watching and re-rendering |
| [lsp.md](lsp.md) | LSP | The language server |
| [cli-and-release.md](cli-and-release.md) | CLI | Command surface, exit codes, JSON output, CI, releases |

## How a spec is laid out

A spec opens with a short header:

```
Status: As-built | Draft | Accepted | Superseded by <file>
Verified against: main @ <commit>
Code: <paths this spec governs>
```

**Problem** says in a few paragraphs what the area is for, who depends on it
and which constraints shaped it, such as byte parity with kapitan or a cold
start budget.

**Requirements** are grouped by topic. Each one is a single testable statement
with an ID and an RFC 2119 keyword, followed by where it is verified and where
it came from:

```
REF-7  `krab refs --write` MUST NOT replace an existing ref file unless
       --force is given.
       Test: crates/krab/tests/refs_write.rs::write_refuses_to_overwrite_without_force
       Since: #158
```

`Test:` names what checks the requirement: a test function, a fixture together
with the test that runs it, a job in `ci.yml` or `release.yml`, or a smoke
script under `scripts/`. Scripts are run by hand and are not part of CI, so
they count for less than a test. A requirement without any check says
`Test: none`, which is a gap worth closing, not a reason to drop the
requirement. `Since:` names the pull request or commit that introduced the
behaviour. A requirement that `main` does not meet yet says
`Since: not met yet (#issue)` and has a row under Open deviations.

**Acceptance criteria** describe the scenarios that matter end to end, in
Given/When/Then form. Each criterion names the requirements it covers and the
test or fixture that exercises it. Requirements that a single unit test already
pins down do not need a criterion of their own.

**Edge cases** cover failure modes: missing files, broken input, external tools
that are absent or fail, interrupted runs. Each has an ID (`INV-EC-2`) and the
behaviour krab shows.

**Interfaces** describe what other code or users rely on: file formats, the
JSON of `--json` output, the daemon protocol, environment variables. Flags stay
in `CLI.md`.

**Out of scope** lists what the area deliberately does not do, with the reason.

**Open deviations** lists requirements that `main` does not meet yet, each with
its issue. When a fix lands, the row is removed and the requirement gets its
`Test:` line.

## Keywords

MUST and MUST NOT are absolute. SHOULD means the requirement holds unless a
documented reason says otherwise. MAY is left to the implementer. Lower-case
"must" in prose carries no normative weight.
