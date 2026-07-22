## Summary

Describe the problem and the outcome of this change.

## Scope

- Affected compositor backend, application, crate, service, or documentation:
- User-visible or compatibility impact:
- Work intentionally left for follow-up:

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --workspace`
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `cargo test --workspace`
- [ ] `./scripts/check-markdown-links.sh`
- [ ] Relevant manual testing is described below.

Manual test environment and results:

## Safety and documentation

- [ ] I considered IPC, permissions, process launching, input, capture, file access, and automation implications where relevant.
- [ ] I updated user-facing documentation or explained why no documentation change is needed.
- [ ] Logs and screenshots are free of credentials and private data.
