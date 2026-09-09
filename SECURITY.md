# Security policy

Token Nomad handles agent configuration, endpoint references, text skills, and integration targets. Treat every imported bundle, configuration file, and statusline payload as untrusted input.

The security boundary keeps credentials in environment variables or the agent's native credential store. Export and migration plans carry portable typed fields and skill directories containing `SKILL.md` plus `.md` or `.txt` text files; any script, binary, dependency, or mixed skill is rejected as a whole. API-key endpoints carry only the `key_env` identifier and never the secret value. Plans do not copy credential values, plugins, hooks, opaque agent state, or Git history. Import and sync create plans without writing to agent targets, and apply is the explicit write step. The export scanner uses conservative patterns to reject likely credentials; it cannot guarantee that a bundle contains no secret, so review a bundle before sharing it.

Integration writes use atomic replacement and an operation journal so a target can be restored after a successful application. The journal retains managed field or block metadata and hashes, never raw native configuration or shell contents. Existing unrelated fields remain preserved, and a target that changed after planning or application is treated as a conflict requiring review.

The bundled statusline renderer accepts one JSON document on standard input, parses it once with `jq`, sanitizes terminal control sequences in display fields, and uses raw paths only for filesystem lookups. Test fixtures run from a temporary cwd with isolated `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, `CODEX_HOME`, and `CLAUDE_CONFIG_DIR` values.

## Reporting a vulnerability

Before any public release process, maintainers must enable GitHub's private vulnerability reporting. Until it is enabled, share reports only through a private collaborator advisory workflow or another private channel already established by the maintainers; do not open a public issue for a vulnerability. Include a concise impact description, affected revision, reproduction steps that use synthetic data, and a suggested mitigation when available.

Do not include credentials, private configuration contents, personal paths, or live endpoint data in an issue, pull request, log, or reproduction bundle. Redact those values before sharing any diagnostic artifact.

Security fixes are reviewed, tested, and coordinated before any public release process is considered.
