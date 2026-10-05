# Dependency fetching

Status: As-built
Verified against: main @ a4fb9c4
Code: `crates/krab-compile/src/fetch.rs`, `crates/krab-compile/src/oci.rs`, `fetch_dependencies` in `crates/krab-compile/src/engine.rs`, the fetch options in `crates/krab/src/cmd_compile.rs`

## Problem

Targets declare external inputs in `parameters.kapitan.dependencies`: git
repositories, files or archives over http(s), helm charts and OCI artifacts.
Each item names a `source` and an `output_path`, and the compile inputs then
read the files from there. kapitan fetches them in its `dependency_manager`;
krab does the same natively so a compile needs neither the Python kapitan nor
oras.

Fetching runs before staleness is decided. The fetched files are then
ordinary inputs: the inputs that read them record the reads and the manifest
tracks them, so a changed dependency recompiles exactly the targets that read
it.

Three constraints shape the design. Compiled output must match kapitan's, so
copy semantics, archive handling and OCI layer layout follow kapitan 0.36.3
and oras. A repository whose dependencies are all present must compile
offline, which is why an existing output path is not fetched again (D4 in
[DECISIONS.md](../DECISIONS.md)). And the sources are untrusted network
input, so OCI digests are verified (D12) and registry credentials are kept
off plain http and foreign token realms (D11).

## Requirements

### Declaration

```
FET-1  `parameters.kapitan.dependencies` MUST be a list or null. Each item
       MUST have `type` (`git`, `http`, `https`, `helm` or `oci`), `source`
       and `output_path`; a `helm` item MUST also have `chart_name`. A
       missing field or another type MUST fail naming the target and the
       item index.
       Test: crates/krab-compile/src/fetch.rs::parses_every_kind_and_normalises_paths
       Since: #62

FET-2  An item's destination MUST be `os.path.normpath(os.path.join(root,
       output_path))` with root the compile output path: an absolute
       `output_path` is kept and `..` is resolved lexically, also above
       root.
       Test: crates/krab-compile/src/fetch.rs::normalise_join_matches_normpath
       Since: #62

FET-3  Dependencies of the candidate targets MUST be fetched before
       staleness is decided, so the fetched files count as reads of the
       targets that use them.
       Test: none
       Since: #62
```

### Which items are fetched

```
FET-4  An item MUST be considered only when --fetch or --force-fetch is in
       effect or the item has `force_fetch: true`. --fetch and --force-fetch
       default to `compile.fetch` and `compile.force-fetch` from `.kapitan`;
       --no-fetch MUST override `compile.fetch`.
       Test: crates/krab-compile/src/fetch.rs::item_force_fetch_applies_without_fetch_flag
       Since: #62

FET-5  A considered item whose destination already exists (a dangling
       symbolic link counts) MUST NOT be fetched unless it is forced; it is
       reported as skipped with reason "already present" (D4).
       Test: crates/krab-compile/src/fetch.rs::git_dependency_checks_out_ref_and_copies_subdir
       Since: #62

FET-6  --force-fetch MUST fetch every item and overwrite existing files.
       `force_fetch: true` MUST do the same for its item, with or without
       --fetch (D5).
       Test: crates/krab-compile/src/fetch.rs::item_force_fetch_applies_without_fetch_flag
       Since: #62

FET-7  Items with the same `source` and destination, from any targets, MUST
       be fetched once and reported once. The remaining items MUST be grouped
       by source (git, http, oci) or by chart identity (helm: source,
       `chart_name`, `version`, `helm_path`); each group MUST fetch its
       source once and copy it to every destination, and groups MUST run in
       parallel up to --parallelism.
       Test: crates/krab-compile/src/fetch.rs::http_dependency_saves_or_unpacks
       Since: #62

FET-8  With --dry-run, items that would be fetched MUST be reported as
       `would_fetch` with their reason and nothing MUST be downloaded or
       written.
       Test: crates/krab-compile/src/fetch.rs::dry_run_reports_without_fetching
       Since: #62

FET-9  A failure MUST be reported for each affected destination; a failure
       of the shared step (clone, download, pull) MUST fail every destination
       of its group. Any failure MUST abort the compile with "N dependencies
       failed to fetch" after all groups finish.
       Test: crates/krab-compile/src/fetch.rs::git_dependency_checks_out_ref_and_copies_subdir
       Since: #62
```

