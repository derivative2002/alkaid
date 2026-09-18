use herdr_agent_quota::configure::{agy, claude};
use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn agy_setup_is_idempotent_and_restores_the_previous_statusline() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    fs::write(
        &settings,
        r#"{"theme":"dark","statusLine":{"type":"command","command":"echo old","refreshInterval":5}}"#,
    )
    .unwrap();

    agy::apply_at(&settings, &state, &executable).unwrap();
    let once = fs::read(&settings).unwrap();
    agy::apply_at(&settings, &state, &executable).unwrap();
    assert_eq!(fs::read(&settings).unwrap(), once);

    let installed: Value = serde_json::from_slice(&once).unwrap();
    assert_eq!(installed["theme"], "dark");
    assert_eq!(installed["statusLine"]["refreshInterval"], 5);
    let command = installed["statusLine"]["command"].as_str().unwrap();
    assert!(command.contains("agy-statusline"));
    assert!(command.contains(state.to_str().unwrap()));

    agy::uninstall_at(&settings, &state).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored["statusLine"]["command"], "echo old");
    assert_eq!(restored["statusLine"]["refreshInterval"], 5);
}

#[test]
fn old_plugin_wrappers_are_repaired_without_becoming_the_backup() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    fs::write(
        &settings,
        r#"{"statusLine":{"type":"command","command":"HERDR_PLUGIN_STATE_DIR='/wrong' '/old/herdr-agent-quota' claude-statusline"}}"#,
    )
    .unwrap();

    claude::apply_at(&settings, &state, &executable).unwrap();
    let installed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    let command = installed["statusLine"]["command"].as_str().unwrap();
    assert!(command.contains(state.to_str().unwrap()));
    assert!(!command.contains("/wrong"));

    claude::uninstall_at(&settings, &state).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert!(restored.get("statusLine").is_none());
}

#[test]
fn claude_statusline_refreshes_rate_limits_with_the_configured_interval() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    fs::write(
        &settings,
        r#"{"statusLine":{"type":"command","command":"echo old"}}"#,
    )
    .unwrap();

    claude::apply_at(&settings, &state, &executable).unwrap();
    let installed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(installed["statusLine"]["refreshInterval"], 60);

    claude::apply_at_with_refresh_interval(&settings, &state, &executable, 300).unwrap();
    let customized: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(customized["statusLine"]["refreshInterval"], 300);

    claude::uninstall_at(&settings, &state).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored["statusLine"]["command"], "echo old");
    assert!(restored["statusLine"].get("refreshInterval").is_none());
}

/// `configure` rewrites the whole settings file, so it must hand the user's
/// own keys back in the order they wrote them. Re-sorting a config file the
/// plugin does not own is an unasked-for edit, and it shows up in their diffs.
#[test]
fn claude_install_keeps_the_users_own_settings_key_order() {
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join("settings.json");
    let state = home.path().join("state");
    std::fs::write(
        &settings,
        r#"{"zzzLast":1,"model":"opus","alwaysThinkingEnabled":true,"aaaFirst":2}"#,
    )
    .unwrap();

    herdr_agent_quota::configure::claude::apply_at(
        &settings,
        &state,
        std::path::Path::new("/usr/local/bin/herdr-agent-quota"),
    )
    .unwrap();

    let written = std::fs::read_to_string(&settings).unwrap();
    // Order the keys by where they actually landed in the rewritten file.
    let mut order: Vec<(usize, &str)> = ["zzzLast", "model", "alwaysThinkingEnabled", "aaaFirst"]
        .into_iter()
        .map(|key| {
            let at = written
                .find(&format!("\"{key}\""))
                .unwrap_or_else(|| panic!("configure dropped {key}:\n{written}"));
            (at, key)
        })
        .collect();
    order.sort_unstable();
    let order: Vec<&str> = order.into_iter().map(|(_, key)| key).collect();
    assert_eq!(
        order,
        vec!["zzzLast", "model", "alwaysThinkingEnabled", "aaaFirst"],
        "configure re-sorted the user's settings file:\n{written}"
    );
    assert!(written.contains("claude-statusline"));
}

#[test]
fn claude_preserves_a_user_owned_refresh_interval_on_first_install() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    fs::write(
        &settings,
        r#"{"statusLine":{"type":"command","command":"echo old","refreshInterval":15}}"#,
    )
    .unwrap();

    claude::apply_at(&settings, &state, &executable).unwrap();
    let installed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(installed["statusLine"]["refreshInterval"], 15);

    claude::uninstall_at(&settings, &state).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored["statusLine"]["refreshInterval"], 15);
}

#[test]
fn repair_migrates_a_previous_backup_from_the_old_state_directory() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let old_state = directory.path().join("old-state");
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    fs::create_dir_all(&old_state).unwrap();
    fs::write(
        old_state.join("claude-statusline.original.json"),
        r#"{"type":"command","command":"echo user-owned"}"#,
    )
    .unwrap();
    fs::write(
        &settings,
        format!(
            r#"{{"statusLine":{{"type":"command","command":"HERDR_PLUGIN_STATE_DIR='{}' '/old/herdr-agent-quota' claude-statusline"}}}}"#,
            old_state.display()
        ),
    )
    .unwrap();

    claude::apply_at(&settings, &state, &executable).unwrap();
    claude::uninstall_at(&settings, &state).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored["statusLine"]["command"], "echo user-owned");
}

