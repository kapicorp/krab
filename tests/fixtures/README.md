# Fixtures

`kadet/` is a two-target inventory snapshot with a component, a library
loaded through `load_from_search_paths` and a jinja2 template, exercising
the `kapitan` API krab bundles for kadet components
(`crates/krab-compile/tests/kadet_runner.rs`).

# Fixture inventory

A small inventory exercising the features the engine must reproduce exactly:
class resolution (init.yml, relative classes, diamonds), EXTEND_UNIQUE lists,
merge-time dereferencing, most shipped resolvers, YAML 1.1 scalars, kapitan
model normalisation, and PyYAML emitter quirks (folding, quoting, unicode).
The resolvers it does not call are listed in `UNCALLED_RESOLVERS` in
`crates/krab-inventory/tests/fixture.rs`.

`expected/<target>.yaml` is what kapitan 0.36.3 (omegaconf backend) prints for
`kapitan inventory -t <target>`. Regenerate it with the reference
implementation, from the repository root, as the parity job in
`.github/workflows/ci.yml` does:

```sh
python3.12 -m venv /tmp/kapitan-ref
/tmp/kapitan-ref/bin/pip install "kapitan[omegaconf]==0.36.3" "omegaconf==2.4.0.dev3"
/tmp/kapitan-ref/bin/python tests/fixtures/generate_expected.py
git diff tests/fixtures/expected
```

CI uses Python 3.12; `generate_expected.py` fails under Python 3.14 in
kapitan's multiprocessing. omegaconf is pinned because kapitan 0.36.3 accepts
any version from 2.4.0.dev3, and later ones dropped `ListMergeMode`, which
its omegaconf backend imports. With kapitan installed as a PEX instead:
`cd tests/fixtures && PEX_INTERPRETER=1 kapitan generate_expected.py`.

# Compile fixtures

`kadet-output/` and `helm/` are small repositories compiled by
`crates/krab/tests/kadet_output.rs` and `crates/krab/tests/helm_input.rs`.
Their expected output (`kadet-output-expected/<style>/cm.yaml`,
`helm-expected/compiled`) is `compiled/` from kapitan 0.36.3 run on a copy of
the fixture, with the same Python packages as above and, for helm, helm 3.17.
CI does not regenerate it. To check or refresh one:

```sh
cp -r tests/fixtures/helm /tmp/helm-ref && cd /tmp/helm-ref
/tmp/kapitan-ref/bin/kapitan compile
diff -r compiled "$OLDPWD/tests/fixtures/helm-expected/compiled"
```

For `kadet-output`, the `literal` output comes from the fixture's own
`.kapitan`; `double-quotes` and `folded` come from the `.kapitan` the
matching test in `kadet_output.rs` writes before compiling.

`kadet-output` is also the quickest repository to try krab on
(CONTRIBUTING.md, Build).
