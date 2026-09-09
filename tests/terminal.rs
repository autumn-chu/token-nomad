use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    sync::mpsc,
    time::Duration,
};

#[test]
fn native_agent_keeps_terminal_resize_suspend_input_and_exit() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::write(&agent,"#!/bin/sh\ntest -t 0 && test -t 1 || exit 99\nprintf 'READY\\n'\nread answer\nstty size\nprintf 'ANSWER=%s\\n' \"$answer\"\nexit 17\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config,format!("version=1\n[executables]\ncodex={:?}\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n",agent.to_str().unwrap())).unwrap();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nomad"));
    command.args(["--config", config.to_str().unwrap(), "run", "default"]);
    command.cwd(root.path());
    command.env("HOME", root.path());
    command.env("XDG_CONFIG_HOME", root.path().join("config"));
    command.env("XDG_STATE_HOME", root.path().join("state"));
    command.env("XDG_CACHE_HOME", root.path().join("cache"));
    command.env("CODEX_HOME", root.path().join("codex-home"));
    command.env("CLAUDE_CONFIG_DIR", root.path().join("claude-home"));
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
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0; 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if tx
                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                .is_err()
            {
                break;
            }
        }
    });
    let mut output = String::new();
    while !output.contains("READY") {
        output.push_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }
    let pid = child.process_id().unwrap().to_string();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-STOP", &pid])
            .status()
            .unwrap()
            .success()
    );
    assert!(child.try_wait().unwrap().is_none());
    pair.master
        .resize(PtySize {
            rows: 33,
            cols: 91,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-CONT", &pid])
            .status()
            .unwrap()
            .success()
    );
    writer.write_all(b"hello terminal\n").unwrap();
    while !output.contains("ANSWER=hello terminal") {
        output.push_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }
    assert!(output.contains("33 91"), "{output}");
    assert_eq!(child.wait().unwrap().exit_code(), 17);
}

#[test]
fn ctrl_c_reaches_the_native_agent() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::write(
        &agent,
        "#!/bin/sh\ntrap 'echo INTERRUPTED; exit 130' INT\necho READY\nread answer\n",
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config,format!("version=1\n[executables]\ncodex={:?}\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n",agent.to_str().unwrap())).unwrap();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nomad"));
    command.args(["--config", config.to_str().unwrap(), "run", "default"]);
    command.cwd(root.path());
    command.env("HOME", root.path());
    command.env("XDG_CONFIG_HOME", root.path().join("config"));
    command.env("XDG_STATE_HOME", root.path().join("state"));
    command.env("XDG_CACHE_HOME", root.path().join("cache"));
    command.env("CODEX_HOME", root.path().join("codex-home"));
    command.env("CLAUDE_CONFIG_DIR", root.path().join("claude-home"));
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
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0; 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if tx
                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                .is_err()
            {
                break;
            }
        }
    });
    let mut output = String::new();
    while !output.contains("READY") {
        output.push_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }
    writer.write_all(&[3]).unwrap();
    while !output.contains("INTERRUPTED") {
        output.push_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }
    assert_eq!(child.wait().unwrap().exit_code(), 130);
}
