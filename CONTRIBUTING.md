# Contributing to Token Nomad

Contributions should preserve the portable data boundary, keep agent targets untouched during planning, and leave credentials outside files and test fixtures.

## Development environment

Use Rust 1.90 and keep `Cargo.lock` synchronized with `Cargo.toml`. The built-in selector needs no additional selector dependency; install `fzf` only when developing or testing the optional fzf selector. The complete suite also requires Bash and `jq` for the statusline renderer. The repository uses a worktree per task so the primary checkout stays clean:

```sh
git worktree add ../token-nomad-worktrees/<task> -b feat/<task> main
cd ../token-nomad-worktrees/<task>
```

## Verification

Run the formatter, linter, and tests before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
```

Shell renderer tests invoke the checked-in `assets/claude-statusline.sh` with fixture JSON and require `jq`. Tests that inspect configuration, state, caches, or integrations set `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, `CODEX_HOME`, and `CLAUDE_CONFIG_DIR` to isolated temporary directories and run from a temporary cwd so ancestor `.codex` discovery cannot reach a developer checkout.

Use fixture values for endpoint names, environment variable names, and paths. Keep account credentials and personal configuration out of the repository and out of test output.

## Scope and compatibility

Portable fields remain typed and explicit. A selected skill directory contains `SKILL.md` and `.md` or `.txt` text files; any script, binary, dependency, or mixed skill is rejected as a whole. `export`, `import`, and `sync` require `--agent claude|codex`; export selects one agent, and import or sync must match the bundle's agent. Planning commands produce a reviewable plan without writing to an agent target, and application is a separate action identified by a plan ID. Resolve each changed profile, endpoint, or skill with its own reviewed `--replace kind:id`; sync never deletes local records omitted from the bundle. Restore uses the operation ID recorded by the journal. An API-key endpoint carries only its `key_env` identifier and never its secret value; imports bind it through an explicit `--key-env ENDPOINT=ENV_NAME` option whose right side is an environment variable name, never a value.

Export rejects likely credentials with conservative patterns, not a proof that selected text has no secret. Review bundle contents before sharing them.

Changes that migrate plugins, hooks, opaque agent state, credentials, or Git history require a separate design decision. Do not add them as implicit fallbacks to export, import, or sync.

## Pull requests

Use a focused branch and a conventional commit subject such as `feat(export): add profile bundle selection` or `fix(statusline): preserve unrelated fields`. The pull request description should identify the user-visible contract, explain any compatibility impact, and include the exact verification commands that passed.

Keep generated build output, local configuration, caches, and secrets untracked. Update the README or examples when a user-facing command or configuration field changes.
