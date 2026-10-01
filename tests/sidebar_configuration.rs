//! The sidebar configuration these tests write is a string of TOML and
//! nothing else: no shell is started and no pane is read, so this file is
//! not gated to Unix the way `configure_round_trip.rs` is — that one drives
//! `sh` stubs, and these ran on Windows too until they were moved out of it
//! (Codex g1 S3).

use herdr_agent_quota::configure::herdr::{add_quota_row, remove_quota_row};

#[test]
fn sidebar_configuration_is_idempotent_and_removes_plugin_rows() {
    let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("key = \"prefix+shift+r\""));
    assert!(applied.contains("type = \"plugin_action\""));
    assert!(applied.contains("command = \"herdr-agent-quota.refresh\""));
    assert!(applied.contains("key = \"prefix+shift+q\""));
    assert!(applied.contains("command = \"herdr-agent-quota.open-settings\""));
    assert!(applied.contains("agent_panel_sort = \"spaces\" # herdr-agent-quota"));
    assert_eq!(add_quota_row(&applied).unwrap(), applied);
    assert_eq!(
        remove_quota_row(&applied).unwrap(),
        "[ui]\n[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
}

#[test]
fn sidebar_configuration_preserves_a_conflicting_refresh_key() {
    let original = concat!(
        "[[keys.command]]\n",
        "key = \"prefix+shift+r\"\n",
        "type = \"shell\"\n",
        "command = \"echo user-owned\"\n",
        "description = \"user refresh\"\n\n",
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert_eq!(applied.matches("key = \"prefix+shift+r\"").count(), 1);
    assert!(applied.contains("command = \"echo user-owned\""));
    assert!(!applied.contains("command = \"herdr-agent-quota.refresh\""));
    assert_eq!(
        remove_quota_row(&applied).unwrap(),
        "[[keys.command]]\nkey = \"prefix+shift+r\"\ntype = \"shell\"\ncommand = \"echo user-owned\"\ndescription = \"user refresh\"\n\n[ui]\n\n[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
}

#[test]
fn sidebar_configuration_preserves_a_conflicting_settings_key() {
    let original = concat!(
        "[[keys.command]]\n",
        "key = \"prefix+shift+q\"\n",
        "type = \"shell\"\n",
        "command = \"echo user-owned\"\n",
        "description = \"user settings\"\n\n",
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert_eq!(applied.matches("key = \"prefix+shift+q\"").count(), 1);
    assert!(applied.contains("command = \"echo user-owned\""));
    assert!(!applied.contains("command = \"herdr-agent-quota.open-settings\""));
    assert!(applied.contains("command = \"herdr-agent-quota.refresh\""));
    let removed = remove_quota_row(&applied).unwrap();
    assert!(removed.contains("command = \"echo user-owned\""));
    assert!(!removed.contains("herdr-agent-quota.refresh"));
}

#[test]
fn default_herdr_rows_become_plane_provider_usage_and_topic_lines() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("$quota_provider_model"));
    assert!(applied.contains("bold = true"));
    for base in ["$quota_5h", "$quota_week", "$quota_week_inline"] {
        for band in ["normal", "warning", "danger", "unknown"] {
            assert!(applied.contains(&format!("{base}_{band}")), "{base}_{band}");
        }
        // `Severity` has no caution band, so no token can ever fill this row.
        assert!(
            !applied.contains(&format!("{base}_caution")),
            "{base}_caution is unreachable and must not be configured"
        );
    }
    assert!(!applied.contains("[\"$quota_summary\"]"));
    assert!(applied.contains("$quota_topic"));
    assert!(applied.contains("$quota_context"));
    assert!(applied.contains("$quota_provider_model"));
    assert!(!applied.contains("fg = \"#969eae\""));
    assert!(applied.find("$quota_provider_model").unwrap() < applied.find("$quota_topic").unwrap());
    assert!(applied.contains("$quota_cache"));
    assert!(applied.contains("$quota_cache_ttl"));
    assert!(applied.contains("$quota_cache_state"));
    assert!(!applied.contains("$quota_5h_label"));
    assert!(!applied.contains("$quota_5h_eta"));
    assert!(!applied.contains("fg = \"#c8cdd6\""));
    assert!(applied.contains("row_gap = 1 # herdr-agent-quota"));
    assert!(applied.find("$quota_topic").unwrap() < applied.find("$quota_5h_normal").unwrap());
    assert!(applied.contains("fg = \"#82d978\""));
    assert!(applied.contains("fg = \"#e4b957\""));
    assert!(!applied.contains("fg = \"#c6d768\""));
    assert!(!applied.contains("fg = \"#e2bd58\""));
    assert!(applied.contains("fg = \"#f16f7e\""));
    assert!(!applied.contains("fg = \"#eceef2\""));
    assert!(!applied.contains("selection_bg"));
    assert!(!applied.contains("active_row_bg"));
    assert!(!applied.contains("[ui.sidebar.agents.rows_by_agent]"));
    assert!(applied.contains("$quota_icon"));
    assert!(applied.contains("$quota_icon_working"));
    assert!(applied.contains("$quota_icon_done"));
    assert!(applied.contains("fg = \"#e9e9f0\""));
    assert!(applied.contains("fg = \"#f9e2af\""));
    assert!(applied.contains("fg = \"#94e2d5\""));
}

#[test]
fn non_semantic_text_inherits_the_active_herdr_theme() {
    let applied =
        add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"tab\", \"agent\"]]\n")
            .unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let rows = document["ui"]["sidebar"]["agents"]["rows"]
        .as_array()
        .unwrap();
    let inherited = [
        "$quota_topic",
        "$quota_cache",
        "$quota_cache_ttl",
        "$quota_5h_unknown",
        "$quota_week_unknown",
        "$quota_week_inline_unknown",
    ];

    for token in inherited {
        let style = rows
            .iter()
            .filter_map(toml_edit::Value::as_array)
            .flat_map(|row| row.iter())
            .find(|item| configured_token(item) == Some(token))
            .and_then(toml_edit::Value::as_inline_table)
            .unwrap_or_else(|| panic!("missing styled token {token}"));
        assert!(
            !style.contains_key("fg"),
            "{token} must inherit Herdr's foreground: {style}"
        );
    }
}

