//! The sidebar configuration these tests write is a string of TOML and
//! nothing else: no shell is started and no pane is read, so this file is
//! not gated to Unix the way `configure_round_trip.rs` is — that one drives
//! `sh` stubs, and these ran on Windows too until they were moved out of it
//! (Codex g1 S3).

mod common;

use common::*;
use herdr_agent_quota::cli::AgentOrder;
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

#[test]
fn the_attention_rows_lead_with_the_action_state_and_keep_every_quota_row() {
    let attention = managed_rows(&attention_rows(""));
    let grouped = managed_rows(&grouped_rows(""));
    // DESIGN-r3 head row: action state first, then the brand icon, the Space
    // and the Tab — the Space name has no `$quota_group` header to live in.
    assert_eq!(
        attention[0],
        [
            "state_text",
            "$quota_icon",
            "$quota_icon_working",
            "$quota_icon_done",
            "workspace",
            "tab"
        ]
    );
    assert_eq!(attention[1], ["agent"]);
    assert!(attention
        .iter()
        .flatten()
        .all(|token| token != "$quota_group"));
    // Everything below the identity rows is the grouped layout, row for row:
    // switching order must not drop or reorder a quota row.
    assert_eq!(grouped[0], ["$quota_group"]);
    assert_eq!(attention[1..], grouped[2..]);
}

#[test]
fn attention_rows_are_idempotent_and_switch_cleanly_both_ways() {
    let attention = attention_rows("");
    assert_eq!(attention_rows(&attention), attention);
    assert_eq!(grouped_rows(&attention), grouped_rows(""));
    assert_eq!(attention_rows(&grouped_rows("")), attention);
}