### Copying into the destination

```
FET-10 An unforced copy MUST follow kapitan's `safe_copy_tree`: it never
       overwrites an existing file and skips entries whose name starts with
       `.`. A forced copy MUST follow `copy_tree(clobber_files=True)`: it
       replaces existing files. Dot entries in a forced copy follow
       FET-32.
       Test: crates/krab-compile/src/fetch.rs::safe_copy_never_overwrites_and_skips_dotfiles
       Since: #62

FET-11 Each run MUST fetch into a fresh directory under the system temporary
       directory and remove it when all groups are done.
       Test: none
       Since: #62
```

### git

```
FET-12 A `git` source MUST be cloned once per run with the `git` binary,
       with `GIT_TERMINAL_PROMPT=0` and the user's git configuration in
       effect. Per destination it MUST check out `ref`, else the remote's
       default branch; run `git submodule update --init` when
       `submodules: true`; and copy the repository or its `subdir`, which
       MUST exist.
       Test: crates/krab-compile/src/fetch.rs::git_dependency_checks_out_ref_and_copies_subdir
       Since: #62
```

### http and https

```
FET-13 An `http`/`https` source MUST be downloaded once per run with a
       30 second connect timeout. Without `unpack`, the file MUST be copied
       to the destination path, replacing an existing file only when
       forced.
       Test: crates/krab-compile/src/fetch.rs::http_dependency_saves_or_unpacks
       Since: #62

FET-14 With `unpack: true` the archive type MUST be chosen like kapitan's
       `unpack_downloaded_file`, from the Content-Type without parameters:
       `application/x-tar` is a tar (gzipped or not, by magic bytes);
       `application/zip` is a zip; `application/gzip`, `application/x-gzip`,
       `application/octet-stream`, `application/x-compressed` and
       `application/x-compressed-tar` are a gzipped tar only when the
       source name ends in `.tar.gz` or `.tgz`. Zip magic bytes MUST select
       zip when the type is missing or `application/octet-stream`. Any other
       case MUST fail with "Content-Type ... is not supported for unpack".
       Test: crates/krab-compile/src/fetch.rs::unpack_dispatches_on_content_type_and_magic
       Since: #62

FET-15 An unforced unpack MUST extract into a temporary directory and copy
       with FET-10's safe copy; a forced unpack MUST extract directly into
       the destination, overwriting.
       Test: crates/krab-compile/src/fetch.rs::http_dependency_saves_or_unpacks
       Since: #62

FET-16 Extracting a tar or zip archive MUST NOT create entries outside the
       extraction directory (absolute paths and `..` components are
       rejected by the `tar` and `zip` crates).
       Test: none
       Since: #62
```

### helm

```
FET-17 A `helm` item MUST run `helm pull --destination <tmp> --untar`, with
       `--version` when `version` is set, and either the `oci://` source as
       the chart reference or `--repo <source> <chart_name>`. The binary is
       `helm_path` when set, else `helm`. The pull MUST produce a directory
       named `chart_name`.
       Test: crates/krab-compile/src/fetch.rs::helm_dependency_pulls_once_and_copies
       Since: #62

FET-18 A chart with a `version` MUST be kept under
       `$XDG_CACHE_HOME/krab/charts/<hash of source>/<chart>-<version>` and
       MUST NOT be pulled again while that directory exists, unless forced.
       A chart without `version` MUST be pulled into the run's temporary
       directory only.
       Test: crates/krab-compile/src/fetch.rs::helm_dependency_pulls_once_and_copies
       Since: #62
```

### OCI

```
FET-19 An `oci` source MUST be a bare `registry/repository[:tag][@digest]`
       reference: a `https://`, `http://` or `oci://` prefix MUST fail, and
       `media_type` MUST contain `/`. The first component MUST be a host
       (contains `.` or `:`, or is `localhost`); the tag defaults to
       `latest`; `docker.io` maps to `registry-1.docker.io` with `library/`
       for single-segment repositories.
       Test: crates/krab-compile/src/oci.rs::references
       Since: #62

FET-20 The pull MUST follow the registry distribution API as oras does: each
       manifest layer (only those whose media type is listed, when any
       destination of the source sets `media_type`; the union otherwise) is
       written to the path in its `org.opencontainers.image.title`
       annotation, or to its digest with `:` replaced by `-`. Layers that
       are tar archives, gzipped or not, MUST then be extracted into the
       artifact root and removed, like kapitan's `_extract_tar_blobs`. The
       artifact or its `subpath` is copied to each destination.
       Test: crates/krab-compile/src/fetch.rs::oci_dependency_pulls_extracts_and_copies
       Since: #62