#[test]
fn context_precedes_the_window_rows_and_identity_keeps_tab_and_agent() {
    let applied =
        add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let agents = &document["ui"]["sidebar"]["agents"];
    let rows = agents["rows"].as_array().unwrap();
    let index_of = |token: &str| {
        rows.iter()
            .position(|row| row_contains_token(row, token))
            .unwrap_or_else(|| panic!("missing {token}:\n{applied}"))
    };
    let context = index_of("$quota_context_normal");
    let five = index_of("$quota_5h_normal");
    let week = index_of("$quota_week_normal");
    let month = index_of("$quota_month_normal");
    assert_eq!(context + 1, five, "context precedes 5h");
    assert_eq!(five + 1, week, "5h precedes 7d");
    assert_eq!(week + 1, month, "7d precedes 30d");
    assert_eq!(month + 1, rows.len(), "30d is the last quota row");

    let identity = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_icon"))
        .and_then(toml_edit::Value::as_array)
        .unwrap();
    assert!(!identity
        .iter()
        .any(|item| item.as_str() == Some("state_icon")));
    assert!(identity
        .iter()
        .any(|item| configured_token(item) == Some("tab")));
    assert!(identity
        .iter()
        .any(|item| configured_token(item) == Some("$quota_icon_working")));
    assert!(identity
        .iter()
        .any(|item| configured_token(item) == Some("$quota_icon_done")));
    assert!(
        rows.iter().any(agent_only_row),
        "the native agent row is kept:\n{applied}"
    );
    for token in ["$quota_icon", "$quota_provider_model"] {
        assert_eq!(
            token_fg(&applied, token).as_deref(),
            Some("#e9e9f0"),
            "{token}"
        );
    }
    assert!(agents.get("rows_by_agent").is_none(), "{applied}");
}

