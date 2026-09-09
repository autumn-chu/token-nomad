use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn invoke(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nomad"));
    command
        .env_clear()
        .current_dir(root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(
            "TERM",
            std::env::var_os("TERM").unwrap_or_else(|| "dumb".into()),
        )
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("CODEX_HOME", root.join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude-home"));
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ] {
        command.env_remove(key);
    }
    command.args(args).output().unwrap()
}

fn setup() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    assert!(invoke(root.path(), &["init"]).status.success());
    root
}

fn configure(root: &Path, text: &str) {
    fs::write(root.join("config/nomad/config.toml"), text).unwrap();
}

fn executable(path: &Path, output: &str) {
    fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' {output:?}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn legacy_pi_and_chat_completions_configuration_requires_migration_without_rewrite() {
    let root = setup();
    let legacy = "version=1\n[executables]\npi='pi'\n[endpoints.company]\nauth='api-key'\nprotocol='openai-chat-completions'\nbase_url='https://example.test'\nkey_env='TEST_KEY'\n[profiles.old]\nagent='pi'\nendpoint='company'\ncontext_window=100\nmax_tokens=50\n";
    configure(root.path(), legacy);
    let output = invoke(root.path(), &["list"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("removed Pi or chat-completions"));
    assert_eq!(
        fs::read_to_string(root.path().join("config/nomad/config.toml")).unwrap(),
        legacy
    );
}

#[test]
fn relative_executable_paths_resolve_from_the_configuration_file() {
    let root = setup();
    let bin = root.path().join("config/nomad/bin");
    fs::create_dir_all(&bin).unwrap();
    executable(&bin.join("codex"), "relative executable");
    let destination = root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    configure(
        root.path(),
        "version=1
[executables]
codex='bin/codex'
[endpoints.official]
auth='native'
[profiles.default]
agent='codex'
endpoint='official'
",
    );
    let output = invoke(
        root.path(),
        &["run", "default", "--cwd", destination.to_str().unwrap()],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "relative executable\n"
    );
}

#[test]
fn configuration_validation_rejects_unsupported_versions_and_profile_names() {
    let root = setup();
    configure(root.path(), "version=2\nendpoints={}\nprofiles={}\n");
    let output = invoke(root.path(), &["list"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unsupported configuration version"));

    configure(
        root.path(),
        "version=1\nendpoints={}\n[profiles.'invalid/name']\nagent='codex'\nendpoint='official'\n",
    );
    let output = invoke(root.path(), &["list"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Profile names"));
}

#[test]
fn selector_metadata_replaces_terminal_controls() {
    let root = setup();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let capture = root.path().join("fzf-input");
    let environment = root.path().join("fzf-environment");
    let arguments = root.path().join("fzf-arguments");
    let shell = root.path().join("fzf-shell");
    let fzf = bin.join("fzf");
    fs::write(
        &fzf,
        "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf '0.44.1\\n'; exit 0; fi\nprintf '%s' \"${FZF_DEFAULT_OPTS-unset}\" > \"$NOMAD_FZF_ENVIRONMENT\"\nprintf '%s\\n' \"$@\" > \"$NOMAD_FZF_ARGUMENTS\"\nprintf '%s' \"$SHELL\" > \"$NOMAD_FZF_SHELL\"\ncat > \"$NOMAD_CAPTURE\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o700)).unwrap();
    configure(
        root.path(),
        r#"version=1
selector='fzf'
[endpoints.official]
auth='native'
[profiles.default]
agent='codex'
endpoint='official'
model="\u001b[31mred\tvalue"
"#,
    );

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nomad"));
    command.args([
        "--config",
        root.path()
            .join("config/nomad/config.toml")
            .to_str()
            .unwrap(),
    ]);
    command.cwd(root.path());
    command.env("HOME", root.path());
    command.env("XDG_CONFIG_HOME", root.path().join("config"));
    command.env("XDG_STATE_HOME", root.path().join("state"));
    command.env("XDG_CACHE_HOME", root.path().join("cache"));
    command.env("CODEX_HOME", root.path().join("codex-home"));
    command.env("CLAUDE_CONFIG_DIR", root.path().join("claude-home"));
    command.env("NOMAD_CAPTURE", &capture);
    command.env("NOMAD_FZF_ENVIRONMENT", &environment);
    command.env("NOMAD_FZF_ARGUMENTS", &arguments);
    command.env("NOMAD_FZF_SHELL", &shell);
    command.env("FZF_DEFAULT_OPTS", "--bind=enter:execute(unsafe)");
    command.env("PATH", format!("{}:/bin:/usr/bin", bin.display()));
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ] {
        command.env_remove(key);
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let status = child.wait().unwrap();
    assert_eq!(status.exit_code(), 0);

    let selected_input = fs::read(capture).unwrap();
    assert_eq!(environment, root.path().join("fzf-environment"));
    assert_eq!(fs::read_to_string(environment).unwrap(), "unset");
    let arguments = fs::read_to_string(arguments).unwrap();
    assert!(!arguments.contains("--no-tty-default"));
    assert!(!arguments.contains("--with-shell"));
    assert_eq!(fs::read_to_string(shell).unwrap(), "/bin/sh");
    assert!(selected_input.ends_with(&[0]));
    assert!(!selected_input.contains(&b'\n'));
    assert_eq!(
        selected_input.iter().filter(|byte| **byte == b'\t').count(),
        1
    );
    let selected_text = String::from_utf8(selected_input).unwrap();
    assert!(!selected_text.contains('\u{1b}'));
    assert!(selected_text.contains("�[31mred�value"));
}

#[test]
fn fzf_0_74_uses_no_tty_default_without_newer_shell_flags() {
    let root = setup();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let arguments = root.path().join("fzf-arguments");
    let shell = root.path().join("fzf-shell");
    let fzf = bin.join("fzf");
    fs::write(
        &fzf,
        "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf '0.74.3\\n'; exit 0; fi\nprintf '%s\\n' \"$@\" > \"$NOMAD_FZF_ARGUMENTS\"\nprintf '%s' \"$SHELL\" > \"$NOMAD_FZF_SHELL\"\ncat > /dev/null\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o700)).unwrap();
    configure(
        root.path(),
        "version=1\nselector='fzf'\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n",
    );

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nomad"));
    command.args([
        "--config",
        root.path()
            .join("config/nomad/config.toml")
            .to_str()
            .unwrap(),
    ]);
    command.cwd(root.path());
    command.env("HOME", root.path());
    command.env("XDG_CONFIG_HOME", root.path().join("config"));
    command.env("XDG_STATE_HOME", root.path().join("state"));
    command.env("XDG_CACHE_HOME", root.path().join("cache"));
    command.env("CODEX_HOME", root.path().join("codex-home"));
    command.env("CLAUDE_CONFIG_DIR", root.path().join("claude-home"));
    command.env("NOMAD_FZF_ARGUMENTS", &arguments);
    command.env("NOMAD_FZF_SHELL", &shell);
    command.env("PATH", format!("{}:/bin:/usr/bin", bin.display()));
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ] {
        command.env_remove(key);
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    assert_eq!(child.wait().unwrap().exit_code(), 0);

    let arguments = fs::read_to_string(arguments).unwrap();
    assert!(arguments.contains("--no-tty-default"));
    assert!(!arguments.contains("--with-shell"));
    assert_eq!(fs::read_to_string(shell).unwrap(), "/bin/sh");
}

#[test]
fn selector_preview_uses_only_allowlisted_profile_metadata() {
    let root = setup();
    configure(
        root.path(),
        r#"version=1
[endpoints.company]
auth='api-key'
protocol='openai-responses'
base_url='https://private.example/v1'
key_env='SYNTHETIC_SECRET_SOURCE'
[profiles.'-preview']
agent='codex'
endpoint='company'
label='Safe label'
description='Safe description'
tags=['safe']
model='safe-model'
reasoning='high'
args=['--api-key','synthetic-secret-value']
"#,
    );
    let output = invoke(root.path(), &["__selector-preview", "--", "-preview"]);
    assert!(output.status.success(), "{output:?}");
    let preview = String::from_utf8(output.stdout).unwrap();
    assert!(preview.contains("Safe label"));
    assert!(preview.contains("Safe description"));
    assert!(preview.contains("Endpoint: company"));
    assert!(!preview.contains("synthetic-secret-value"));
    assert!(!preview.contains("SYNTHETIC_SECRET_SOURCE"));
    assert!(!preview.contains("private.example"));
}
