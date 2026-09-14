## What changed

<!-- Describe the user-visible behavior in one or two sentences. -->

## Why

<!-- Explain the problem or requirement this change addresses. -->

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --all-targets --all-features --locked`
- [ ] Shell renderer checks run with `jq` when statusline behavior changed

## Security and portability

- [ ] No credentials, personal paths, or live configuration values are included
- [ ] Portable fields remain typed and migration planning performs no agent-target writes
- [ ] User-facing commands or configuration changes are documented

## Review notes

<!-- Add compatibility notes, migration concerns, or follow-up work that reviewers need. -->
