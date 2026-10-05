# Inventory server

```
Status: As-built
Verified against: main @ a4fb9c4
Code: crates/krab-server/, crates/krab/src/cmd_server.rs,
      crates/krab/src/app.rs (Connector, App::client, build_version),
      crates/krab/src/main.rs (--no-daemon, log filter)
```

## Problem

Rendering a large inventory takes seconds, and most CLI calls and every
editor request need only a few targets of an inventory that changed in one
file since the last call. The inventory server (the daemon, `krab server run`)
keeps the rendered inventory in memory, watches the files it was rendered
from, re-renders only the targets a change affects, and answers JSON-RPC over
a unix socket. The CLI, `krab inventory watch` and the language server
([lsp.md](lsp.md)) are its clients.

Four constraints shape it. The CLI must give the same answer with and without
the daemon, so the daemon runs the same `krab-inventory` code as the local
path and the CLI falls back to a local render whenever the daemon is
unavailable. Users run more than one build of krab against the same inventory
(the shell's binary and a development build an editor points at), so the
socket is keyed on the inventory and the build, and builds never stop each
other's daemon. Configuration read at start-up (`.kapitan`, a `resolvers.py`)
is fixed for the life of the process, so a change to it stops the daemon and
the next request starts a fresh one instead of reloading in place. A daemon
is started implicitly, so start-up must be cheap to lose: the socket is bound
before the initial render, and a second starter fails at bind time.

Flags and the `krab server` subcommands are listed in
[CLI.md](../CLI.md#krab-server); the reasoning behind the state layout is in
[DESIGN.md](../DESIGN.md#server-krab-server).

## Requirements

### Starting and reaching a daemon

```
SRV-1  A command that reads the inventory and has a connector MUST use the
       daemon for this inventory and build, starting a detached one when none
       answers on the socket.
       Test: none
       Since: b3c122e

SRV-2  `--no-daemon`, or `KRAB_NO_DAEMON` set to any value other than
       `0`, `n`, `no`, `f`, `false`, `off` or empty, MUST make every command
       render in-process without connecting to or starting a daemon.
       Test: none
       Since: b3c122e

SRV-3  `--raw` MUST imply local rendering, as `--no-daemon` does.
       Test: none
       Since: 58f8ddd

SRV-4  When the daemon cannot be reached or started, a command that has a
       local path MUST render locally and log a warning that names the socket.
       `krab inventory watch`, `krab server start` and `krab lsp` have no
       local path and MUST fail instead.
       Test: none
       Since: b3c122e

SRV-5  The daemon path and the local path MUST give identical results for
       the same inventory and command.
       Test: none
       Since: b3c122e

SRV-6  The client MUST start the daemon as `<current exe> server run
       --inventory-path <root> --idle-timeout 1800` in a new session
       (`setsid`), with stdin from /dev/null and stdout and stderr appended
       to the log file (SRV-17), creating the log directory if needed.
       Test: none
       Since: b3c122e

SRV-7  After starting a daemon the client MUST wait up to 10 s, polling every
       25 ms, for the socket to accept a connection. On timeout it MUST fail
       with "server did not start; see <log>" followed by the last 5 lines
       of the log.
       Test: none
       Since: #86

SRV-8  The client MUST reap the daemon process it started and MUST stop
       waiting as soon as that process has exited.
       Test: none
       Since: b3c122e

SRV-9  After starting a daemon the client MUST call `server.info` and refuse
       the connection when the reported version or protocol differs from its
       own, naming the socket, version, pid and binary of the server that
       answered.
       Test: none
       Since: #86

SRV-10 When a server already answering on the socket reports a different
       version or protocol, the client MUST send it `server.shutdown`, wait
       up to 3 s for the socket to stop accepting, and then start a fresh one.
       Test: none
       Since: b3c122e
```

### Socket, log and build identity

```
SRV-11 The socket MUST be `<runtime dir>/<inventory>-<build>.sock`, where
       `<inventory>` is the first 16 hex digits of the BLAKE3 hash of the
       canonical inventory path (the path as given when it cannot be
       canonicalised) and `<build>` the first 8 hex digits of the BLAKE3 hash
       of the build identity (SRV-13).
       Test: none
       Since: #86

SRV-12 The runtime directory MUST be `$XDG_RUNTIME_DIR/krab` when
       XDG_RUNTIME_DIR is set and not empty, else `/tmp/krab-<uid>`.
       Test: none
       Since: b3c122e

SRV-13 The build identity MUST be `<crate version>+<size>-<mtime>`, where
       size and mtime (Unix seconds) are those of the running executable, so
       a rebuilt binary gets a new socket.
       Test: none
       Since: 58f8ddd

SRV-14 The socket and the directory holding it MUST be usable only by the
       user who owns the daemon.
       Test: none
       Since: b3c122e

SRV-15 When the socket path exceeds the platform limit for unix socket
       addresses, the client MUST fail at once with a message naming the
       runtime directory, or use a shorter path, instead of waiting for a
       daemon that cannot bind.
       Test: none
       Since: #86

SRV-16 A starting daemon MUST create the runtime directory if missing and
       bind its socket only when no live server answers on it. A live socket
       MUST make the start fail with `AddrInUse` ("a server is already
       running") and the process exit. A socket file nobody answers on MUST
       be removed and replaced, and so MUST dead sockets that other builds
       left for the same inventory.
       Test: none
       Since: #86

SRV-17 The log file MUST be `$XDG_STATE_HOME/krab/server-<inventory>.log`
       when XDG_STATE_HOME is set and not empty, else
       `$HOME/.local/state/krab/server-<inventory>.log` (`/tmp` when HOME is
       unset). Every build's daemon for the inventory appends to it.
       Test: none
       Since: b3c122e
```

### Start-up

```
SRV-18 The daemon MUST bind its socket before the initial render. During
       the render, `server.*` requests MUST be answered at once (with
       `ready: false` in `server.info`), and every other request MUST be held
       until the render is done and then answered.
       Test: none
       Since: #86
```

### Protocol

```
SRV-19 The protocol MUST be JSON-RPC 2.0 with one JSON document per line in
       each direction. A connection is answered in request order; separate
       connections are served concurrently, one thread each.
       Test: none
       Since: b3c122e

SRV-20 Errors MUST use these codes: -32700 for a line that is not a request
       (answered with id 0), -32601 for an unknown method, -32602 for
       parameters that do not deserialise, and 1 for inventory failures, whose
       `data` is an array of diagnostics.
       Test: none
       Since: b3c122e

SRV-21 `server.info` MUST report protocol version 2.
       Test: none
       Since: #86

SRV-22 `inventory.target`, `inventory.classes` and `inventory.explain` for a
       target whose render failed MUST return that target's diagnostic; for
       an unknown name they MUST return `inventory::unknown_target`. A `path`
       on `inventory.target` that does not exist MUST return
       `inventory::pattern_not_found`.
       Test: none
       Since: b3c122e

SRV-23 `inventory.targets` and `inventory.all` given `labels` MUST return
       only targets whose `parameters.kapitan.labels` contain every given
       pair; without `labels` (or with null params) they return all targets.
       Test: none
       Since: 000035c
```

### Generations and long polls

```
SRV-24 The generation MUST start at 0 and increase by exactly 1 after each
       completed render pass: the initial render and every batch of file
       changes, including a batch that re-rendered no target. Every result
       that carries a generation MUST report the generation its data belongs
       to.
       Test: none
       Since: b3c122e

SRV-25 Each render pass MUST append a change summary (generation, time,
       changed files, re-rendered targets, duration, errors) to a history
       that keeps the latest 200.
       Test: none
       Since: b3c122e

SRV-26 `inventory.wait` MUST return as soon as the generation exceeds the
       given one, with `timed_out: false` and the history entries newer than
       the given generation. Otherwise it MUST return after `timeout_ms`
       (default 30000, capped at 120000) with the current generation,
       `timed_out: true` and no changes.
       Test: none
       Since: b3c122e

SRV-27 The client's read timeout (150 s) MUST exceed the longest
       `inventory.wait` the server allows (120 s).
       Test: none
       Since: b3c122e
```

### Watching and re-rendering

```
SRV-28 The daemon MUST watch the inventory directory recursively with
       events debounced 150 ms, starting after the initial render. Access
       events MUST be ignored, and so MUST events on existing regular files
       whose extension is not `yml` or `yaml` unless the file is a
       configuration source (SRV-33, SRV-34). Directories and paths that no
       longer exist MUST be passed on.
       Test: none
       Since: b3c122e

SRV-29 A batch of changed paths MUST re-render exactly: the targets indexed
       under a changed path; for a changed directory or vanished path, every
       target indexed under a path below it; every target whose last render
       failed; and every target file discovered since the last pass. Targets
       whose files are gone MUST leave the rendered set and the error set.
       Before re-rendering, the caches that depend on the changed paths (and
       on everything below a changed directory) MUST be dropped.
       Test: none
       Since: b3c122e

SRV-30 The index MUST map every file a target was rendered from, and every
       path probed while resolving its class names, to that target, so a new
       file that shadows a class (`classes/common/init.yml` next to
       `classes/common.yml`) re-renders exactly the targets that include it.
       Test: crates/krab-inventory/tests/fixture.rs::single_target_render_touches_only_its_closure
       Since: b3c122e

SRV-31 For an inventory file that is a symlink, the daemon MUST map its real
       location back to the inventory path and, within 500 ms of learning
       about it, watch the real directory (non-recursively) when it lies
       outside the inventory.
       Test: none
       Since: b3c122e

SRV-32 Every change on disk under the watched directories MUST eventually
       show in the daemon's answers without a restart. This includes saves
       by rename and `git checkout` of a file, watcher errors, and event
       queue overflow, after which the daemon must re-read what it may have
       missed.
       Test: none
       Since: b3c122e
```

### Restart on configuration change

```
SRV-33 A change to the `.kapitan` the daemon was configured from MUST make
       it stop: it stops accepting connections within one accept poll
       (50 ms), removes its socket and exits, and the next client request
       starts a fresh daemon with the new configuration.
       Test: crates/krab-server/tests/restart.rs::a_changed_configuration_source_stops_the_server
       Since: #155

SRV-34 A change to the `resolvers.py` the daemon loaded, or to a project
       module it imported, MUST stop the daemon in the same way as SRV-33.
       Test: none
       Since: #77

SRV-35 A change to an inventory file MUST re-render in place and MUST NOT
       stop the daemon.
       Test: crates/krab-server/tests/restart.rs::a_changed_configuration_source_stops_the_server
       Since: b3c122e

SRV-36 The directory of each configuration source that lies outside the
       inventory MUST be watched non-recursively; a directory that cannot be
       watched is logged at warn and skipped.
       Test: none
       Since: #77

SRV-37 After writing a response, a connection MUST be closed when the
       daemon is shutting down or stopping for a configuration change.
       Test: none
       Since: #77
```

### Lifetime

```
SRV-38 The daemon MUST exit once no request has arrived for the idle
       timeout (default 1800 s). Receiving a request line and writing its
       response both reset the idle clock.
       Test: none
       Since: b3c122e

SRV-39 `server.shutdown` MUST answer `true`, after which the daemon stops
       accepting connections, removes its socket and exits.
       Test: none
       Since: b3c122e

SRV-40 `krab server stop` and `krab server status` MUST act on every live
       daemon for the inventory, whichever build started it. `status --json`
       prints a list of `server.info` results.
       Test: none
       Since: #86

SRV-41 `krab server start` MUST start a daemon only when none of this build
       answers, and print the `server.info` of the one it reached.
       Test: none
       Since: b3c122e
```

### Logging

```
SRV-42 Log output MUST go to stderr through `tracing`, filtered by RUST_LOG,
       defaulting to `info` for `krab server run` and `warn` for every other
       command. A spawned daemon's stderr is the log file (SRV-6).
       Test: none
       Since: b3c122e

SRV-43 The daemon SHOULD log at info: the socket it listens on, the start of
       the initial render, the initial render's target and error counts and
       duration, each re-render (generation, files, targets, errors,
       duration), a configuration source change, and an idle exit.
       Test: none
       Since: #86

SRV-44 Every daemon log line MUST identify the process and build that wrote
       it, and a fatal error MUST be logged as an `error` event.
       Test: none
       Since: b3c122e

SRV-45 `krab server logs` MUST print the last N lines of the log file
       (`-n`, default 50).
       Test: none
       Since: b3c122e
```

### Secrets

```
SRV-46 The daemon MUST NOT reveal refs. It holds and serves the reference
       tags only; revealing happens in the client.
       Test: none
       Since: b3c122e
```

## Acceptance criteria

SRV-AC-1. First call starts a daemon (SRV-1, SRV-6, SRV-7, SRV-9, SRV-18).
Given no daemon for the inventory and build, when `krab inventory targets`
runs, then a detached `krab server run` binds the socket, the CLI's request
waits for the initial render, and the CLI prints the targets from the daemon.
Test: none (verified by hand in #86).

SRV-AC-2. Concurrent starters (SRV-16, SRV-9). Given no daemon, when two
clients start at once and each spawns one, then a daemon that finds a live
socket fails at bind with "a server is already running" and exits, exactly one
daemon survives, and both clients get their answer from it. Test: none
(verified by hand in #86).

SRV-AC-3. Two builds (SRV-11, SRV-13, SRV-40). Given the shell's krab and a
development build pointed at the same inventory, when both run commands, then
each talks to its own daemon on its own socket, neither stops the other, and
`krab server status` from either lists both. Test: none.

SRV-AC-4. Configuration edit (SRV-33, SRV-35). Given a running daemon, when
an inventory file changes, then the daemon re-renders in place; when
`.kapitan` changes, then the daemon stops and the next request is answered by
a new process with the new settings. Test:
crates/krab-server/tests/restart.rs::a_changed_configuration_source_stops_the_server
covers the stop flag; the respawn was verified by hand in #155.

SRV-AC-5. Watch a change (SRV-24, SRV-26, SRV-29). Given `krab inventory
watch` connected at generation N, when a class file is saved, then the wait
returns generation N+1 with a change listing that file and the targets
re-rendered from it. Test: none.

SRV-AC-6. Shadowing class file (SRV-29, SRV-30). Given targets that include
`common` resolved from `classes/common.yml`, when `classes/common/init.yml`
is created, then exactly those targets re-render. Test:
crates/krab-inventory/tests/fixture.rs::single_target_render_touches_only_its_closure
covers the recorded probe, not the daemon's re-render.

## Edge cases

SRV-EC-1. A daemon loses the start race (SRV-AC-2): its "a server is already
running" line lands in the shared log with nothing to tell it apart from the
winner's lines (#139).

SRV-EC-2. The spawned daemon dies during start-up (for example the socket
path is too long, #170): the client still waits the full 10 s, then
reports the log tail, and the command renders locally (SRV-4). The dead child
is not reaped (open deviation #169).

SRV-EC-3. Target discovery fails while the daemon runs (the targets directory
is removed, or two target files map to one name without
`compose-target-name`): `discover_targets` errors are replaced by an empty
target list (`state.rs:123`, `state.rs:142`), so the daemon serves no targets
and records no error, while the local path reports the error. Found by
reading the code; not filed.

SRV-EC-4. A file changes during the initial render: the watcher starts only
after the render (`lib.rs:48-57`), so a change made after the file was read
is not seen until the next event on a relevant path.

SRV-EC-5. A watcher error arrives: the whole debounced batch is dropped with a
warn line and no rescan follows (open deviation #136).

SRV-EC-6. A render error without a target name is reported in the change
summary of its pass but not by `inventory.diagnostics`, which lists errors per
target.

SRV-EC-7. A client more than 200 generations behind gets only the 200 newest
changes from `inventory.wait`.

SRV-EC-8. A configuration source changes while requests are in flight: the
batch that carried the change still re-renders with the old configuration,
answers in flight complete with it, a pending `inventory.wait` returns that
generation, and the connection is then closed (SRV-37).

SRV-EC-9. A long-polling client (the language server, `inventory watch`)
resets the idle clock with every wait, so the daemon does not idle out while
one is connected.

SRV-EC-10. A rebuilt binary gets a new socket; the previous build's daemon
keeps running until its idle timeout and `krab server status` lists both.

SRV-EC-11. `${oc.env:...}` resolves in the daemon's environment, which it
inherited from the process that started it. Changing a variable needs
`krab server stop` (documented in CLI.md).

SRV-EC-12. `krab server run` in the foreground logs to the terminal, yet
`server.info` still reports the log file path.

## Interfaces

Socket and log paths: SRV-11, SRV-12, SRV-17. Environment read by the daemon
and client: XDG_RUNTIME_DIR, XDG_STATE_HOME, HOME, RUST_LOG, KRAB_NO_DAEMON
(and the deprecated KAPITAN_NO_DAEMON, see [CLI.md](../CLI.md)).

The wire types are in `crates/krab-server/src/protocol.rs`; protocol version 2.

| Method | Params | Result |
|---|---|---|
| `server.info` | none | `InfoResult`: version, protocol, pid, exe, ready, resolvers, inventory_path, socket, log, generation, targets, errors, uptime_secs, idle_timeout_secs, last_change |
| `server.shutdown` | none | `true` |
| `inventory.targets` | optional `{labels: [[k, v], ...]}` | `{generation, targets: [TargetSummary]}` |
| `inventory.target` | `{name, path?}` | `{name, digest, generation, document, warnings}` |
| `inventory.all` | optional `{labels}` | `{generation, documents: {name: document}, errors}` |
| `inventory.classes` | `{name}` | class names of the target |
| `inventory.explain` | `{target, path}` | `Explanation` from `krab-inventory` |
| `inventory.deps` | `{files: [path]}` | sorted names of targets rendered from any of the files |
| `inventory.diagnostics` | none | `{generation, errors, warnings}` |
| `inventory.class_usage` | none | `ClassUsage` list from `krab-inventory` |
| `inventory.wait` | `{generation, timeout_ms?}` | `{generation, timed_out, changes: [ChangeSummary]}` |

`inventory.class_usage` is served but is missing from the method list in
`protocol.rs` and CLI.md.

A `ChangeSummary` is `{generation, at (Unix ms), changed_files,
rerendered, duration_ms, errors}`. A `TargetSummary` is `{name, path, file,
digest, doc_digest, ok, error?, labels, classes (count), inputs}`.

## Out of scope

| Item | Reason | Issue |
|---|---|---|
| TCP or HTTP transport, OpenAPI description | The daemon serves local clients of one user over a unix socket | #21, #40 |
| One daemon serving several inventories | One daemon per inventory and build keeps state and restarts independent | #22 |

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| SRV-26 | #135 | A generation bump can be missed. `wait_for` checks the generation under the `inner` read lock and then waits on a condvar under the separate `changed_lock` (`state.rs:276-297`); `rerender` bumps under the write lock and notifies afterwards (`state.rs:256-271`). A waiter between the two sleeps until its timeout, 60 s for both `inventory watch` and the language server. |
| SRV-32 | #136 | Watcher errors drop the batch at warn (`watch.rs:27-31`) and an overflow, which carries no paths, is ignored; nothing forces a rescan, so a missed event leaves the render stale until restart. |
| SRV-32 | #59 | Unverified. A daemon kept serving a class file's old content after the file was reverted with `git checkout` in a git worktree; its log showed a watch on a symlink target directory and no event after the initial render. Not reproduced in a plain repository or a symlinked directory; the worktree layout is unverified. #136 is a candidate cause. |
| SRV-44 | #139 | Log lines carry no pid or build, and every build appends to one file (`paths.rs:68-70`). Fatal errors (bind failure, watcher start failure, a panicking accept loop) return to `main` and reach the log as a bare `error:` line from `eprintln!`, bypassing `tracing`. |
| SRV-14 | #150 | Without XDG_RUNTIME_DIR, `/tmp/krab-<uid>` and the socket take their mode from the umask (`rpc.rs:37`, no chmod after bind) and no peer credential check exists. On Linux a connect needs write permission on the socket (unix(7)), so under umask 022 other users cannot connect, but under umask 002 the owner's group can, and systems that ignore socket permissions (unix(7) names older BSDs) protect nothing. |
| SRV-8 | #169 | The spawned `Child` is dropped (`client.rs:206`), so a daemon that exits while the spawning process lives becomes a zombie, and the client polls only the socket (`client.rs:145-149`), waiting the full 10 s after the child has died. |
| SRV-15 | #170 | `socket_path` (`paths.rs:38-41`) does not check the length; with a long XDG_RUNTIME_DIR the bind fails with "path must be shorter than SUN_LEN" and every command waits 10 s before rendering locally. |
| SRV-5 | #134 | No test compares the daemon and `--no-daemon` paths; every CLI integration test runs with `--no-daemon`. |