/// Negative control: the plugin backs up the statusLine of each settings file
/// it takes over. One machine has several Claude HOMEs, and a single
/// state-dir-wide backup name made the second `apply` keep the first file's
/// backup and overwrite nothing — but restore the wrong file on `uninstall`.
#[test]
fn two_settings_files_keep_independent_backups_and_restore_independently() {
    let directory = tempdir().unwrap();
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    let first = directory.path().join("home-a/settings.json");
    let second = directory.path().join("home-b/settings.json");
    fs::create_dir_all(first.parent().unwrap()).unwrap();
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    fs::write(
        &first,
        r#"{"statusLine":{"type":"command","command":"echo first"}}"#,
    )
    .unwrap();
    fs::write(
        &second,
        r#"{"statusLine":{"type":"command","command":"echo second"}}"#,
    )
    .unwrap();

    claude::apply_at(&first, &state, &executable).unwrap();
    claude::apply_at(&second, &state, &executable).unwrap();

    let backups: Vec<String> = fs::read_dir(&state)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("claude-statusline.original."))
        .collect();
    assert_eq!(
        backups.len(),
        2,
        "expected one backup per settings file, found {backups:?}"
    );

    // Uninstalling the first file restores only its own original.
    claude::uninstall_at(&first, &state).unwrap();
    let restored_first: Value = serde_json::from_slice(&fs::read(&first).unwrap()).unwrap();
    assert_eq!(restored_first["statusLine"]["command"], "echo first");

    // The second file is still under the plugin's wrapper.
    let still_installed: Value = serde_json::from_slice(&fs::read(&second).unwrap()).unwrap();
    assert!(still_installed["statusLine"]["command"]
        .as_str()
        .unwrap()
        .contains("claude-statusline"));
    assert!(
        second.exists(),
        "uninstalling one settings file disturbed another"
    );

    // Uninstalling the second restores its own original too.
    claude::uninstall_at(&second, &state).unwrap();
    let restored_second: Value = serde_json::from_slice(&fs::read(&second).unwrap()).unwrap();
    assert_eq!(restored_second["statusLine"]["command"], "echo second");
}

/// The index the contract asks for: settings path -> backup name -> takeover time.
#[test]
fn apply_records_a_backups_index_keyed_by_settings_path() {
    let directory = tempdir().unwrap();
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    let settings = directory.path().join("home/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        r#"{"statusLine":{"type":"command","command":"echo old"}}"#,
    )
    .unwrap();

    claude::apply_at(&settings, &state, &executable).unwrap();

    let index: Value =
        serde_json::from_slice(&fs::read(state.join("claude-statusline.backups.json")).unwrap())
            .unwrap();
    let entry = index
        .get(settings.to_str().unwrap())
        .expect("settings path missing from the backups index");
    let backup = entry["backup"].as_str().unwrap();
    assert!(backup.starts_with("claude-statusline.original."));
    assert!(state.join(backup).exists(), "index names a missing backup");
    assert!(entry["taken_over_at_unix"].as_u64().unwrap() > 0);
}

/// Without a backup for *this* settings file, uninstall must explain rather
/// than delete a statusLine it never owned.
#[test]
fn uninstall_without_a_backup_leaves_the_statusline_untouched() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let state = directory.path().join("empty-state");
    let command =
        "HERDR_PLUGIN_STATE_DIR='/elsewhere' '/usr/bin/herdr-agent-quota' claude-statusline";
    fs::write(
        &settings,
        format!(r#"{{"statusLine":{{"type":"command","command":"{command}"}}}}"#),
    )
    .unwrap();

    claude::uninstall_at(&settings, &state).unwrap();

    let after: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(
        after["statusLine"]["command"], command,
        "uninstall deleted a statusLine it had no backup for"
    );
}

/// The old single-name backup predates per-settings naming; the next apply has
/// to adopt it under the settings-specific name instead of orphaning it.
#[test]
fn a_legacy_single_name_backup_is_migrated_to_the_settings_specific_name() {
    let directory = tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let state = directory.path().join("state");
    let executable = directory.path().join("herdr-agent-quota");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("claude-statusline.original.json"),
        r#"{"type":"command","command":"echo legacy-original"}"#,
    )
    .unwrap();
    fs::write(
        &settings,
        format!(
            r#"{{"statusLine":{{"type":"command","command":"HERDR_PLUGIN_STATE_DIR='{}' '/old/herdr-agent-quota' claude-statusline"}}}}"#,
            state.display()
        ),
    )
    .unwrap();

    claude::apply_at(&settings, &state, &executable).unwrap();

    let migrated: Vec<String> = fs::read_dir(&state)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with("claude-statusline.original.")
                && name != "claude-statusline.original.json"
        })
        .collect();
    assert_eq!(
        migrated.len(),
        1,
        "the legacy backup was not migrated: {migrated:?}"
    );
    assert!(
        !state.join("claude-statusline.original.json").exists(),
        "the legacy backup was left behind"
    );
    assert!(
        state.join("claude-statusline.backups.json").exists(),
        "no backups index was written"
    );

    claude::uninstall_at(&settings, &state).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored["statusLine"]["command"], "echo legacy-original");
}

#[test]
fn direct_configuration_write_refuses_an_ambiguous_cache_directory() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .args(["configure", "--apply"])
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must run through Herdr"));
}