FET-21 A layer title or `subpath` that is absolute or leaves the artifact
       directory MUST fail; a `subpath` that does not exist MUST fail.
       Test: crates/krab-compile/src/oci.rs::challenges_and_paths
       Since: #62

FET-22 Destinations of one source that differ in `insecure` or `tls_verify`
       MUST all fail with "conflicting connection settings".
       Test: crates/krab-compile/src/fetch.rs::oci_dependency_pulls_extracts_and_copies
       Since: #62

FET-23 Without `subpath`, a destination that ends up holding only
       directories MUST get a warning suggesting `subpath`.
       Test: crates/krab-compile/src/fetch.rs::oci_dependency_pulls_extracts_and_copies
       Since: #62

FET-24 A digest reference MUST match the sha256 of the manifest bytes before
       they are parsed. An index MUST list exactly one manifest, and that
       manifest MUST match its index entry. Every layer MUST match its
       digest. A digest with an algorithm other than `sha256` MUST fail. A
       failed check MUST write nothing to the destination (D12).
       Test: crates/krab-compile/src/fetch.rs::oci_manifest_is_verified_against_its_digest
       Since: #178

FET-25 `insecure: true` MUST use plain http. `tls_verify` MUST be a boolean
       or the path of a CA bundle, which MUST hold at least one certificate.
       Test: none
       Since: #62

FET-26 Authentication MUST answer one `WWW-Authenticate` challenge per
       request. For `Bearer`, a token MUST be requested from the realm with
       the challenge's `service` and `scope`, sending `OCI_USERNAME` and
       `OCI_PASSWORD` as basic credentials when both are set, and read from
       `token` or `access_token`. For `Basic`, the credentials MUST be sent
       directly, or the pull MUST fail asking for them.
       Test: crates/krab-compile/src/fetch.rs::oci_dependency_pulls_extracts_and_copies
       Since: #62

FET-27 Credentials MUST NOT be sent over plain http: with `insecure: true`
       and credentials set, a registry that asks for authentication MUST
       fail before anything is sent (D11).
       Test: crates/krab-compile/src/oci.rs::credentials_are_not_sent_over_plain_http
       Since: #177

FET-28 A token realm on another origin than the registry MUST use https and
       MUST receive credentials only when `tls_verify` is not false.
       Anonymous token requests MAY go to any https realm and to a
       same-origin http realm (D11).
       Test: crates/krab-compile/src/oci.rs::token_realms_on_another_origin
       Since: #177
