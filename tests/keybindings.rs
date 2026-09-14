#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;

use config::Config;
use std::fs;
use tempfile::tempdir;

fn load(text: &str) -> anyhow::Result<Config> {
    let directory = tempdir()?;
    let path = directory.path().join("config.toml");
    fs::write(&path, text)?;
    Config::load(&path)
}

fn config_with_bindings(bindings: &str) -> String {
    format!(
        "version=1\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n[keybindings.fzf]\n{bindings}\n"
    )
}

fn config_with_tui_bindings(bindings: &str) -> String {
    format!(
        "version=1\n[endpoints.official]\nauth='native'\n[profiles.default]\nagent='codex'\nendpoint='official'\n[keybindings.tui]\n{bindings}\n"
    )
}

#[test]
fn tui_keybindings_default_to_native_actions() {
    let config = load(&config_with_tui_bindings("")).unwrap();
    let bindings = config.keybindings.tui;
    assert_eq!(bindings.accept, "enter");
    assert_eq!(bindings.cancel, "esc");
    assert_eq!(bindings.choose_agent, "ctrl-l");
    assert_eq!(bindings.choose_endpoint, "ctrl-e");
    assert_eq!(bindings.help, "f1");
}

#[test]
fn tui_keybindings_canonicalize_aliases_and_reject_conflicts() {
    let config = load(&config_with_tui_bindings("accept='ctrl-m'\nhelp='f4'")).unwrap();
    assert_eq!(config.keybindings.tui.accept, "enter");
    assert_eq!(config.keybindings.tui.help, "f4");
    for binding in [
        "accept='a'",
        "accept='ctrl-c'",
        "accept='ctrl-m'\ncancel='enter'",
    ] {
        let error = load(&config_with_tui_bindings(binding))
            .unwrap_err()
            .to_string();
        assert!(error.contains("TUI keybindings") || error.contains("Ctrl-C"));
    }
}

#[test]
fn fzf_keybindings_default_to_safe_static_actions() {
    let config = load(&config_with_bindings("")).unwrap();
    let bindings = config.keybindings.fzf;
    assert_eq!(bindings.accept, "enter");
    assert_eq!(bindings.cancel, "esc");
    assert_eq!(bindings.choose_agent, "ctrl-l");
    assert_eq!(bindings.choose_endpoint, "ctrl-e");
}

#[test]
fn fzf_keybindings_reject_shell_text_printable_keys_and_ctrl_c() {
    for binding in [
        "accept='execute(echo unsafe)'",
        "accept='a'",
        "accept='space'",
        "accept='ctrl-c'",
    ] {
        let error = load(&config_with_bindings(binding))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Fzf keybindings") || error.contains("Ctrl-C"));
    }
}

#[test]
fn fzf_keybindings_reject_duplicate_terminal_equivalents() {
    let error = load(&config_with_bindings("accept='ctrl-m'\ncancel='enter'"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("same key"));
}
