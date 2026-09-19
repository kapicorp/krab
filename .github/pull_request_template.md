## Summary

<!-- One or two sentences describing what this PR does. -->

Fixes #

## Motivation

<!-- Why is this change needed? What problem does it solve? -->

## Test plan

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --locked`
- [ ] `cargo test --locked`
- [ ] Fixture case added or updated, with the expected output regenerated from the reference (engine changes only)
- [ ] Checked against a real inventory: `diff` of `kapitan inventory -t …` and `krab inventory -t …` is empty, and `krab compile --force` leaves `git status compiled` clean (loading, merging, interpolation, resolver, emitter or compile changes only)

## Risk assessment

<!-- What could go wrong, and how is this reverted? -->

## Left out on purpose

<!-- What is deliberately not included, so the PR stays reviewable. -->
