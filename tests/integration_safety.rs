use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap()
}

fn invoke(root: &Path, args: &[&str]) -> Output {
    let root = canonical(root);
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

fn configure(root: &Path, body: &str) {
    let config = canonical(root).join("config/nomad/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        config,
        format!("version = 1\nendpoints = {{}}\nprofiles = {{}}\n{body}"),
    )
    .unwrap();
}

fn operation_id(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .last()
        .unwrap()
        .to_owned()
}

#[test]
fn shell_journals_only_the_managed_block_and_restore_rejects_later_text() {
    let root = root();
    let root_path = canonical(root.path());
    let target = root_path.join("shellrc");
    let private_line = "export PRIVATE_FIXTURE='synthetic-secret'\n";
    fs::write(&target, private_line).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    configure(
        root.path(),
        &format!("[integrations.shell]\ntarget = {:?}\n", target),
    );

    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    let plan = fs::read(root_path.join("state/nomad/plans/shell.json")).unwrap();
    assert!(!String::from_utf8_lossy(&plan).contains("synthetic-secret"));
    assert!(!String::from_utf8_lossy(&plan).contains("PRIVATE_FIXTURE"));

    let applied = invoke(root.path(), &["integrate", "apply", "shell"]);
    assert!(applied.status.success(), "{applied:?}");
    let id = operation_id(&applied);
    let journal = fs::read(root_path.join(format!("state/nomad/operations/{id}.json"))).unwrap();
    assert!(!String::from_utf8_lossy(&journal).contains("synthetic-secret"));
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let installed = fs::read_to_string(&target).unwrap();
    fs::write(&target, format!("{installed}# added later\n")).unwrap();
    let restored = invoke(root.path(), &["integrate", "restore", &id]);
    assert!(!restored.status.success(), "{restored:?}");
    assert!(
        String::from_utf8_lossy(&restored.stderr).contains("Target changed after installation")
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        format!("{installed}# added later\n")
    );
    assert!(
        root_path
            .join(format!("state/nomad/operations/{id}.json"))
            .is_file()
    );
    fs::write(&target, installed).unwrap();
    assert!(
        invoke(root.path(), &["integrate", "restore", &id])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), private_line);
}

#[test]
fn codex_journal_omits_unrelated_values_and_restore_rejects_later_edits() {
    let root = root();
    let root_path = canonical(root.path());
    let target = root_path.join("codex/config.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(
        &target,
        "model = 'synthetic-secret-model'\n[tui]\ntheme = 'dark'\n",
    )
    .unwrap();
    configure(
        root.path(),
        &format!("[integrations.codex-statusline]\ntarget = {:?}\n", target),
    );

    assert!(
        invoke(root.path(), &["integrate", "plan", "codex-statusline"])
            .status
            .success()
    );
    let applied = invoke(root.path(), &["integrate", "apply", "codex-statusline"]);
    assert!(applied.status.success(), "{applied:?}");
    let id = operation_id(&applied);
    let journal = fs::read(root_path.join(format!("state/nomad/operations/{id}.json"))).unwrap();
    assert!(!String::from_utf8_lossy(&journal).contains("synthetic-secret-model"));

    let installed = fs::read_to_string(&target).unwrap();
    let mut document = installed.clone();
    document.push_str("notifications = true\n");
    fs::write(&target, document).unwrap();
    let restored = invoke(root.path(), &["integrate", "restore", &id]);
    assert!(!restored.status.success(), "{restored:?}");
    assert!(
        String::from_utf8_lossy(&restored.stderr).contains("Target changed after installation")
    );
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("notifications = true")
    );
    fs::write(&target, installed).unwrap();
    assert!(
        invoke(root.path(), &["integrate", "restore", &id])
            .status
            .success()
    );
    let restored = fs::read_to_string(&target).unwrap();
    assert!(restored.contains("synthetic-secret-model"));
    assert!(!restored.contains("status_line"));
}

