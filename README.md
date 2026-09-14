# Token Nomad

Token Nomad is a Rust command-line tool for selecting Claude and Codex profiles, launching an agent with a named endpoint, and moving portable profile data between machines through reviewable plans.

The tool keeps portable data typed and explicit. It can carry profile metadata, endpoint references, and text skills while leaving credentials, plugins, hooks, provider-specific state, and Git history under the control of the destination machine.

## Requirements

- Rust 1.90 or newer with Cargo.
- Bash and `jq` for the Claude statusline renderer.
- A locally installed `claude` or `codex` executable for launching an agent.

The built-in selector is the default and needs no extra selector dependency. To opt into `selector = "fzf"`, install fzf with the package manager you use:

```sh
# macOS
brew install fzf

# Debian or Ubuntu
sudo apt-get install fzf
```

## Build and install locally

```sh
cargo build --release --locked
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/nomad "$HOME/.local/bin/nomad"
```

Token Nomad is developed and distributed from source in this repository; it does not publish a package or release from CI.

Installing the binary does not change agent settings, credentials, endpoints, or configuration files. `nomad init` creates its starter configuration only when no configuration file already exists.

## Quick start

Create a configuration with the native Claude and Codex defaults, inspect the available profiles, and preview a launch:

```sh
nomad init
nomad list --table
nomad doctor
```

Preview the deterministic fixture profile with a temporary agent or endpoint selection for one invocation:

```sh
nomad --config examples/config.toml --agent claude --endpoint official run review --dry-run
nomad --config examples/config.toml --agent codex --endpoint official run implementation --dry-run
```

The global `--config PATH` option selects an explicit configuration file. The `examples/config.toml` file is a safe fixture that contains no credentials or personal paths.

## Configuration

The configuration is TOML with versioned top-level data, named endpoints, named profiles, and optional selector settings. A profile chooses an agent and endpoint and may carry portable launch fields such as a model, reasoning mode, labels, and tags. Extra launch arguments remain local invocation data.

Native endpoints let Claude or Codex use their own local authentication. API-key endpoints name an environment variable; the secret value stays outside the configuration and repository. Unknown keys and unsupported versions are rejected so a typo cannot silently change a launch.

The starter configuration contains only Claude and Codex native profiles. Add another endpoint or profile explicitly when a local workflow requires it.

```toml
version = 1
selector = "builtin"

[endpoints.official]
auth = "native"

[profiles.review]
agent = "claude"
endpoint = "official"
label = "Review"
description = "Review the current change"
tags = ["review"]

[profiles.implementation]
agent = "codex"
endpoint = "official"
label = "Implementation"
description = "Implement the selected change"
tags = ["build"]

[keybindings.fzf]
accept = "enter"
cancel = "esc"
previous = "up"
next = "down"
toggle_preview = "ctrl-p"
preview_below = "ctrl-/"
preview_right = "alt-/"
choose_agent = "ctrl-l"
choose_endpoint = "ctrl-e"
```

The built-in selector has fixed controls: type to filter, `Up`/`Down` to move, `Enter` to launch, and `Esc` to cancel. `[keybindings.fzf]` applies only when `selector = "fzf"`. Its binding names are `accept`, `cancel`, `previous`, `next`, `toggle_preview`, `preview_below`, `preview_right`, `choose_agent`, and `choose_endpoint`. Their defaults are `enter`, `esc`, `up`, `down`, `ctrl-p`, `ctrl-/`, `alt-/`, `ctrl-l`, and `ctrl-e`, with `Ctrl-C` reserved for cancellation. Bindings may name only supported keys; shell actions and unmodified printable keys are rejected so ordinary typing remains fzf search.

## Portable export and migration

Export writes a bundle directory containing portable typed data and selected text skills. The destination must be fresh, and each `--skill DIR` points to a skill directory with `SKILL.md`:

```sh
nomad --agent claude export ./bundle --profile review --skill ./skills/review --skill ./skills/checklist
```