#[test]
fn attention_rows_uninstall_to_herdr_stock_rows() {
    let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
    let applied = attention_rows(original);
    assert!(applied.contains("state_text"), "{applied}");
    let removed = remove_quota_row(&applied).unwrap();
    assert!(!removed.contains("state_text"), "{removed}");
    assert!(!removed.contains("$quota"), "{removed}");
    assert_eq!(
        removed,
        "[ui]\n[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
}

#[test]
fn attention_rows_keep_a_user_row_through_install_switch_and_uninstall() {
    // A customised base layout is never taken over: the plugin's rows go to
    // `rows_by_agent` and the user's rows stay exactly as written.
    let original =
        "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"], [\"terminal_title\"]]\n";
    let applied = attention_rows(original);
    assert_eq!(
        managed_rows(&applied),
        [vec!["state_icon", "agent"], vec!["terminal_title"]]
    );
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let claude = document["ui"]["sidebar"]["agents"]["rows_by_agent"]["claude"]
        .as_array()
        .unwrap();
    let first = claude.get(0).unwrap().as_array().unwrap();
    assert_eq!(first.iter().find_map(configured_token), Some("state_text"));
    assert_eq!(attention_rows(&applied), applied);
    let switched = grouped_rows(&applied);
    assert!(!switched.contains("state_text"), "{switched}");
    assert!(switched.contains("terminal_title"), "{switched}");
    let removed = remove_quota_row(&applied).unwrap();
    assert!(removed.contains("terminal_title"), "{removed}");
    assert!(!removed.contains("state_text"), "{removed}");
}

#[test]
fn identity_rows_show_who_each_seat_is_and_no_usage() {
    use herdr_agent_quota::cli::AgentOrder;
    let rows = managed_rows(&identity_rows("", AgentOrder::Quota));
    assert_eq!(
        rows,
        [
            vec!["$quota_group"],
            // No state text: the harness icon's colour is the state (0927).
            // Each seat row opens with a pinned seat's band (0928), empty
            // on every other seat.
            vec![
                "$al_pin_head",
                "$quota_icon",
                "$quota_icon_working",
                "$quota_icon_done",
                "$al_space",
                "tab"
            ],
            // Second-row logos are the default (0927).
            vec![
                "$al_pin_bar",
                "$al_role",
                "$al_channel",
                "$al_model_mark",
                "$al_model"
            ],
            vec!["$al_pin_bar", "pane"],
        ]
    );
    let flat = rows.concat();
    for usage in ["5h", "week", "month", "context", "cache", "provider_model"] {
        assert!(
            flat.iter().all(|token| !token.contains(usage)),
            "{usage} leaked into identity rows: {flat:?}"
        );
    }
}

#[test]
fn identity_rows_are_the_same_in_both_orders_so_a_toggle_rewrites_nothing() {
    use herdr_agent_quota::cli::AgentOrder;
    assert_eq!(
        identity_rows("", AgentOrder::Quota),
        identity_rows("", AgentOrder::Attention)
    );
    let applied = identity_rows("", AgentOrder::Quota);
    assert!(applied.contains("key = \"prefix+shift+o\""), "{applied}");
    assert!(
        applied.contains("command = \"herdr-agent-quota.toggle-order\""),
        "{applied}"
    );
}

#[test]
fn identity_rows_are_idempotent_and_switch_cleanly_with_the_usage_rows() {
    use herdr_agent_quota::cli::AgentOrder;
    let identity = identity_rows("", AgentOrder::Quota);
    assert_eq!(identity_rows(&identity, AgentOrder::Quota), identity);
    assert_eq!(grouped_rows(&identity), grouped_rows(""));
    assert_eq!(
        identity_rows(&grouped_rows(""), AgentOrder::Quota),
        identity
    );
    assert_eq!(
        identity_rows(&attention_rows(""), AgentOrder::Quota),
        identity
    );
}

#[test]
fn identity_rows_uninstall_to_herdr_stock_rows() {
    use herdr_agent_quota::cli::AgentOrder;
    let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
    let applied = identity_rows(original, AgentOrder::Attention);
    let removed = remove_quota_row(&applied).unwrap();
    for leftover in ["$al_", "state_text", "\"pane\"", "toggle-order", "$quota"] {
        assert!(!removed.contains(leftover), "{leftover}: {removed}");
    }
    assert_eq!(
        removed,
        "[ui]\n[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
}

#[test]
fn every_candidate_look_writes_the_rows_it_promises() {
    use herdr_agent_quota::look::{GroupStyle, LogoLayout};
    // The defaults are plain headers with second-row logos.
    assert_eq!(
        managed_rows(&looked_rows("", GroupStyle::Plain, LogoLayout::SecondRow)),
        managed_rows(&identity_rows(
            "",
            herdr_agent_quota::cli::AgentOrder::Quota
        ))
    );
    // Logos off keeps round two's rows minus the state text.
    assert_eq!(
        managed_rows(&looked_rows("", GroupStyle::Plain, LogoLayout::None)),
        [
            vec!["$quota_group"],
            vec![
                "$al_pin_head",
                "$quota_icon",
                "$quota_icon_working",
                "$quota_icon_done",
                "$al_space",
                "tab"
            ],
            vec!["$al_pin_bar", "$al_role", "$al_model"],
            vec!["$al_pin_bar", "pane"],
        ]
    );
    // No identity look shows state text any more.
    for group in EVERY_GROUP {
        for logos in EVERY_LOGO {
            let rows = managed_rows(&looked_rows("", group, logos));
            assert!(
                !rows.concat().contains(&"state_text".to_string()),
                "{group:?} {logos:?}"
            );
        }
    }

    let rule = managed_rows(&looked_rows("", GroupStyle::Rule, LogoLayout::Inline));
    assert_eq!(rule[0], ["$quota_group"]);
    // Codex's two candidates have no gap row; only the count candidate does.
    assert_ne!(rule.last().unwrap(), &["$al_gap"]);
    let count = managed_rows(&looked_rows("", GroupStyle::Count, LogoLayout::None));
    assert_eq!(count.last().unwrap(), &["$al_gap"]);
    assert_eq!(
        rule[1],
        [
            "$al_pin_head",
            "$quota_icon",
            "$quota_icon_working",
            "$quota_icon_done",
            "$al_channel",
            "$al_model_mark",
            "$al_space",
            "tab"
        ]
    );
    assert!(looked_rows("", GroupStyle::Rule, LogoLayout::None).contains("fg = \"#89b4fa\""));
    // Only the harness mark carries status colour: channel and model marks
    // are the neutral grey in every layout.
    for logos in [LogoLayout::Inline, LogoLayout::SecondRow] {
        let rendered = looked_rows("", GroupStyle::Plain, logos);
        for token in ["$al_channel", "$al_model_mark"] {
            let styled = format!("token = \"{token}\", fg = \"#a6adc8\"");
            assert!(rendered.contains(&styled), "{logos:?} {token}: {rendered}");
        }
    }

    let second = managed_rows(&looked_rows("", GroupStyle::Bar, LogoLayout::SecondRow));
    assert_eq!(second[0], ["$quota_group"]);
    assert_eq!(
        second[2],
        [
            "$al_pin_bar",
            "$al_role",
            "$al_channel",
            "$al_model_mark",
            "$al_model"
        ]
    );
}

#[test]
fn every_candidate_look_is_idempotent_and_uninstalls_to_stock_rows() {
    let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
    for group in EVERY_GROUP {
        for logos in EVERY_LOGO {
            let applied = looked_rows(original, group, logos);
            assert_eq!(
                looked_rows(&applied, group, logos),
                applied,
                "{group:?}/{logos:?}"
            );
            // Switching to another look rewrites cleanly.
            assert_eq!(
                looked_rows(
                    &applied,
                    herdr_agent_quota::look::GroupStyle::Plain,
                    herdr_agent_quota::look::LogoLayout::None
                ),
                looked_rows(
                    original,
                    herdr_agent_quota::look::GroupStyle::Plain,
                    herdr_agent_quota::look::LogoLayout::None
                ),
                "{group:?}/{logos:?}"
            );
            let removed = remove_quota_row(&applied).unwrap();
            assert_eq!(
                removed,
                "[ui]\n[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n",
                "{group:?}/{logos:?}"
            );
        }
    }
}