#[test]
fn target_drift_after_plan_is_rejected_without_writing_or_journaling() {
    let root = root();
    let root_path = canonical(root.path());
    let target = root_path.join("shellrc");
    fs::write(&target, "# reviewed\n").unwrap();
    configure(
        root.path(),
        &format!("[integrations.shell]\ntarget = {:?}\n", target),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    fs::write(&target, "# changed\n").unwrap();

    let applied = invoke(root.path(), &["integrate", "apply", "shell"]);
    assert!(!applied.status.success());
    assert!(
        String::from_utf8_lossy(&applied.stderr)
            .contains("managed target state changed since preview")
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "# changed\n");
    assert_eq!(
        fs::read_dir(root_path.join("state/nomad/operations"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn journal_left_before_target_write_is_recoverable_without_touching_target() {
    let root = root();
    let root_path = canonical(root.path());
    let target = root_path.join("shellrc");
    fs::write(&target, "# unchanged\n").unwrap();
    configure(
        root.path(),
        &format!("[integrations.shell]\ntarget = {:?}\n", target),
    );
    assert!(
        invoke(root.path(), &["integrate", "plan", "shell"])
            .status
            .success()
    );
    let operations = root_path.join("state/nomad/operations");
    fs::create_dir_all(&operations).unwrap();
    let operation = operations.join("77-88.json");
    fs::copy(root_path.join("state/nomad/plans/shell.json"), &operation).unwrap();

    let restored = invoke(root.path(), &["integrate", "restore", "77-88"]);
    assert!(restored.status.success(), "{restored:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "# unchanged\n");
    assert!(!operation.exists());
}

#[test]
fn fifo_target_is_rejected_without_blocking_or_creating_a_plan() {
    let root = root();
    let root_path = canonical(root.path());
    let target = root_path.join("shell-fifo");
    assert!(
        Command::new("/usr/bin/mkfifo")
            .arg(&target)
            .status()
            .unwrap()
            .success()
    );
    configure(
        root.path(),
        &format!("[integrations.shell]\ntarget = {:?}\n", target),
    );

    let rejected = invoke(root.path(), &["integrate", "plan", "shell"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("not a regular file"));
    assert!(!root_path.join("state/nomad/plans/shell.json").exists());
}

#[test]
fn symlinked_target_or_state_component_is_rejected_before_target_changes() {
    let root = root();
    let root_path = canonical(root.path());
    let real = root_path.join("real");
    let linked = root_path.join("linked");
    fs::create_dir(&real).unwrap();
    symlink(&real, &linked).unwrap();
    let target = linked.join("shellrc");
    configure(
        root.path(),
        &format!("[integrations.shell]\ntarget = {:?}\n", target),
    );
    let rejected = invoke(root.path(), &["integrate", "plan", "shell"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("symlink component"));
    assert!(!real.join("shellrc").exists());

    fs::remove_file(&linked).unwrap();
    let state_target = root_path.join("state-target");
    fs::create_dir(&state_target).unwrap();
    symlink(&state_target, root_path.join("state")).unwrap();
    configure(
        root.path(),
        &format!(
            "[integrations.shell]\ntarget = {:?}\n",
            root_path.join("safe-shellrc")
        ),
    );
    let rejected = invoke(root.path(), &["integrate", "plan", "shell"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("symlink component"));
    assert!(!root_path.join("safe-shellrc").exists());
}

#[test]
fn legacy_snapshot_operation_is_left_in_place_with_a_migration_error() {
    let root = root();
    let root_path = canonical(root.path());
    let operations = root_path.join("state/nomad/operations");
    fs::create_dir_all(&operations).unwrap();
    let record = operations.join("12-34.json");
    fs::write(
        &record,
        r#"{"target":"/tmp/fixture","before":{"File":{"bytes":[115,101,99,114,101,116],"mode":384}},"after":"Missing"}"#,
    )
    .unwrap();

    let restored = invoke(root.path(), &["integrate", "restore", "12-34"]);
    assert!(!restored.status.success());
    assert!(
        String::from_utf8_lossy(&restored.stderr)
            .contains("Legacy whole-snapshot integration operation detected")
    );
    assert!(record.is_file());
}

#[test]
fn oversized_native_input_is_rejected_without_creating_a_plan() {
    let root = root();
    let root_path = canonical(root.path());
    let target = root_path.join("large-shellrc");
    fs::write(&target, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    configure(
        root.path(),
        &format!("[integrations.shell]\ntarget = {:?}\n", target),
    );

    let rejected = invoke(root.path(), &["integrate", "plan", "shell"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("size limit"));
    assert!(!root_path.join("state/nomad/plans/shell.json").exists());
}
