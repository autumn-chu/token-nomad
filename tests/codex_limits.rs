use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

const STATUS_LINE: [&str; 10] = [
    "five-hour-limit",
    "weekly-limit",
    "current-dir",
    "context-used",
    "git-branch",
    "model-with-reasoning",
    "total-input-tokens",
    "total-output-tokens",
    "task-progress",
    "thread-title",
];

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

fn configure(root: &Path, target: &Path) {
    fs::create_dir_all(root.join("config/nomad")).unwrap();
    fs::write(
        root.join("config/nomad/config.toml"),
        format!(
            "version = 1\nendpoints = {{}}\nprofiles = {{}}\n[integrations.codex-statusline]\ntarget = {:?}\n",
            target.to_str().unwrap()
        ),
    )
    .unwrap();
}

fn setup(original: &str) -> (TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let target = root
        .path()
        .canonicalize()
        .unwrap()
        .join(".codex/config.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, original).unwrap();
    configure(root.path(), &target);
    (root, target)
}

#[test]
fn default_limits_are_first_and_unrelated_tui_settings_survive_repeated_apply() {
    let (root, target) = setup(
        "model = 'keep-me'\nservice_tier = 'default'\n[tui]\ntheme = 'dark'\nnotifications = true\n",
    );

    let planned = invoke(root.path(), &["integrate", "plan", "codex-statusline"]);
    assert!(planned.status.success(), "{planned:?}");
    let applied = invoke(root.path(), &["integrate", "apply", "codex-statusline"]);
    assert!(applied.status.success(), "{applied:?}");

    let installed = fs::read(&target).unwrap();
    let document: toml::Value = toml::from_slice(&installed).unwrap();
    let status_line = document
        .get("tui")
        .and_then(|value| value.get("status_line"))
        .and_then(toml::Value::as_array)
        .unwrap();
    let values: Vec<_> = status_line
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(values, STATUS_LINE);
    assert_eq!(
        document.get("model").and_then(toml::Value::as_str),
        Some("keep-me")
    );
    assert_eq!(
        document.get("service_tier").and_then(toml::Value::as_str),
        Some("default")
    );
    assert_eq!(
        document
            .get("tui")
            .and_then(|value| value.get("theme"))
            .and_then(toml::Value::as_str),
        Some("dark")
    );
    assert_eq!(
        document
            .get("tui")
            .and_then(|value| value.get("notifications"))
            .and_then(toml::Value::as_bool),
        Some(true)
    );

    let repeated = invoke(root.path(), &["integrate", "apply", "codex-statusline"]);
    assert!(repeated.status.success(), "{repeated:?}");
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("Already applied"));
    assert_eq!(fs::read(&target).unwrap(), installed);
}

#[test]
fn existing_codex_status_line_still_requires_explicit_conflict_resolution() {
    let (root, target) = setup("[tui]\nstatus_line = ['custom']\n");
    let before = fs::read(&target).unwrap();

    let planned = invoke(root.path(), &["integrate", "plan", "codex-statusline"]);
    assert!(!planned.status.success(), "{planned:?}");
    assert!(
        String::from_utf8_lossy(&planned.stderr)
            .contains("Existing Codex status_line requires explicit conflict resolution")
    );
    assert_eq!(fs::read(&target).unwrap(), before);
}
