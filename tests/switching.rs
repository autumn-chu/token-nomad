use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn run(root: &Path, arguments: &[&str]) -> Output {
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
    command.args(arguments).output().unwrap()
}

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::write(&agent, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir_all(root.path().join("config/nomad")).unwrap();
    fs::write(
        root.path().join("config/nomad/config.toml"),
        format!(
            "version=1\n[executables]\nclaude={agent:?}\ncodex={agent:?}\n[endpoints.official]\nauth='native'\n[endpoints.claude-api]\nauth='api-key'\nprotocol='anthropic-messages'\nbase_url='https://claude.example/v1'\nkey_env='TEST_CLAUDE_KEY'\n[endpoints.codex-api]\nauth='api-key'\nprotocol='openai-responses'\nbase_url='https://codex.example/v1'\nkey_env='TEST_CODEX_KEY'\n[profiles.claude]\nagent='claude'\nendpoint='official'\n[profiles.codex]\nagent='codex'\nendpoint='official'\n"
        ),
    )
    .unwrap();
    root
}

#[test]
fn explicit_agent_filters_lists_and_rejects_a_different_run_profile() {
    let root = fixture();
    let list = run(root.path(), &["--agent", "claude", "list"]);
    assert!(list.status.success());
    let list = String::from_utf8(list.stdout).unwrap();
    assert!(list.contains("claude"));
    assert!(!list.contains("codex\tcodex"));

    let output = run(
        root.path(),
        &["--agent", "claude", "run", "codex", "--dry-run"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match --agent"));
}

#[test]
fn endpoint_override_is_transient_and_validated_before_launch() {
    let root = fixture();
    let output = run(
        root.path(),
        &["--endpoint", "claude-api", "run", "codex", "--dry-run"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible"));
    let config = fs::read_to_string(root.path().join("config/nomad/config.toml")).unwrap();
    assert!(config.contains("endpoint='official'"));
}

#[test]
fn endpoint_urls_reject_embedded_credentials_without_echoing_them() {
    let root = fixture();
    let config_path = root.path().join("config/nomad/config.toml");
    let config = fs::read_to_string(&config_path).unwrap().replace(
        "https://claude.example/v1",
        "https://user:synthetic-password-123@example.invalid/v1",
    );
    fs::write(&config_path, config).unwrap();

    let output = run(
        root.path(),
        &["--endpoint", "claude-api", "run", "claude", "--dry-run"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("Endpoint URL"));
    assert!(!stderr.contains("synthetic-password-123"));
}

#[test]
fn plain_profile_list_replaces_terminal_control_sequences() {
    let root = fixture();
    let config_path = root.path().join("config/nomad/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str("[profiles.controls]\nagent='codex'\nendpoint=\"official\\u001b]0;synthetic-title\\u0007\"\nmodel=\"model\\u001b[31mred\"\nreasoning=\"high\\u001b[0m\"\n");
    fs::write(&config_path, config).unwrap();

    let output = run(root.path(), &["--agent", "codex", "list"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("official�]0;synthetic-title�"));
    assert!(stdout.contains("model�[31mred"));
    assert!(stdout.contains("high�[0m"));
    assert!(!stdout.contains('\u{1b}'));
    assert!(!stdout.contains('\u{7}'));
}
