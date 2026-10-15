use super::*;
use serde_json::json;

fn cfg() -> Config {
    serde_json::from_value(json!({"jwt_secret":"test", "auth_password":"test"})).unwrap()
}

fn write_settings(root: &Path, file: &str, value: &Value) -> PathBuf {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

fn profile() -> Value {
    json!({"model":"relay-sonnet", "availableModels":["relay-sonnet","relay-opus"], "effortLevel":"high",
        "apiKeyHelper":"printenv RELAY_TEST_KEY", "env":{"ANTHROPIC_BASE_URL":"https://claude.example/api",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL":"relay-small"}, "permissions":{"deny":["Read(.secret)"]}})
}

#[test]
fn discovers_native_profiles_without_dotenv_and_reloads_edits() {
    let root = tempfile::tempdir().unwrap();
    assert!(providers_at(Some(root.path()))
        .unwrap()
        .providers
        .is_empty());
    write_settings(root.path(), "providers/zeta.json", &profile());
    write_settings(
        root.path(),
        "providers/alpha.json",
        &json!({"model":"custom", "env":{
        "ANTHROPIC_BASE_URL":"https://other.example", "ANTHROPIC_AUTH_TOKEN":"private-token"}}),
    );
    write_settings(root.path(), "providers/ignored.txt", &profile());
    let listing = providers_at(Some(root.path())).unwrap();
    assert_eq!(
        listing
            .providers
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "zeta"]
    );
    assert!(listing.warnings.is_empty());
    assert_eq!(
        listing.providers[1].default_model.as_deref(),
        Some("relay-sonnet")
    );
    assert_eq!(
        listing.providers[1].models,
        ["relay-sonnet", "relay-opus", "relay-small"]
    );
    assert_eq!(
        listing.providers[1].default_reasoning_level.as_deref(),
        Some("high")
    );
    let mut edited = profile();
    edited["model"] = "new-model".into();
    write_settings(root.path(), "providers/zeta.json", &edited);
    assert_eq!(
        read_provider(root.path(), "zeta")
            .unwrap()
            .default_model
            .as_deref(),
        Some("new-model")
    );
    std::fs::remove_file(root.path().join("providers/zeta.json")).unwrap();
    assert!(read_provider(root.path(), "zeta").is_err());
    assert_eq!(providers_at(Some(root.path())).unwrap().providers.len(), 1);
}

#[test]
fn native_defaults_read_model_aliases_effort_and_respect_legacy_explicit_overrides() {
    let root = tempfile::tempdir().unwrap();
    write_settings(
        root.path(),
        "settings.json",
        &json!({"model":"saved-model", "availableModels":["saved-model","other-model"],
        "effortLevel":"medium", "env":{"ANTHROPIC_MODEL":"native-env-model", "CLAUDE_CODE_EFFORT_LEVEL":"high"}}),
    );
    let settings = read_cli_settings_at(Some(root.path())).unwrap();
    let defaults = cli_defaults_from(&cfg(), &settings, Some("inherited-process-model"));
    assert_eq!(defaults.model, "native-env-model");
    assert_eq!(defaults.reasoning_level.as_deref(), Some("high"));
    assert_eq!(
        defaults.models,
        ["native-env-model", "saved-model", "other-model"]
    );
    let legacy: Config =
        serde_json::from_value(json!({"jwt_secret":"test", "auth_password":"test",
        "claude_model":"explicit-model", "claude_models":"sonnet, opus,sonnet"}))
        .unwrap();
    let defaults = cli_defaults_from(&legacy, &settings, None);
    assert_eq!(defaults.model, "explicit-model");
    assert_eq!(defaults.models, ["explicit-model", "sonnet", "opus"]);
    let empty = cli_defaults_from(&cfg(), &NativeSettings::default(), None);
    assert_eq!(empty.model, "default");
    assert_eq!(empty.models, ["sonnet", "opus", "haiku"]);
    assert!(empty.reasoning_level.is_none());
    let mut native = profile();
    native["env"]["CLAUDE_CODE_EFFORT_LEVEL"] = "auto".into();
    write_settings(root.path(), "providers/auto.json", &native);
    assert!(read_provider(root.path(), "auto")
        .unwrap()
        .default_reasoning_level
        .is_none());
}

#[test]
fn config_directory_uses_claude_config_dir_then_home_and_does_not_change_process_environment() {
    assert_eq!(
        config_dir_from(Some("/custom/claude".into()), Some("/user".into())),
        Some(PathBuf::from("/custom/claude"))
    );
    assert_eq!(
        config_dir_from(Some("".into()), Some("/user".into())),
        Some(PathBuf::from("/user/.claude"))
    );
    assert_eq!(config_dir_from(None, None), None);
    assert_eq!(config_dir_from(None, Some("".into())), None);
}

#[test]
fn native_empty_model_clears_inherited_model_and_inline_credentials_stay_in_private_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let native = json!({"model":"saved-model", "env":{"ANTHROPIC_MODEL":"", "ANTHROPIC_API_KEY":"private-inline-key",
        "ANTHROPIC_BASE_URL":"https://claude.example"}});
    let path = write_settings(root.path(), "providers/inline.json", &native);
    let settings = NativeSettings::load(&path).unwrap();
    assert_eq!(
        settings.model(Some("old-process-model")).as_deref(),
        Some("saved-model")
    );
    let profile = read_provider(root.path(), "inline").unwrap();
    let mut command = Command::new("claude");
    let file = configure_command(&mut command, Some(&profile), None)
        .unwrap()
        .unwrap();
    let settings: Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
    assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "private-inline-key");
    assert_eq!(settings["apiKeyHelper"], "");
    assert!(!command
        .as_std()
        .get_args()
        .any(|arg| arg.to_string_lossy().contains("private-inline-key")));
    assert!(!serde_json::to_string(&profile)
        .unwrap()
        .contains("private-inline-key"));
}

#[test]
fn invalid_profile_does_not_hide_valid_ones_or_echo_secret_values() {
    let root = tempfile::tempdir().unwrap();
    write_settings(root.path(), "providers/good.json", &profile());
    write_settings(
        root.path(),
        "providers/bad.json",
        &json!({"env":"do-not-echo-credential"}),
    );
    write_settings(root.path(), "providers/cli.json", &profile());
    let listing = providers_at(Some(root.path())).unwrap();
    assert_eq!(listing.providers.len(), 1);
    assert_eq!(listing.warnings.len(), 2);
    assert!(!listing
        .warnings
        .join(" ")
        .contains("do-not-echo-credential"));
    for name in ["../good", "/tmp/good", ".", "..", "bad name", "cli"] {
        assert!(read_provider(root.path(), name).is_err());
    }
    for (field, value) in [
        ("model", json!("--help")),
        ("availableModels", json!(["invalid model"])),
        (
            "env",
            json!({"ANTHROPIC_BASE_URL":"https://user:password@example.com"}),
        ),
        ("env", json!({"KEY":12})),
    ] {
        let mut bad = profile();
        bad[field] = value;
        let path = write_settings(root.path(), "providers/invalid.json", &bad);
        assert!(NativeSettings::load(&path).is_err(), "{field}");
    }
    let large = root.path().join("providers/large.json");
    std::fs::write(&large, vec![b' '; MAX_SETTINGS_BYTES as usize + 1]).unwrap();
    assert!(NativeSettings::load(&large).is_err());
}

#[test]
fn native_credentials_helpers_and_other_private_fields_never_leave_public_catalog() {
    let root = tempfile::tempdir().unwrap();
    let mut native = profile();
    native["env"]["ANTHROPIC_AUTH_TOKEN"] = "private-api-token".into();
    native["env"]["INTERNAL_SETTING"] = "private-env-value".into();
    native["hooks"] =
        json!({"SessionStart":[{"hooks":[{"type":"command","command":"private-hook-command"}]}]});
    write_settings(root.path(), "providers/relay.json", &native);
    let public =
        serde_json::to_string(&providers_at(Some(root.path())).unwrap().providers).unwrap();
    for private in [
        "private-api-token",
        "private-env-value",
        "private-hook-command",
        "RELAY_TEST_KEY",
        "apiKeyHelper",
        "hooks",
    ] {
        assert!(!public.contains(private), "private field leaked: {private}");
    }
    assert!(public.contains("relay-sonnet"));
}

#[test]
fn invocation_preserves_native_settings_isolates_routing_and_overrides_effort_for_one_turn() {
    let root = tempfile::tempdir().unwrap();
    write_settings(root.path(), "providers/relay.json", &profile());
    let provider = read_provider(root.path(), "relay").unwrap();
    let mut command = Command::new("claude");
    let file = configure_command(&mut command, Some(&provider), Some("max"))
        .unwrap()
        .unwrap();
    let settings: Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
    assert_eq!(settings["apiKeyHelper"], "printenv RELAY_TEST_KEY");
    assert_eq!(settings["permissions"], profile()["permissions"]);
    assert_eq!(
        settings["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
        "relay-small"
    );
    assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "");
    assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "");
    assert_eq!(settings["env"]["CLAUDE_CODE_USE_BEDROCK"], "0");
    assert_eq!(settings["env"]["CLAUDE_CODE_EFFORT_LEVEL"], "max");
    assert_eq!(settings["effortLevel"], "high");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        file.as_file().metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(command.as_std().get_args().next().unwrap(), "--settings");
    let path = file.path().to_owned();
    drop(file);
    assert!(!path.exists());
    let mut command = Command::new("claude");
    let file = configure_command(&mut command, Some(&provider), None)
        .unwrap()
        .unwrap();
    let settings: Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
    assert_eq!(settings["env"]["CLAUDE_CODE_EFFORT_LEVEL"], "");
    assert_eq!(settings["effortLevel"], "high");
    assert_eq!(
        effective_model(&cfg(), Some(&provider), None).unwrap(),
        "relay-sonnet"
    );
    assert_eq!(
        effective_model(&cfg(), Some(&provider), Some(" manual ")).unwrap(),
        "manual"
    );
}

