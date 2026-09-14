use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nomad"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root)
        .env("HOME", root)
        .env("CODEX_HOME", root.join("codex-home"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude-config"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .args(args)
        .output()
        .unwrap()
}

fn config(root: &Path, name: &str) {
    fs::create_dir_all(root.join("config/nomad")).unwrap();
    fs::write(root.join("config/nomad/config.toml"), format!("version=1\n[endpoints.official]\nauth='native'\n[profiles.{name}]\nagent='codex'\nendpoint='official'\nmodel='synthetic-model'\nargs=['--local-only']\n")).unwrap();
}

#[test]
fn portable_export_omits_local_arguments_and_import_requires_a_preview() {
    let sender = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    config(sender.path(), "safe");
    fs::create_dir_all(receiver.path().join("config/nomad")).unwrap();
    fs::write(
        receiver.path().join("config/nomad/config.toml"),
        "version=1\nendpoints={}\nprofiles={}\n",
    )
    .unwrap();
    let bundle = sender.path().join("bundle");
    let export = invoke(
        sender.path(),
        &[
            "--agent",
            "codex",
            "export",
            bundle.to_str().unwrap(),
            "--profile",
            "safe",
        ],
    );
    assert!(export.status.success(), "{export:?}");
    let contents = fs::read_to_string(bundle.join("profiles.toml")).unwrap();
    assert!(contents.contains("synthetic-model"));
    assert!(!contents.contains("local-only"));
    let skills = receiver.path().join("skills");
    let preview = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(preview.status.success(), "{preview:?}");
    let id = String::from_utf8(preview.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    assert!(invoke(receiver.path(), &["apply", &id]).status.success());
    let imported = fs::read_to_string(receiver.path().join("config/nomad/config.toml")).unwrap();
    assert!(imported.contains("synthetic-model"));
    assert!(!imported.contains("local-only"));
    fs::write(
        receiver.path().join("config/nomad/config.toml"),
        format!("{imported}# later edit\n"),
    )
    .unwrap();
    let restore = invoke(receiver.path(), &["restore", &id]);
    assert!(!restore.status.success());
    assert_eq!(
        fs::read_to_string(receiver.path().join("config/nomad/config.toml")).unwrap(),
        format!("{imported}# later edit\n")
    );
    fs::write(receiver.path().join("config/nomad/config.toml"), &imported).unwrap();
    assert!(invoke(receiver.path(), &["restore", &id]).status.success());
    assert_eq!(
        fs::read_to_string(receiver.path().join("config/nomad/config.toml")).unwrap(),
        "version=1\nendpoints={}\nprofiles={}\n"
    );
}

#[test]
fn portable_export_rejects_synthetic_secret_without_echoing_it() {
    let root = TempDir::new().unwrap();
    config(root.path(), "safe");
    let skill = root.path().join("safe-skill");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "api_key = 'synthetic-secret-0123456789'\n", // gitleaks:allow -- Synthetic credential rejection fixture.
    )
    .unwrap();
    let output = invoke(
        root.path(),
        &[
            "--agent",
            "codex",
            "export",
            root.path().join("bundle").to_str().unwrap(),
            "--skill",
            skill.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-secret-0123456789"));
}

#[test]
fn skills_only_bundle_round_trips_and_rejects_unlisted_content() {
    let sender = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    fs::create_dir_all(sender.path().join("config/nomad")).unwrap();
    fs::write(
        sender.path().join("config/nomad/config.toml"),
        "version=1\nendpoints={}\nprofiles={}\n",
    )
    .unwrap();
    fs::create_dir_all(receiver.path().join("config/nomad")).unwrap();
    fs::write(
        receiver.path().join("config/nomad/config.toml"),
        "version=1\nendpoints={}\nprofiles={}\n",
    )
    .unwrap();
    let skill = sender.path().join("portable-skill");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "Use only safe text.\n").unwrap();
    let bundle = sender.path().join("bundle");
    assert!(
        invoke(
            sender.path(),
            &[
                "--agent",
                "codex",
                "export",
                bundle.to_str().unwrap(),
                "--skill",
                skill.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let skills = receiver.path().join("skills");
    let preview = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(preview.status.success(), "{preview:?}");
    let id = String::from_utf8(preview.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    assert!(invoke(receiver.path(), &["apply", &id]).status.success());
    assert_eq!(
        fs::read_to_string(skills.join("portable-skill/SKILL.md")).unwrap(),
        "Use only safe text.\n"
    );
    fs::write(skills.join("portable-skill/SKILL.md"), "later local edit\n").unwrap();
    assert!(!invoke(receiver.path(), &["restore", &id]).status.success());
    assert_eq!(
        fs::read_to_string(skills.join("portable-skill/SKILL.md")).unwrap(),
        "later local edit\n"
    );
    fs::write(
        skills.join("portable-skill/SKILL.md"),
        "Use only safe text.\n",
    )
    .unwrap();
    assert!(invoke(receiver.path(), &["restore", &id]).status.success());
    assert!(!skills.join("portable-skill").exists());
    fs::write(bundle.join("skills/portable-skill/tool.sh"), "#!/bin/sh\n").unwrap();
    let rejected = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    fs::remove_file(bundle.join("skills/portable-skill/tool.sh")).unwrap();
    std::process::Command::new("/usr/bin/mkfifo")
        .arg(bundle.join("skills/portable-skill/pipe"))
        .status()
        .unwrap();
    let fifo = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(!fifo.status.success());
    fs::remove_file(bundle.join("skills/portable-skill/pipe")).unwrap();
    fs::create_dir_all(bundle.join("skills/portable-skill/dependency")).unwrap();
    let empty_directory = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(!empty_directory.status.success());
    fs::remove_dir(bundle.join("skills/portable-skill/dependency")).unwrap();
    let alias = sender.path().join("bundle-parent");
    symlink(sender.path(), &alias).unwrap();
    let ancestor = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            alias.join("bundle").to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(!ancestor.status.success());
    fs::create_dir_all(skill.join("empty-dependency")).unwrap();
    let empty_source_directory = invoke(
        sender.path(),
        &[
            "--agent",
            "codex",
            "export",
            sender.path().join("empty-bundle").to_str().unwrap(),
            "--skill",
            skill.to_str().unwrap(),
        ],
    );
    assert!(!empty_source_directory.status.success());
}

#[test]
fn portable_export_rejects_json_and_pem_credentials_without_echoing_them() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("config/nomad")).unwrap();
    fs::write(
        root.path().join("config/nomad/config.toml"),
        "version=1\nendpoints={}\nprofiles={}\n",
    )
    .unwrap();
    for (name, text, secret) in [
        (
            "json-skill",
            "{\"api_key\":\"json-synthetic-0123456789\"}\n",
            "json-synthetic-0123456789",
        ),
        (
            "pem-skill",
            "-----BEGIN PRIVATE KEY-----\nPEM-SYNTHETIC\n",
            "PEM-SYNTHETIC",
        ),
    ] {
        let skill = root.path().join(name);
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), text).unwrap();
        let output = invoke(
            root.path(),
            &[
                "--agent",
                "codex",
                "export",
                root.path().join(format!("{name}-bundle")).to_str().unwrap(),
                "--skill",
                skill.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
}

#[test]
fn portable_export_rejects_credential_url_without_echoing_it() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("config/nomad")).unwrap();
    let secret = "synthetic-password-123";
    fs::write(root.path().join("config/nomad/config.toml"), format!("version=1\n[endpoints.remote]\nauth='api-key'\nprotocol='openai-responses'\nbase_url='https://user:{secret}@example.invalid/v1'\nkey_env='LOCAL_KEY'\n[profiles.safe]\nagent='codex'\nendpoint='remote'\n")).unwrap();
    let output = invoke(
        root.path(),
        &[
            "--agent",
            "codex",
            "export",
            root.path().join("bundle").to_str().unwrap(),
            "--profile",
            "safe",
        ],
    );
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
}

#[test]
fn replacing_a_skill_round_trips_existing_safe_content_and_modes() {
    let sender = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    for root in [sender.path(), receiver.path()] {
        fs::create_dir_all(root.join("config/nomad")).unwrap();
        fs::write(
            root.join("config/nomad/config.toml"),
            "version=1\nendpoints={}\nprofiles={}\n",
        )
        .unwrap();
    }
    let source_skill = sender.path().join("portable-skill");
    fs::create_dir_all(source_skill.join("docs")).unwrap();
    fs::write(source_skill.join("SKILL.md"), "new skill\n").unwrap();
    fs::write(source_skill.join("docs/guide.md"), "new guide\n").unwrap();
    let bundle = sender.path().join("bundle");
    assert!(
        invoke(
            sender.path(),
            &[
                "--agent",
                "codex",
                "export",
                bundle.to_str().unwrap(),
                "--skill",
                source_skill.to_str().unwrap(),
            ],
        )
        .status
        .success()
    );

    let skills = receiver.path().join("skills");
    let target = skills.join("portable-skill");
    fs::create_dir_all(target.join("legacy")).unwrap();
    fs::write(target.join("SKILL.md"), "old skill\n").unwrap();
    fs::write(target.join("legacy/old.md"), "old guide\n").unwrap();
    fs::create_dir_all(skills.join("unrelated")).unwrap();
    fs::write(skills.join("unrelated/SKILL.md"), "unrelated\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(target.join("legacy"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(target.join("SKILL.md"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(
        target.join("legacy/old.md"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();

    let conflict = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(!conflict.status.success());
    let preview = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
            "--replace",
            "skill:portable-skill",
        ],
    );
    assert!(preview.status.success(), "{preview:?}");
    let id = String::from_utf8(preview.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    assert!(invoke(receiver.path(), &["apply", &id]).status.success());
    assert_eq!(
        fs::read_to_string(target.join("SKILL.md")).unwrap(),
        "new skill\n"
    );
    assert!(target.join("docs/guide.md").exists());
    assert!(!target.join("legacy/old.md").exists());
    assert_eq!(
        fs::read_to_string(skills.join("unrelated/SKILL.md")).unwrap(),
        "unrelated\n"
    );

    assert!(invoke(receiver.path(), &["restore", &id]).status.success());
    assert_eq!(
        fs::read_to_string(target.join("SKILL.md")).unwrap(),
        "old skill\n"
    );
    assert_eq!(
        fs::read_to_string(target.join("legacy/old.md")).unwrap(),
        "old guide\n"
    );
    assert!(!target.join("docs/guide.md").exists());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(target.join("legacy"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(target.join("SKILL.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(target.join("legacy/old.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert_eq!(
        fs::read_to_string(skills.join("unrelated/SKILL.md")).unwrap(),
        "unrelated\n"
    );
}

#[test]
fn import_drift_and_conflicts_stop_before_any_write() {
    let sender = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    config(sender.path(), "safe");
    config(receiver.path(), "safe");
    let receiver_config = receiver.path().join("config/nomad/config.toml");
    fs::write(
        &receiver_config,
        fs::read_to_string(&receiver_config)
            .unwrap()
            .replace("synthetic-model", "different-model"),
    )
    .unwrap();
    let bundle = sender.path().join("bundle");
    assert!(
        invoke(
            sender.path(),
            &[
                "--agent",
                "codex",
                "export",
                bundle.to_str().unwrap(),
                "--profile",
                "safe"
            ]
        )
        .status
        .success()
    );
    let skills = receiver.path().join("skills");
    let conflict = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    assert!(!conflict.status.success());
    let preview = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
            "--replace",
            "profile:safe",
        ],
    );
    assert!(preview.status.success());
    let id = String::from_utf8(preview.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    fs::write(
        receiver.path().join("config/nomad/config.toml"),
        "version=1\nendpoints={}\nprofiles={}\n",
    )
    .unwrap();
    let applied = invoke(receiver.path(), &["apply", &id]);
    assert!(!applied.status.success());
    assert_eq!(
        fs::read_to_string(receiver.path().join("config/nomad/config.toml")).unwrap(),
        "version=1\nendpoints={}\nprofiles={}\n"
    );
}

#[test]
fn obstructed_journal_and_tampered_records_never_apply_or_restore_changes() {
    let sender = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    config(sender.path(), "safe");
    fs::create_dir_all(receiver.path().join("config/nomad")).unwrap();
    let receiver_config = receiver.path().join("config/nomad/config.toml");
    fs::write(&receiver_config, "version=1\nendpoints={}\nprofiles={}\n").unwrap();
    let bundle = sender.path().join("bundle");
    assert!(
        invoke(
            sender.path(),
            &[
                "--agent",
                "codex",
                "export",
                bundle.to_str().unwrap(),
                "--profile",
                "safe"
            ]
        )
        .status
        .success()
    );
    let skills = receiver.path().join("skills");
    let preview = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    let id = String::from_utf8(preview.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    let operations = receiver.path().join("state/nomad/sharing/operations");
    fs::create_dir_all(operations.parent().unwrap()).unwrap();
    fs::write(&operations, "obstructed").unwrap();
    assert!(!invoke(receiver.path(), &["apply", &id]).status.success());
    assert_eq!(
        fs::read_to_string(&receiver_config).unwrap(),
        "version=1\nendpoints={}\nprofiles={}\n"
    );
    fs::remove_file(&operations).unwrap();
    let plan = receiver
        .path()
        .join(format!("state/nomad/sharing/plans/{id}.json"));
    let plan_text = fs::read_to_string(&plan).unwrap();
    fs::write(
        &plan,
        plan_text.replacen("\"version\":1", "\"version\":99", 1),
    )
    .unwrap();
    assert!(!invoke(receiver.path(), &["apply", &id]).status.success());
    assert_eq!(
        fs::read_to_string(&receiver_config).unwrap(),
        "version=1\nendpoints={}\nprofiles={}\n"
    );

    let clean = invoke(
        receiver.path(),
        &[
            "--agent",
            "codex",
            "import",
            bundle.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
        ],
    );
    let clean_id = String::from_utf8(clean.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    assert!(
        invoke(receiver.path(), &["apply", &clean_id])
            .status
            .success()
    );
    let after = fs::read_to_string(&receiver_config).unwrap();
    let journal = receiver
        .path()
        .join(format!("state/nomad/sharing/operations/{clean_id}.json"));
    let journal_text = fs::read_to_string(&journal).unwrap();
    fs::write(
        &journal,
        journal_text.replacen("\"version\":1", "\"version\":99", 1),
    )
    .unwrap();
    assert!(
        !invoke(receiver.path(), &["restore", &clean_id])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&receiver_config).unwrap(), after);
}