Review a bundle locally before moving it to another machine. The receiver creates a plan and prints its identifier; applying that identifier is the separate write step:

```sh
nomad --config ./sender.toml --agent claude export ./bundle --profile review --skill ./skills/review
find ./bundle -type f -print | sort
sed -n '1,200p' ./bundle/profiles.toml
nomad --config ./receiver.toml --agent claude import ./bundle --skills-dir ./local-skills --replace profile:review
# Review the printed plan ID and changes, then apply that exact ID.
nomad apply PLAN_ID
# Restore a completed import only when needed.
nomad restore OP_ID
```

`--agent claude|codex` is mandatory for export, import, and sync. Export selects one agent for each bundle, and the agent supplied to import or sync must match that bundle. Use `--profile ID` and `--skill DIR` more than once when selecting multiple records, or export skills alone without a profile. `--skills-dir LOCAL` names the destination skill directory. A changed existing profile, endpoint, or skill is a separate conflict: add the specific `--replace profile:ID`, `--replace endpoint:ID`, or `--replace skill:ID` that you reviewed. `sync` has the same planning behavior as `import`; neither command deletes local records omitted from the bundle.

Repeat `--key-env ENDPOINT=ENV_NAME` to bind an imported API endpoint to a local environment variable name, for example `--key-env hosted=LOCAL_ANTHROPIC_API_KEY`. Pass the variable name only, never `NAME=value` and never a credential. The variable's value is not read into the bundle or plan.

The migration format carries portable profile fields and skill directories made of `.md` or `.txt` text files, including the required `SKILL.md`. Any script, binary, dependency, or mixed skill is rejected as a whole. Export uses conservative patterns to catch likely credentials, but that check cannot prove a bundle is secret-free; inspect it before sharing. For API-key endpoints, a plan may carry the `key_env` identifier but never reads or copies the secret value. Plugins, hooks, opaque agent state, credentials, and Git synchronization are outside the migration boundary.

## Integrations and statusline

Integrations use safe field or block updates and keep an operation journal for explicit restore. Targets and optional sources resolve relative to the configuration file. Existing targets and all sources must be bounded regular files without symlink or traversal components; missing targets are allowed. These are the supported component names and configuration forms:

```toml
[integrations.shell]
target = "/absolute/path/to/.zshrc"

[integrations.claude-statusline]
target = "/absolute/path/to/bin/nomad-claude-statusline"
# source = "/absolute/path/to/approved-renderer.sh" # optional; otherwise use the bundled renderer

[integrations.claude-settings]
target = "/absolute/path/to/.claude/settings.json"
source = "/absolute/path/to/bin/nomad-claude-statusline"
# expected_command = "bash /absolute/path/to/old-renderer.sh" # required only to replace that exact existing command

[integrations.codex-statusline]
target = "/absolute/path/to/.codex/config.toml"
```

`shell` manages only Nomad's marked shell block. `claude-statusline` installs the embedded renderer or the approved source and refuses a differing existing target. `claude-settings` manages only `statusLine.type` and `statusLine.command`; an existing command requires the exact reviewed `expected_command` and must be a safe `bash` path. `codex-statusline` manages only `tui.status_line` and refuses a conflicting existing array. Other settings remain in place.

Plan, apply, and restore an integration with its component name. `apply` requires the unchanged plan; it prints the restore operation ID on success:

```sh
nomad --config ./integration.toml integrate plan claude-settings
nomad --config ./integration.toml integrate apply claude-settings
nomad integrate restore OP_ID
```

The renderer reads one JSON document from standard input, uses a single `jq` parse, sanitizes display fields, and keeps raw paths available only for filesystem operations. Keep targets inside an isolated test home when validating an integration.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for worktree, testing, and pull request conventions and [SECURITY.md](SECURITY.md) for vulnerability handling.

## License

Token Nomad is available under the [MIT License](LICENSE).
