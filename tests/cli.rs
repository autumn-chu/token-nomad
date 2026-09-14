use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn invoke(root: &Path, args: &[&str]) -> Output {
    let root = root.canonicalize().unwrap();
    Command::new(env!("CARGO_BIN_EXE_nomad"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&root)
        .env("HOME", &root)
        .env("CODEX_HOME", root.join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude-config"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .args(args)
        .output()
        .unwrap()
}
fn invoke_with_config(root: &Path, config: &Path, args: &[&str]) -> Output {
    let root = root.canonicalize().unwrap();
    let config = config.canonicalize().unwrap();
    Command::new(env!("CARGO_BIN_EXE_nomad"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&root)
        .env("HOME", &root)
        .env("CODEX_HOME", root.join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude-config"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .arg("--config")
        .arg(&config)
        .args(args)
        .output()
        .unwrap()
}
fn setup() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    assert!(invoke(root.path(), &["init"]).status.success());
    root
}
fn canonical_root(root: &TempDir) -> std::path::PathBuf {
    root.path().canonicalize().unwrap()
}
fn configure(root: &Path, text: &str) {
    fs::write(root.join("config/nomad/config.toml"), text).unwrap();
}
fn fake(root: &Path) -> String {
    let path = root.join("fake agent");
    fs::write(&path, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf 'key=%s\\n' \"$NOMAD_ENDPOINT_KEY\"\nprintf 'cwd=%s\\n' \"$PWD\"\nexit 17\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn init_preserves_existing_configuration() {
    let root = setup();
    let before = fs::read(root.path().join("config/nomad/config.toml")).unwrap();
    assert!(!invoke(root.path(), &["init"]).status.success());
    assert_eq!(
        before,
        fs::read(root.path().join("config/nomad/config.toml")).unwrap()
    );
}

#[test]
fn lists_defaults_and_rejects_noninteractive_selection() {
    let root = setup();
    let output = invoke(root.path(), &["list"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("native default"));
    assert!(!invoke(root.path(), &[]).status.success());
}

#[test]
fn custom_codex_preserves_arguments_credentials_directory_and_exit_code() {
    let root = setup();
    let exe = fake(root.path());
    configure(
        root.path(),
        &format!(
            "version=1\n[executables]\ncodex={exe:?}\n[endpoints.company]\nauth='api-key'\nprotocol='openai-responses'\nbase_url='https://example.com/v1'\nkey_env='TEST_SECRET'\n[profiles.deep]\nagent='codex'\nendpoint='company'\nmodel='test-model'\nreasoning='high'\n"
        ),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_nomad"))
        .env_clear()
        .current_dir(root.path())
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("CODEX_HOME", root.path().join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.path().join("claude-config"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("TEST_SECRET", "synthetic-secret")
        .args([
            "--config",
            root.path()
                .join("config/nomad/config.toml")
                .to_str()
                .unwrap(),
            "run",
            "deep",
            "--cwd",
            root.path().to_str().unwrap(),
            "--",
            "two words",
            "$(no-shell)",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("two words\n$(no-shell)\n"));
    assert!(text.contains("key=synthetic-secret"));
    assert!(text.contains("model_provider=\"nomad\""));
    assert!(text.contains(&format!(
        "cwd={}",
        root.path().canonicalize().unwrap().display()
    )));
}

#[test]
fn dry_run_needs_no_key_and_never_prints_native_arguments() {
    let root = setup();
    let exe = fake(root.path());
    configure(
        root.path(),
        &format!(
            "version=1\n[executables]\ncodex={exe:?}\n[endpoints.company]\nauth='api-key'\nprotocol='openai-responses'\nbase_url='https://example.com/v1'\nkey_env='NOMAD_TEST_MISSING'\n[profiles.deep]\nagent='codex'\nendpoint='company'\nmodel='test'\n"
        ),
    );
    let output = invoke(
        root.path(),
        &[
            "run",
            "deep",
            "--dry-run",
            "--",
            "--api-key",
            "do-not-print",
        ],
    );
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("do-not-print"));
    assert!(!invoke(root.path(), &["run", "deep"]).status.success());
}

#[test]
fn incompatible_and_disabled_profiles_never_execute() {
    let root = setup();
    let exe = fake(root.path());
    for (protocol, disabled) in [("anthropic-messages", false), ("openai-responses", true)] {
        configure(
            root.path(),
            &format!(
                "version=1\n[executables]\ncodex={exe:?}\n[endpoints.company]\nauth='api-key'\nprotocol='{protocol}'\nbase_url='https://example.com/v1'\nkey_env='TEST_SECRET'\n[profiles.deep]\nagent='codex'\nendpoint='company'\nmodel='test'\ndisabled={disabled}\n"
            ),
        );
        assert!(
            !invoke(root.path(), &["run", "deep", "--dry-run"])
                .status
                .success()
        );
    }
}

#[test]
fn integration_is_reversible_and_protects_later_edits() {
    let root = setup();
    let target = canonical_root(&root).join("shellrc");
    fs::write(&target, "# keep me\n").unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.shell]\ntarget={:?}\n",
            target.to_str().unwrap()
        ),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "# keep me\n");
    let applied = invoke(root.path(), &["integrate", "apply", "shell"]);
    assert!(applied.status.success(), "{:?}", applied);
    let output = String::from_utf8(applied.stdout).unwrap();
    let id = output.split_whitespace().last().unwrap();
    assert!(
        invoke(root.path(), &["integrate", "apply", "shell"])
            .status
            .success()
    );
    let installed = fs::read_to_string(&target).unwrap();
    fs::write(&target, format!("{installed}# later edit\n")).unwrap();
    assert!(
        !invoke(root.path(), &["integrate", "restore", id])
            .status
            .success()
    );
    fs::write(&target, installed).unwrap();
    assert!(
        invoke(root.path(), &["integrate", "restore", id])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "# keep me\n");
}

#[test]
fn malformed_config_diagnostics_do_not_echo_source() {
    let root = setup();
    configure(root.path(), "version = 'secret-sensitive-value'");
    let output = invoke(root.path(), &["list"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-sensitive-value"));
}

#[test]
fn non_utf8_arguments_reach_native_cli() {
    use std::os::unix::ffi::OsStringExt;
    let root = setup();
    let exe = fake(root.path());
    configure(
        root.path(),
        &format!(
            "version=1\n[executables]\ncodex={exe:?}\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n"
        ),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_nomad"))
        .env_clear()
        .current_dir(root.path())
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("CODEX_HOME", root.path().join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.path().join("claude-config"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .args([
            "--config",
            root.path()
                .join("config/nomad/config.toml")
                .to_str()
                .unwrap(),
            "run",
            "default",
            "--",
        ])
        .arg(std::ffi::OsString::from_vec(vec![0xff, b'x']))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17));
    assert!(output.stdout.windows(2).any(|v| v == [0xff, b'x']));
}

#[test]
fn relative_path_executable_is_resolved_before_changing_directory() {
    let root = setup();
    let exe = root.path().join("codex");
    fs::write(&exe, "#!/bin/sh\necho correct-executable\n").unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let destination = root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nomad"))
        .env_clear()
        .current_dir(root.path())
        .env("PATH", ".")
        .env("HOME", root.path())
        .args([
            "--config",
            root.path()
                .join("config/nomad/config.toml")
                .to_str()
                .unwrap(),
            "run",
            "codex",
            "--cwd",
            destination.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "correct-executable\n"
    );
}

#[test]
fn changed_target_requires_a_new_integration_preview() {
    let root = setup();
    let target = canonical_root(&root).join("rc");
    fs::write(&target, "# original\n").unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.shell]\ntarget={:?}\n",
            target.to_str().unwrap()
        ),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    fs::write(&target, "# changed\n").unwrap();
    assert!(
        !invoke(root.path(), &["integrate", "apply", "shell"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "# changed\n");
}

#[test]
fn native_auth_settings_conflicts_are_reported_without_values() {
    let root = setup();
    let exe = fake(root.path());
    for agent in ["claude", "codex"] {
        configure(
            root.path(),
            &format!(
                "version=1\n[executables]\n{agent}={exe:?}\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='{agent}'\nendpoint='official'\n"
            ),
        );
        let dir = root.path().join(format!(".{agent}"));
        fs::create_dir_all(&dir).unwrap();
        if agent == "claude" {
            fs::write(
                dir.join("settings.json"),
                r#"{"env":{"ANTHROPIC_API_KEY":"synthetic-secret"}}"#,
            )
            .unwrap();
        } else {
            fs::write(
                dir.join("config.toml"),
                "[model_providers.openai]\nbase_url='https://synthetic-secret.example'\n",
            )
            .unwrap();
        }
        let result = invoke(root.path(), &["run", "default", "--dry-run"]);
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-secret"));
    }
}

#[test]
fn shell_integration_preserves_existing_ap_function_at_runtime() {
    let root = setup();
    let target = canonical_root(&root).join("rc");
    fs::write(&target, "ap() { echo existing; }\n").unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.shell]\ntarget={:?}\n",
            target.to_str().unwrap()
        ),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    assert!(
        invoke(root.path(), &["integrate", "apply", "shell"])
            .status
            .success()
    );
    let output = Command::new("/bin/bash")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("CODEX_HOME", root.path().join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.path().join("claude-config"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .current_dir(root.path())
        .args(["-c", "source \"$1\"; ap", "bash", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "existing\n");
}

#[test]
fn relative_integration_paths_use_the_selected_config_directory() {
    let root = setup();
    let config = root.path().join("custom/nomad/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        "version=1\nendpoints={}\nprofiles={}\n[integrations.shell]\ntarget='shellrc'\n",
    )
    .unwrap();
    assert!(
        invoke_with_config(root.path(), &config, &["integrate", "plan", "shell"])
            .status
            .success()
    );
    assert!(
        invoke_with_config(root.path(), &config, &["integrate", "apply", "shell"])
            .status
            .success()
    );
    let target = config.parent().unwrap().join("shellrc");
    assert!(target.is_file());
    assert!(!root.path().join("shellrc").exists());
}

#[test]
fn codex_statusline_preserves_unrelated_settings_and_restores_idempotently() {
    let root = setup();
    let target = canonical_root(&root).join(".codex/config.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let original = "model = 'keep-me'\ntui = { theme = 'dark' }\n";
    fs::write(&target, original).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.codex-statusline]\ntarget={:?}\n",
            target.to_str().unwrap()
        ),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "codex-statusline"])
            .status
            .success()
    );
    let state = root.path().join("state/nomad");
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(state.join("plans"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(state.join("plans/codex-statusline.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let applied = invoke(root.path(), &["integrate", "apply", "codex-statusline"]);
    assert!(applied.status.success(), "{applied:?}");
    let id = String::from_utf8_lossy(&applied.stdout)
        .split_whitespace()
        .last()
        .unwrap()
        .to_owned();
    assert_eq!(
        fs::metadata(state.join("operations"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(state.join(format!("operations/{id}.json")))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let installed = fs::read_to_string(&target).unwrap();
    let document: toml::Value = toml::from_str(&installed).unwrap();
    assert_eq!(
        document.get("model").and_then(|value| value.as_str()),
        Some("keep-me")
    );
    assert_eq!(
        document
            .get("tui")
            .and_then(|value| value.get("theme"))
            .and_then(|value| value.as_str()),
        Some("dark")
    );
    let status_line = document
        .get("tui")
        .and_then(|value| value.get("status_line"))
        .and_then(|value| value.as_array())
        .unwrap();
    let names: Vec<_> = status_line.iter().map(|value| value.as_str()).collect();
    assert_eq!(
        names,
        vec![
            Some("five-hour-limit"),
            Some("weekly-limit"),
            Some("current-dir"),
            Some("context-used"),
            Some("git-branch"),
            Some("model-with-reasoning"),
            Some("total-input-tokens"),
            Some("total-output-tokens"),
            Some("task-progress"),
            Some("thread-title"),
        ]
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let repeated = invoke(root.path(), &["integrate", "apply", "codex-statusline"]);
    assert!(repeated.status.success(), "{repeated:?}");
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("Already applied"));
    assert!(
        invoke(root.path(), &["integrate", "restore", &id])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    assert!(
        !invoke(root.path(), &["integrate", "restore", &id])
            .status
            .success()
    );
}

#[test]
fn claude_settings_preserve_fields_and_require_exact_expected_command() {
    let root = setup();
    let source = canonical_root(&root).join("renderer.sh");
    let target = canonical_root(&root).join(".claude/settings.json");
    fs::write(&source, "#!/bin/sh\nprintf ok\n").unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let original = serde_json::json!({
        "env": {"KEEP_ME": "value"},
        "permissions": {"allow": ["Read"]},
        "statusLine": {"type": "command", "command": "bash /old-renderer.sh", "extra": "keep-me"}
    });
    let original_bytes = format!("{}\n", serde_json::to_string_pretty(&original).unwrap());
    fs::write(&target, &original_bytes).unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.claude-settings]\ntarget={:?}\nsource={:?}\nexpected_command='bash /wrong-renderer.sh'\n",
            target.to_str().unwrap(),
            source.to_str().unwrap()
        ),
    );
    assert!(
        !invoke(root.path(), &["integrate", "plan", "claude-settings"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), original_bytes);
    let expected = format!(
        "bash '{}'",
        source.canonicalize().unwrap().to_str().unwrap()
    );
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.claude-settings]\ntarget={:?}\nsource={:?}\nexpected_command='bash /old-renderer.sh'\n",
            target.to_str().unwrap(),
            source.to_str().unwrap(),
        ),
    );
    let planned = invoke(root.path(), &["integrate", "plan", "claude-settings"]);
    assert!(planned.status.success(), "{planned:?}");
    let applied = invoke(root.path(), &["integrate", "apply", "claude-settings"]);
    assert!(applied.status.success(), "{applied:?}");
    let changed: serde_json::Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(changed["env"]["KEEP_ME"], "value");
    assert_eq!(changed["permissions"]["allow"][0], "Read");
    assert_eq!(changed["statusLine"]["command"], expected);
    assert_eq!(changed["statusLine"]["extra"], "keep-me");
}

#[test]
fn existing_symlink_is_never_replaced_by_source_integration() {
    let root = setup();
    let old_source = canonical_root(&root).join("old-statusline");
    let new_source = canonical_root(&root).join("new-statusline");
    let target = canonical_root(&root).join(".claude/statusline.sh");
    fs::write(&old_source, "old\n").unwrap();
    fs::write(&new_source, "new\n").unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    symlink(&old_source, &target).unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.claude-statusline]\ntarget={:?}\nsource={:?}\n",
            target.to_str().unwrap(),
            new_source.to_str().unwrap()
        ),
    );
    let output = invoke(root.path(), &["integrate", "plan", "claude-statusline"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("regular file"));
    assert_eq!(fs::read_link(&target).unwrap(), old_source);
}

#[test]
fn failed_install_cleans_temporary_files_and_can_be_retried() {
    let root = setup();
    let parent = canonical_root(&root).join("readonly");
    let target = parent.join("shellrc");
    fs::create_dir(&parent).unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.shell]\ntarget={:?}\n",
            target.to_str().unwrap()
        ),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
    let failed = invoke(root.path(), &["integrate", "apply", "shell"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Integration installation failed"));
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    assert_eq!(
        fs::read_dir(root.path().join("state/nomad/operations"))
            .unwrap()
            .count(),
        0
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        invoke(root.path(), &["integrate", "apply", "shell"])
            .status
            .success()
    );
    assert!(target.is_file());
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
}

#[test]
fn endpoint_io_diagnostics_include_operation_and_path() {
    let root = setup();
    let exe = fake(root.path());
    let settings = root.path().join(".codex/config.toml");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::create_dir(&settings).unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\n[executables]\ncodex={exe:?}\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n"
        ),
    );
    let output = invoke(root.path(), &["run", "default", "--dry-run"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Cannot inspect Codex configuration"));
    assert!(error.contains(settings.to_str().unwrap()));
}

#[test]
fn integration_target_io_diagnostics_include_operation_and_path() {
    let root = setup();
    let parent = canonical_root(&root).join("target-parent");
    let target = parent.join("shellrc");
    fs::write(&parent, "not a directory").unwrap();
    configure(
        root.path(),
        &format!(
            "version=1\nendpoints={{}}\nprofiles={{}}\n[integrations.shell]\ntarget={:?}\n",
            target.to_str().unwrap()
        ),
    );
    let output = invoke(root.path(), &["integrate", "plan", "shell"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("non-directory"));
}
