# Agent guidance

Token Nomad is a Rust CLI whose public binary is `nomad`. Keep package metadata, the checked-in lockfile, and user-facing command documentation aligned with the implementation.

The supported built-in agents are Claude and Codex. The launch interface accepts `--agent claude|codex` and `--endpoint NAME` as temporary filters or overrides, while profile configuration continues to name an agent and endpoint explicitly.

Portable commands are `export DIR`, `import DIR`, and `sync DIR`; all require `--agent claude|codex`. Export selects one agent for a bundle, and import or sync must supply that bundle's agent. Export accepts repeatable `--profile ID` and `--skill DIR` selectors for skill directories containing `SKILL.md`. Import and sync accept `--skills-dir LOCAL`, repeatable item-specific `--replace kind:id` choices, and repeatable `--key-env ENDPOINT=ENV_NAME` bindings, then produce a plan. `ENV_NAME` is only an environment variable identifier, never its secret value. Planning does not write to agent targets, and sync never deletes omitted local records. `apply PLAN_ID` performs the reviewed plan, and `restore OP_ID` restores the operation journal entry.

The migration boundary contains portable typed profile fields and skill directories containing `SKILL.md` plus `.md` or `.txt` text files. Any script, binary, dependency, or mixed skill is rejected as a whole. API-key endpoints carry only the `key_env` identifier and never the secret value. The conservative secret scan catches likely credentials but cannot guarantee a bundle is secret-free, so review it before sharing. Plugins, hooks, credentials, opaque agent state, and Git synchronization stay outside this boundary.

Integrations support only `shell`, `claude-statusline`, `claude-settings`, and `codex-statusline`. They use safe field or block updates and an operation journal. Sources and targets resolve relative to the configuration file; existing targets and all sources must be bounded regular paths without symlink or traversal components, while missing targets are allowed. Statusline rendering must preserve unrelated settings, sanitize terminal display data, and avoid raw snapshots. The shell renderer depends only on Bash and `jq` and reads fixture payloads from standard input.

Tests use synthetic fixtures from a temporary cwd under isolated temporary `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, `CODEX_HOME`, and `CLAUDE_CONFIG_DIR` directories so ancestor `.codex` discovery cannot reach a developer checkout. Never read or print a developer's actual configuration, credentials, cache, or agent state while testing.

Before handing off a change, run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and `cargo test --all-targets --all-features --locked`. Keep prose one paragraph or list item per line and write shared documents in English.
