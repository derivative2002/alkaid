//! Helpers both sidebar test binaries use: readers of the written
//! configuration. A module rather than a copy in each file, because both
//! need them (Codex g1o S3 moved the platform-independent tests into their
//! own file, and these are used on both sides).

#![allow(dead_code)]

use herdr_agent_quota::cli::{AgentOrder, AgentSelection};
use herdr_agent_quota::look::{GroupStyle, LogoLayout};

pub fn identity_rows(input: &str, order: herdr_agent_quota::cli::AgentOrder) -> String {
    herdr_agent_quota::configure::herdr::add_sidebar_rows(
        input,
        &AgentSelection::SUPPORTED,
        herdr_agent_quota::cli::SidebarLayout::Gauges,
        herdr_agent_quota::cli::SidebarRowGap::default(),
        herdr_agent_quota::cli::FieldSet::all(),
        herdr_agent_quota::cli::BrandColors::On,
        order,
        herdr_agent_quota::cli::SidebarRows::Identity,
    )
    .unwrap()
}

pub fn looked_rows(
    input: &str,
    group: herdr_agent_quota::look::GroupStyle,
    logos: herdr_agent_quota::look::LogoLayout,
) -> String {
    herdr_agent_quota::configure::herdr::add_sidebar_rows_look(
        input,
        &AgentSelection::SUPPORTED,
        herdr_agent_quota::cli::SidebarLayout::Gauges,
        herdr_agent_quota::cli::SidebarRowGap::default(),
        herdr_agent_quota::cli::FieldSet::all(),
        herdr_agent_quota::cli::BrandColors::On,
        herdr_agent_quota::cli::AgentOrder::Quota,
        herdr_agent_quota::cli::SidebarRows::Identity,
        herdr_agent_quota::configure::herdr::RowLook { group, logos },
    )
    .unwrap()
}

pub fn attention_rows(input: &str) -> String {
    herdr_agent_quota::configure::herdr::add_quota_row_ordered(
        input,
        &AgentSelection::SUPPORTED,
        herdr_agent_quota::cli::SidebarLayout::Gauges,
        herdr_agent_quota::cli::SidebarRowGap::default(),
        herdr_agent_quota::cli::FieldSet::all(),
        herdr_agent_quota::cli::BrandColors::On,
        herdr_agent_quota::cli::AgentOrder::Attention,
    )
    .unwrap()
}

pub fn grouped_rows(input: &str) -> String {
    herdr_agent_quota::configure::herdr::add_quota_row_with(
        input,
        &AgentSelection::SUPPORTED,
        herdr_agent_quota::cli::SidebarLayout::Gauges,
        herdr_agent_quota::cli::SidebarRowGap::default(),
        herdr_agent_quota::cli::FieldSet::all(),
        herdr_agent_quota::cli::BrandColors::On,
    )
    .unwrap()
}

pub fn managed_rows(sidebar: &str) -> Vec<Vec<String>> {
    let document = sidebar.parse::<toml_edit::DocumentMut>().unwrap();
    document["ui"]["sidebar"]["agents"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .filter_map(configured_token)
                .map(str::to_string)
                .collect()
        })
        .collect()
}

pub fn configured_token(value: &toml_edit::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.as_inline_table()?.get("token")?.as_str())
}

pub const EVERY_GROUP: [herdr_agent_quota::look::GroupStyle; 4] = [
    herdr_agent_quota::look::GroupStyle::Plain,
    herdr_agent_quota::look::GroupStyle::Rule,
    herdr_agent_quota::look::GroupStyle::Count,
    herdr_agent_quota::look::GroupStyle::Bar,
];

pub const EVERY_LOGO: [herdr_agent_quota::look::LogoLayout; 3] = [
    herdr_agent_quota::look::LogoLayout::None,
    herdr_agent_quota::look::LogoLayout::Inline,
    herdr_agent_quota::look::LogoLayout::SecondRow,
];
