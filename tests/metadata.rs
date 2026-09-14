#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;

use config::{Agent, Config, Selector};
use std::fs;
use tempfile::tempdir;

fn load(text: &str) -> Config {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, text).unwrap();
    Config::load(&path).unwrap()
}

#[test]
fn existing_profiles_use_native_tui_and_empty_metadata_defaults() {
    let config = load(
        "version = 1\n[endpoints.official]\nauth = 'native'\n[profiles.codex]\nagent = 'codex'\nendpoint = 'official'\n",
    );

    assert_eq!(config.selector, Selector::Tui);
    let profile = config.profiles.get("codex").unwrap();
    assert_eq!(profile.agent, Agent::Codex);
    assert_eq!(profile.label, None);
    assert_eq!(profile.description, None);
    assert!(profile.tags.is_empty());
    assert_eq!(profile.order, None);
}

#[test]
fn builtin_selector_remains_an_explicit_compatibility_option() {
    let config = load(
        "version = 1\nselector = 'builtin'\n[endpoints.official]\nauth = 'native'\n[profiles.codex]\nagent = 'codex'\nendpoint = 'official'\n",
    );
    assert_eq!(config.selector, Selector::Builtin);
}

#[test]
fn profile_metadata_deserializes_with_optional_values() {
    let config = load(
        "version = 1\n[endpoints.official]\nauth = 'native'\n[profiles.review]\nagent = 'codex'\nendpoint = 'official'\nlabel = 'Review'\ndescription = 'Review changes with the team'\ntags = ['review', 'team']\norder = -7\n",
    );

    let profile = config.profiles.get("review").unwrap();
    assert_eq!(profile.label.as_deref(), Some("Review"));
    assert_eq!(
        profile.description.as_deref(),
        Some("Review changes with the team")
    );
    assert_eq!(profile.tags, ["review", "team"]);
    assert_eq!(profile.order, Some(-7));
}