#[test]
fn default_cli_does_not_clear_credentials_or_add_settings_unless_effort_is_selected() {
    let mut command = Command::new("claude");
    assert!(configure_command(&mut command, None, None)
        .unwrap()
        .is_none());
    assert_eq!(command.as_std().get_envs().count(), 0);
    assert_eq!(command.as_std().get_args().count(), 0);
    let file = configure_command(&mut command, None, Some("low"))
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
    assert_eq!(value, json!({"env":{"CLAUDE_CODE_EFFORT_LEVEL":"low"}}));
    for level in REASONING_LEVELS {
        assert_eq!(
            normalize_reasoning_level(Some(format!(" {level} ")))
                .unwrap()
                .as_deref(),
            Some(*level)
        );
    }
    assert!(normalize_reasoning_level(Some("ultra".into())).is_err());
    assert!(normalize_reasoning_level(Some(" ".into()))
        .unwrap()
        .is_none());
}

#[test]
fn native_model_allowlist_does_not_imply_a_default_model() {
    let root = tempfile::tempdir().unwrap();
    write_settings(
        root.path(),
        "providers/allowlist.json",
        &json!({
            "availableModels":["expensive-model","small-model"],
            "env":{"ANTHROPIC_BASE_URL":"https://claude.example"}
        }),
    );
    let profile = read_provider(root.path(), "allowlist").unwrap();
    assert!(profile.default_model.is_none());
    assert_eq!(
        effective_model(&cfg(), Some(&profile), None).unwrap(),
        "default"
    );
}