#[test]
fn provider_model_is_its_own_row_and_week_inline_folds_onto_the_limits_row() {
    let applied =
        add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let agents = &document["ui"]["sidebar"]["agents"];
    let rows = agents["rows"].as_array().unwrap();
    let identity_row = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_provider_model"))
        .unwrap();
    let identity_tokens = identity_row.as_array().unwrap();
    // Provider/model now stands alone rather than sharing the brand-icon row.
    assert_eq!(identity_tokens.len(), 1, "{identity_row}");
    assert!(!identity_tokens.iter().any(|item| {
        matches!(
            configured_token(item),
            Some("$quota_provider") | Some("$quota_model")
        ) || item.as_str() == Some("state_icon")
    }));

    // Herdr's `tab` rides the brand-icon row.
    let icon_row = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_icon"))
        .and_then(toml_edit::Value::as_array)
        .unwrap();
    assert!(icon_row
        .iter()
        .any(|item| configured_token(item) == Some("tab")));
    assert!(icon_row
        .iter()
        .any(|item| configured_token(item) == Some("$quota_icon_working")));
    assert!(icon_row
        .iter()
        .any(|item| configured_token(item) == Some("$quota_icon_done")));

    // Week fold lives on the shared 7d rows — there are no per-agent copies.
    assert!(agents.get("rows_by_agent").is_none(), "{applied}");
    let week_row = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_week_normal"))
        .and_then(toml_edit::Value::as_array)
        .unwrap();
    assert!(
        week_row
            .iter()
            .any(|item| configured_token(item) == Some("$quota_week_inline_normal")),
        "shared rows should fold 7d onto the 7d limits row"
    );
    assert!(
        week_row
            .iter()
            .all(|item| configured_token(item) != Some("$quota_5h_normal")),
        "5h must not sit on the 7d row"
    );
    assert!(
        !rows.iter().any(|row| {
            row_contains_token(row, "$quota_5h_normal")
                && row_contains_token(row, "$quota_week_normal")
        }),
        "5h and 7d each own a row now"
    );
}

#[test]
fn existing_provider_and_model_tokens_are_migrated_to_one_identity_token() {
    let applied = add_quota_row(
        r#"[ui.sidebar.agents]
rows = [["state_icon", "tab", { token = "$quota_provider" }, { token = "$quota_model" }]]
"#,
    )
    .unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let rows = document["ui"]["sidebar"]["agents"]["rows"]
        .as_array()
        .unwrap();
    let identity_row = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_provider_model"))
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        identity_row
            .iter()
            .filter(|item| configured_token(item) == Some("$quota_provider_model"))
            .count(),
        1
    );
    assert!(!identity_row.iter().any(|item| {
        matches!(
            configured_token(item),
            Some("$quota_provider") | Some("$quota_model")
        )
    }));
}

fn configured_token(value: &toml_edit::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.as_inline_table()?.get("token")?.as_str())
}

fn row_contains_token(row: &toml_edit::Value, token: &str) -> bool {
    row.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| configured_token(item) == Some(token))
    })
}

/// A row that carries only Herdr's native `agent` token.
fn agent_only_row(row: &toml_edit::Value) -> bool {
    row.as_array().is_some_and(|items| {
        items.len() == 1
            && items
                .iter()
                .next()
                .is_some_and(|item| configured_token(item) == Some("agent"))
    })
}

/// The `fg` a named token carries in `ui.sidebar.agents.rows`, if any.
fn token_fg(sidebar: &str, token: &str) -> Option<String> {
    let document = sidebar.parse::<toml_edit::DocumentMut>().unwrap();
    for row in document["ui"]["sidebar"]["agents"]["rows"].as_array()? {
        let Some(items) = row.as_array() else {
            continue;
        };
        for item in items {
            if configured_token(item) == Some(token) {
                return item
                    .as_inline_table()
                    .and_then(|table| table.get("fg"))
                    .and_then(toml_edit::Value::as_str)
                    .map(str::to_string);
            }
        }
    }
    None
}

#[test]
fn configuration_removes_obsolete_session_summary_rows() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"], [\"$quota_topic\"], [\"$quota_session\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("$quota_context"));
    assert!(!applied.contains("$quota_session"));
}

#[test]
fn sidebar_configuration_preserves_an_explicit_row_gap() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "row_gap = 2\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("row_gap = 2"));
    assert!(!applied.contains("row_gap = 1"));
    assert_eq!(
        remove_quota_row(&applied).unwrap(),
        "[ui]\n[ui.sidebar.agents]\nrow_gap = 2\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
}

#[test]
fn sidebar_configuration_migrates_the_plugin_owned_gap_to_separated_panes() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "row_gap = 0 # herdr-agent-quota\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("row_gap = 1 # herdr-agent-quota"));
    assert!(!applied.contains("row_gap = 0"));
}
