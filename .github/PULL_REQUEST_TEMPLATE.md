# Pull request

## Purpose

Describe the user or contributor problem and the outcome of this change.

## Validation

List the exact commands and environments used to validate the change.

## Checklist

- [ ] The change is focused and contains no unrelated cleanup.
- [ ] Tests cover the changed behavior and important rejection paths.
- [ ] User-visible behavior is documented in English.
- [ ] User-visible changes are recorded under `Unreleased` in `CHANGELOG.md`.
- [ ] Diagnostics remain structured, deterministic, and free of secrets.
- [ ] Performance claims compare base and candidate on the same host.
- [ ] `cargo fmt`, strict `clippy`, and relevant tests pass with locked dependencies.