```

### Not met yet

These requirements describe the behaviour krab is meant to have. `main` does
not meet them yet; the open deviations below say what it does instead.

```
FET-29 HTTP downloads and registry requests MUST be bounded by a total
       timeout and a size cap, also for decompressed data.
       Test: none
       Since: not met yet (#145)

FET-30 Copying a fetched or extracted tree MUST NOT dereference symbolic
       links.
       Test: none
       Since: not met yet (#146)

FET-31 A `git` fetch SHOULD NOT transfer the repository's full history
       when a ref or subdir is all it needs.
       Test: none
       Since: not met yet (#64)

FET-32 A forced copy MUST skip entries whose name starts with `.`, as an
       unforced copy does, and MUST report the skipped names once.
       Test: none
       Since: not met yet (#217)
```

## Acceptance criteria

1. Given a local git repository with tag `v1` and items for `subdir: lib`,
   `ref: v1`, the whole repository, a duplicate and a missing subdir, when
   compile runs with --fetch, then each destination holds the right
   revision, the whole-repository copy has no `.git`, the duplicate is not
   reported and only the missing subdir fails. A later run leaves an edited
   destination alone, without --fetch considers nothing, and with
   --force-fetch overwrites it. Covers FET-5, FET-6, FET-9, FET-10, FET-12.
   Test: `crates/krab-compile/src/fetch.rs::git_dependency_checks_out_ref_and_copies_subdir`.
2. Given an item with `force_fetch: true` whose destination was edited and an
   unforced item, when compile runs without --fetch, then only the forced
   item is fetched and overwritten. Covers FET-4, FET-6. Test:
   `crates/krab-compile/src/fetch.rs::item_force_fetch_applies_without_fetch_flag`.
3. Given a server answering one request with a tar.gz, when two items unpack
   and save the same URL, then both destinations are written from one
   download. Covers FET-7, FET-13, FET-15. Test:
   `crates/krab-compile/src/fetch.rs::http_dependency_saves_or_unpacks`.
4. Given a stand-in `helm` that logs its calls, when two items share a
   versioned chart and a third uses an `oci://` chart, then helm runs twice,
   a later item for the cached version runs it not at all, and a forced item
   runs it again. Covers FET-7, FET-17, FET-18. Test:
   `crates/krab-compile/src/fetch.rs::helm_dependency_pulls_once_and_copies`.
5. Given a registry that answers `@<digest of GOOD>` with another manifest,
   an index whose child does not match, an index with two entries and a
   `sha512` layer, when each is fetched, then each fails with the expected
   and downloaded digests or the unsupported algorithm and writes nothing,
   while matching pins pull. Covers FET-24. Test:
   `crates/krab-compile/src/fetch.rs::oci_manifest_is_verified_against_its_digest`.

## Edge cases

- FET-EC-1 `git` not on PATH: every git item fails with "git binary not
  found. git must be present in the PATH to fetch git dependencies".
- FET-EC-2 A connection failure or HTTP error status on download: the group
  fails with "fetching unsuccessful" and the cause.
- FET-EC-3 `helm pull` succeeds without a directory named `chart_name`: the
  group fails asking whether `chart_name` is the chart's name.
- FET-EC-4 The temporary directory cannot be created: every item fails.
- FET-EC-5 An OCI index with several manifests: the pull fails listing each
  digest and platform and asks for a `@<digest>` pin.
- FET-EC-6 An OCI bearer challenge without realm, or a scheme other than
  Basic and Bearer: the pull fails naming the registry.
- FET-EC-7 A server that accepts the connection and then stalls: the compile
  waits without limit (FET-29, open).
- FET-EC-8 An archive or artifact containing symbolic links: the copy step
  follows them (FET-30, open).
- FET-EC-9 A run interrupted during a copy: the partly written destination
  exists, so the next unforced run skips it (FET-5) and --force-fetch is
  needed to repair it. The temporary directory stays behind.
- FET-EC-10 An `output_path` that is absolute or climbs out of the
  repository with `..`: the item is written there (FET-2).

## Interfaces

- Item fields per type are listed in [CLI.md](../CLI.md#krab-compile-alias-c);
  the parsed form is `fetch::Dependency`.
- `compile --json` reports each item in `fetched` (see
  [CLI.md](../CLI.md#json-output)); statuses are `fetched` (with `ms`),
  `would_fetch`, `skipped` and `failed` (with `error`), plus `reason` and
  `warnings`. `--explain` prints skipped items with their reason.
- Environment: `OCI_USERNAME`, `OCI_PASSWORD`, `XDG_CACHE_HOME` (helm chart
  cache), `HOME` when `XDG_CACHE_HOME` is unset.
- HTTP requests send `User-Agent: kapitan/<krab version>`.

## Out of scope

- `s3://` and `gs://` object storage sources (#63).
- Local path sources, `type: local` or `file://` (#66).
- Remote inventories, `parameters.kapitan.inventory` (#65).
- Platform selection for multi-manifest OCI indexes, and signature
  verification (left out of #178).

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| FET-29 | #145 | `download` (`fetch.rs:523`) and `Registry::new` (`oci.rs:254`) set only `timeout_connect(30s)`; bodies are read with `.limit(u64::MAX)` (`fetch.rs:543`, `oci.rs:294`); `unpack_file` and `tar_bytes` hold whole archives in memory. |
| FET-30 | #146 | `safe_copy_tree` and `copy_tree` (`fetch.rs:1009-1047`) use `is_dir` and `std::fs::copy`, which follow links, so a link entry in an archive, repository or artifact copies the target's contents (or a whole directory) into the destination. |
| FET-31 | #64 | `fetch_git` runs a full `git clone` per run. |
| FET-32 | #217 | A forced copy copies dot entries too (`fetch.rs:505-508,1019-1021`): a `type: git` dependency without `subdir` leaves `.git` in the dependency path, which `git add` then treats as an embedded repository. |
