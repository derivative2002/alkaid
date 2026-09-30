//! How Space headers and the three logo kinds (harness, channel, model) look.
//!
//! Both are owner-facing candidates (0927): the defaults keep the round-two
//! sidebar exactly as it was, and each alternative is one preference away.
//! Everything here is plain text for Herdr tokens — Herdr 0.9.1 styles a
//! token only by `fg`, `bold` and `dim`, so a "rule" or a "bar" is made of
//! characters, and a gap between Spaces is a row whose only token is a
//! zero-width space (Herdr keeps such a row as a blank line).

use crate::icons::IconStyle;
use crate::model::Harness;

/// How a Space header is drawn in the grouped panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum GroupStyle {
    /// The Space name alone (round two).
    #[default]
    Plain,
    /// Codex candidate B: a fixed-length `━━━━ alioth ━━━━━` in the accent
    /// colour.
    Rule,
    /// `alioth · 4 席 · 1 卡住 · 2 已回`, with a blank line closing each
    /// Space.
    Count,
    /// Codex candidate A: `▌ alioth 4席` in the accent colour — seats only,
    /// no blocked count until a synchronous update can be shown.
    Bar,
}

impl GroupStyle {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_GROUP_STYLE";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Rule => "rule",
            Self::Count => "count",
            Self::Bar => "bar",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "plain" | "default" => Some(Self::Plain),
            "rule" | "line" => Some(Self::Rule),
            "count" | "counts" => Some(Self::Count),
            "bar" => Some(Self::Bar),
            _ => None,
        }
    }

    pub fn installed() -> Self {
        std::env::var(Self::ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .or_else(|| crate::prefs::read(crate::prefs::GROUP_STYLE).and_then(|v| Self::parse(&v)))
            .unwrap_or_default()
    }

    /// Does this style close each Space with a blank row? Only `count`: it
    /// costs a write on whichever pane is last in its Space, and Codex's two
    /// candidates stay without it.
    pub fn gaps(self) -> bool {
        self == Self::Count
    }

    /// Does the header text change with the members' status? Only `count`,
    /// and that is its cost: a member's status change rewrites the head
    /// pane's header (one metadata write on the head, per change).
    pub fn counts_status(self) -> bool {
        self == Self::Count
    }
}

/// Seat and status counts for one Space, from one inventory read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpaceCounts {
    pub seats: usize,
    pub blocked: usize,
    pub done: usize,
}

/// The `rule` header's lead and tail, fixed rather than sized to the
/// sidebar: a `--remote` client's width lives on the client, and Herdr 0.9.1
/// cannot stretch a token to the row.
const RULE_LEAD: &str = "━━━━";
const RULE_TAIL: &str = "━━━━━";

/// The header text for a Space.
pub fn header(style: GroupStyle, name: &str, counts: SpaceCounts) -> String {
    match style {
        GroupStyle::Plain => name.to_string(),
        GroupStyle::Rule => format!("{RULE_LEAD} {name} {RULE_TAIL}"),
        GroupStyle::Count => {
            let mut text = format!("{name} · {} 席", counts.seats);
            if counts.blocked > 0 {
                text.push_str(&format!(" · {} 卡住", counts.blocked));
            }
            if counts.done > 0 {
                text.push_str(&format!(" · {} 已回", counts.done));
            }
            text
        }
        GroupStyle::Bar => format!("▌ {name} {}席", counts.seats),
    }
}

/// Terminal columns: CJK wide/fullwidth characters take two.
#[cfg(test)]
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| {
            let c = c as u32;
            let wide = (0x1100..=0x115F).contains(&c)
                || (0x2E80..=0xA4CF).contains(&c)
                || (0xAC00..=0xD7A3).contains(&c)
                || (0xF900..=0xFAFF).contains(&c)
                || (0xFE30..=0xFE4F).contains(&c)
                || (0xFF00..=0xFF60).contains(&c)
                || (0xFFE0..=0xFFE6).contains(&c);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// The blank row closing a Space: a zero-width space, which Herdr renders as
/// an empty line instead of collapsing the row.
pub const GAP: &str = "\u{200b}";

/// Where the three logo kinds sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum LogoLayout {
    /// Harness mark only; no channel or model mark.
    None,
    /// First row: harness · channel · model marks, then Tab.
    Inline,
    /// Harness mark on the first row; channel and model marks beside the
    /// role on the second (owner 0927: the default).
    #[default]
    SecondRow,
}

impl LogoLayout {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_LOGO_LAYOUT";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Inline => "inline",
            Self::SecondRow => "second-row",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(Self::None),
            "inline" | "first-row" => Some(Self::Inline),
            "second-row" | "second" | "row2" => Some(Self::SecondRow),
            _ => None,
        }
    }

    pub fn installed() -> Self {
        std::env::var(Self::ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .or_else(|| crate::prefs::read(crate::prefs::LOGO_LAYOUT).and_then(|v| Self::parse(&v)))
            .unwrap_or_default()
    }

    pub fn shows_logos(self) -> bool {
        self != Self::None
    }
}

/// The model family a model name belongs to: a session's own model (Claude
/// Code `display_name`/id, Codex config) or a label's model segment.
/// Abbreviations seen on the 0927 seats (`op5`, `ds41-flash`, `ds-v41`,
/// `g6a`, `gpt6-astra`) are covered; anything else — including `codex-home`
/// and `cursor`, which are not model names — is no family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Claude,
    Gpt,
    DeepSeek,
    Glm,
    Gemini,
    Qwen,
    Kimi,
    Grok,
}

pub fn model_family(model: &str) -> Option<ModelFamily> {
    let m = strip_channel_prefix(&model.trim().to_ascii_lowercase()).to_string();
    // `codex-*` names the Codex product, not an OpenAI model (Dubhe brand-map).
    if m.starts_with("codex") {
        return None;
    }
    let starts = |p: &str| m.starts_with(p);
    let second_is_digit = m.chars().nth(1).is_some_and(|c| c.is_ascii_digit());
    if crate::icons::is_deepseek(&m) {
        Some(ModelFamily::DeepSeek)
    } else if starts("glm") {
        Some(ModelFamily::Glm)
    } else if starts("gemini") {
        Some(ModelFamily::Gemini)
    } else if starts("qwen") {
        Some(ModelFamily::Qwen)
    } else if starts("kimi") {
        Some(ModelFamily::Kimi)
    } else if starts("grok") {
        Some(ModelFamily::Grok)
    } else if starts("opus")
        || starts("sonnet")
        || starts("haiku")
        || starts("fable")
        || starts("claude")
        || (starts("op") && m.chars().nth(2).is_some_and(|c| c.is_ascii_digit()))
    {
        Some(ModelFamily::Claude)
    } else if starts("gpt") || (starts("g") && second_is_digit) {
        Some(ModelFamily::Gpt)
    } else {
        None
    }
}

/// Channel prefixes Dubhe puts in front of a model or alias name. They say
/// which route a request takes, not whose model it is, so they come off
/// before the family is matched (Dubhe `brand-map.ts` `CHANNEL_PREFIXES`).
const CHANNEL_PREFIXES: [&str; 7] = [
    "super-relay-",
    "mh-",
    "ag-",
    "or-",
    "ap-",
    "iquest-",
    "ark-",
];

fn strip_channel_prefix(name: &str) -> &str {
    CHANNEL_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(name)
}

/// The short model name the second row shows, taken from the model the
/// pane's own session reported: `Opus 5.5 (1M context)` → `opus-5.5`.
/// Empty when there is nothing to show; never taken from the pane label.
pub fn short_model_name(model: &str) -> String {
    let name = model
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    strip_channel_prefix(&name)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// naming-spec v2.8 model short names, matched on the lowercase model id by
/// prefix, longest prefix first. Real ids carry a leg or vendor suffix
/// (`deepseek-v4.1-flash-dspark-greedy-1m-b200`, `claude-opus-5-qiniu`), so
/// an exact table would miss them.
const SEAT_SHORT_NAMES: [(&str, &str); 16] = [
    ("claude-opus-5-5", "op55"),
    ("claude-opus-5", "op5"),
    ("claude-sonnet-5-5", "son55"),
    ("claude-fable-5-1", "f51"),
    ("deepseek-v4.1-flash", "ds41"),
    ("deepseek-v4-1-flash", "ds41"),
    ("ds-v41-flash", "ds41"),
    ("gpt-6.1-sol", "sol61"),
    ("gpt-6-1-sol", "sol61"),
    ("gpt-6-sol", "sol"),
    ("gpt-6-astra", "astra"),
    ("glm-5.3", "glm53"),
    ("gemini-3.8-flash", "gm38"),
    ("gemini-3-8-flash", "gm38"),
    // v2.8 (规范写者 0930): Cursor 席的实际底座。
    ("grok-4.7", "grok47"),
    ("grok-4-7", "grok47"),
];

/// The short name a seat row shows for the model its own session last used.
/// An id the table does not know is still evidence: it is shown compacted
/// (lowercase, no `claude-` prefix, no dots — `grok-4.7` → `grok47`), never as
/// the unknown mark.
pub fn seat_short_name(model: &str) -> String {
    let id = strip_channel_prefix(&model.trim().to_ascii_lowercase()).to_string();
    let mut table = SEAT_SHORT_NAMES;
    table.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
    if let Some((_, short)) = table.iter().find(|(prefix, _)| {
        // The prefix must end the version: `claude-opus-5` takes
        // `claude-opus-5-qiniu` and `claude-opus-5[1m]`, never
        // `claude-opus-5-6` or `claude-opus-50`.
        id.strip_prefix(prefix).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with('[')
                || rest
                    .strip_prefix('-')
                    .is_some_and(|tail| !tail.starts_with(|c: char| c.is_ascii_digit()))
        })
    }) {
        return short.to_string();
    }
    // Unknown id: drop the vendor prefix and a trailing `-YYYYMMDD`, then
    // dots and any dash before a digit (`grok-4.7` → `grok47`).
    let id = id.strip_prefix("claude-").unwrap_or(&id);
    let id = match id.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => id,
    };
    let chars = id.chars().filter(|c| *c != '.').collect::<Vec<_>>();
    chars
        .iter()
        .enumerate()
        .filter(|(i, c)| **c != '-' || !chars.get(i + 1).is_some_and(|next| next.is_ascii_digit()))
        .map(|(_, c)| *c)
        .collect()
}

/// A model family's mark. Glyphs for the five families the seats use are
/// the @lobehub/icons marks Dubhe's brand-map uses, added to the
/// Herdr Agent Icons Alkaid face at U+E1BA–E1BE (GLM is the z.ai Z knocked
/// out of a tile); Qwen, Kimi and Grok keep the original face's glyphs.
pub fn model_mark(family: ModelFamily, style: IconStyle) -> &'static str {
    match (family, style) {
        (ModelFamily::Glm, IconStyle::Glyph) => "\u{e1bb}",
        (ModelFamily::Glm, IconStyle::Letter) => "GLM",
        (ModelFamily::Claude, IconStyle::Glyph) => "\u{e1ba}",
        (ModelFamily::Gpt, IconStyle::Glyph) => "\u{e1bd}",
        (ModelFamily::DeepSeek, IconStyle::Glyph) => "\u{e1bc}",
        (ModelFamily::Gemini, IconStyle::Glyph) => "\u{e1be}",
        (ModelFamily::Qwen, IconStyle::Glyph) => "\u{e1b0}",
        (ModelFamily::Kimi, IconStyle::Glyph) => "\u{e1a6}",
        (ModelFamily::Grok, IconStyle::Glyph) => "\u{e1b1}",
        (ModelFamily::Claude, IconStyle::Letter) => "CL",
        (ModelFamily::Gpt, IconStyle::Letter) => "GPT",
        (ModelFamily::DeepSeek, IconStyle::Letter) => "DS",
        (ModelFamily::Gemini, IconStyle::Letter) => "GM",
        (ModelFamily::Qwen, IconStyle::Letter) => "QW",
        (ModelFamily::Kimi, IconStyle::Letter) => "KM",
        (ModelFamily::Grok, IconStyle::Letter) => "GK",
    }
}

/// Which way a seat reaches its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The 天枢 (Dubhe) gateway on :8686.
    Dubhe,
    /// The vendor's own endpoint through the bound egress.
    Official,
}

/// A channel's mark (owner 0927: a logo, not text). Dubhe is its own Big
/// Dipper logo, reduced to a one-cell dipper at U+E1BF of the Herdr Agent
/// Icons Alkaid face. Official is the vendor whose own endpoint serves the
/// seat, as in Dubhe's brand-map provider table: Anthropic (U+E1C6) for
/// Claude Code, OpenAI (U+E1BD, the same mark as the GPT model family) for
/// Codex. The letter style is the no-font fallback and keeps 官.
pub fn channel_mark(channel: Channel, harness: Harness, style: IconStyle) -> Option<&'static str> {
    Some(match (channel, style) {
        (Channel::Dubhe, IconStyle::Glyph) => "\u{e1bf}",
        (Channel::Dubhe, IconStyle::Letter) => "天枢",
        (Channel::Official, IconStyle::Letter) => "官",
        (Channel::Official, IconStyle::Glyph) => match harness {
            Harness::Claude => "\u{e1c6}",
            Harness::Codex => "\u{e1bd}",
            Harness::Cursor => return None,
        },
    })
}

/// A base URL reduced to a comparable route: lower-case `host[:port]` plus
/// the path, with scheme, userinfo, query and fragment dropped. Only this is
/// ever compared or kept; the raw URL is not stored or published.
pub fn route_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() {
        return None;
    }
    Some(format!("{host}{}", path.trim_end_matches('/')).to_ascii_lowercase())
}

/// The channel a route names: port 8686 or a `dubhe` gateway path is the
/// 天枢 gateway; the Anthropic API host is official. Anything else is not
/// guessed.
pub fn channel_of_route(route: &str) -> Option<Channel> {
    let (authority, path) = match route.find('/') {
        Some(slash) => route.split_at(slash),
        None => (route, ""),
    };
    let port = authority.rsplit_once(':').map(|(_, port)| port);
    let path_names_dubhe = path
        .split('/')
        .any(|part| part == "8686" || part.contains("dubhe"));
    if port == Some("8686") || path_names_dubhe {
        Some(Channel::Dubhe)
    } else if authority == "api.anthropic.com" {
        Some(Channel::Official)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_on_the_0927_seats_gets_the_right_family() {
        for (model, family) in [
            ("opus-5-5", Some(ModelFamily::Claude)),
            ("op5", Some(ModelFamily::Claude)),
            ("fable-5-1", Some(ModelFamily::Claude)),
            ("ds-v41-flash", Some(ModelFamily::DeepSeek)),
            ("ds41-flash", Some(ModelFamily::DeepSeek)),
            ("ds-v41", Some(ModelFamily::DeepSeek)),
            ("glm-5-3", Some(ModelFamily::Glm)),
            ("gpt-6-sol", Some(ModelFamily::Gpt)),
            ("gpt-6-astra", Some(ModelFamily::Gpt)),
            ("gpt6-astra", Some(ModelFamily::Gpt)),
            ("g6a", Some(ModelFamily::Gpt)),
            ("codex-home", None),
            ("cursor", None),
            ("", None),
        ] {
            assert_eq!(model_family(model), family, "{model}");
        }
    }

    #[test]
    fn the_short_model_name_and_family_follow_dubhe_naming() {
        for (reported, short) in [
            ("Opus 5.5 (1M context)", "opus-5.5"),
            ("Fable 5.1", "fable-5.1"),
            ("deepseek-v4.1-flash", "deepseek-v4.1-flash"),
            ("iquest-m1-coder", "m1-coder"),
            ("ark-glm-5.3", "glm-5.3"),
            ("  ", ""),
        ] {
            assert_eq!(short_model_name(reported), short, "{reported}");
        }
        // A channel prefix is a route, not a brand; `codex-*` is the Codex
        // product, not an OpenAI model (Dubhe brand-map).
        assert_eq!(model_family("super-relay-glm5.2"), Some(ModelFamily::Glm));
        assert_eq!(model_family("ark-kimi-k3"), Some(ModelFamily::Kimi));
        assert_eq!(model_family("codex-mini"), None);
        assert_eq!(model_family("gpt-5-codex"), Some(ModelFamily::Gpt));
        assert_eq!(model_family("glm-5.3-fp8-1m"), Some(ModelFamily::Glm));
    }

    #[test]
    fn a_route_keeps_host_and_path_and_drops_credentials() {
        assert_eq!(
            route_of("https://user:secret@10.8.8.8:8686/v1?key=abc#x").as_deref(),
            Some("10.8.8.8:8686/v1")
        );
        assert_eq!(
            route_of("http://127.0.0.1:8686/").as_deref(),
            Some("127.0.0.1:8686")
        );
        assert_eq!(
            route_of("HTTPS://API.Anthropic.com").as_deref(),
            Some("api.anthropic.com")
        );
        assert_eq!(route_of("  "), None);
        assert_eq!(route_of("https://user@/v1"), None);
    }

    #[test]
    fn channels_follow_the_route() {
        for url in [
            "http://127.0.0.1:8686",
            "https://10.8.8.8:8686/v1",
            "https://siflow-changliu.siflow.cn/siflow/changliu/skyinfer/yjiao/dubhe-llm-astergateway/v1/8686",
        ] {
            let route = route_of(url).unwrap();
            assert_eq!(channel_of_route(&route), Some(Channel::Dubhe), "{url}");
        }
        assert_eq!(
            channel_of_route("api.anthropic.com"),
            Some(Channel::Official)
        );
        assert_eq!(channel_of_route("api.anthropic.com.evil.net"), None);
        assert_eq!(channel_of_route("example.net/v1"), None);
        assert_eq!(channel_of_route("example.net:18686/v1"), None);
    }

    #[test]
    fn headers_fit_the_narrow_sidebar() {
        let counts = SpaceCounts {
            seats: 4,
            blocked: 1,
            done: 2,
        };
        assert_eq!(header(GroupStyle::Plain, "alioth", counts), "alioth");
        assert_eq!(
            header(GroupStyle::Count, "alioth", counts),
            "alioth · 4 席 · 1 卡住 · 2 已回"
        );
        assert_eq!(
            header(
                GroupStyle::Count,
                "lab",
                SpaceCounts {
                    seats: 2,
                    ..Default::default()
                }
            ),
            "lab · 2 席"
        );
        assert_eq!(header(GroupStyle::Bar, "天枢", counts), "▌ 天枢 4席");
        assert_eq!(header(GroupStyle::Rule, "玉衡", counts), "━━━━ 玉衡 ━━━━━");
        // The longest live Space name still fits the 36-column sidebar.
        for style in [GroupStyle::Rule, GroupStyle::Bar] {
            let text = header(style, "siflow-inference-lab", counts);
            assert!(display_width(&text) <= 34, "{text}");
        }
    }

    #[test]
    fn marks_have_a_letter_fallback_and_glm_is_the_zai_glyph() {
        assert_eq!(model_mark(ModelFamily::Glm, IconStyle::Glyph), "\u{e1bb}");
        assert_eq!(model_mark(ModelFamily::Glm, IconStyle::Letter), "GLM");
        assert_eq!(
            model_mark(ModelFamily::DeepSeek, IconStyle::Glyph),
            "\u{e1bc}"
        );
        assert_eq!(model_mark(ModelFamily::DeepSeek, IconStyle::Letter), "DS");
        assert_eq!(
            channel_mark(Channel::Dubhe, Harness::Claude, IconStyle::Letter),
            Some("天枢")
        );
        assert_eq!(
            channel_mark(Channel::Official, Harness::Claude, IconStyle::Letter),
            Some("官")
        );
    }

    #[test]
    fn seat_short_names_follow_the_naming_spec_table_by_family_prefix() {
        for (id, short) in [
            ("claude-opus-5-5", "op55"),
            ("claude-opus-5-5[1m]", "op55"),
            ("claude-opus-5", "op5"),
            ("claude-opus-5-qiniu", "op5"),
            ("claude-sonnet-5-5", "son55"),
            ("claude-fable-5-1", "f51"),
            ("deepseek-v4.1-flash", "ds41"),
            ("deepseek-v4.1-flash-dspark-greedy-1m-b200", "ds41"),
            ("gpt-6-astra", "astra"),
            ("gpt-6-sol", "sol"),
            ("gpt-6.1-sol", "sol61"),
            ("glm-5.3", "glm53"),
            ("gemini-3.8-flash", "gm38"),
            ("grok-4.7", "grok47"),
            ("grok-4-7", "grok47"),
        ] {
            assert_eq!(seat_short_name(id), short, "{id}");
        }
        // Read but not in the table: compacted, never the unknown mark.
        for (id, short) in [
            ("grok-4.8", "grok48"),
            ("claude-haiku-4-5-20251001", "haiku45"),
            // A later version is not taken by an earlier prefix.
            ("claude-opus-5-6", "opus56"),
        ] {
            assert_eq!(seat_short_name(id), short, "{id}");
        }
    }

    #[test]
    fn the_official_channel_is_the_serving_vendor_logo() {
        let official = |harness| channel_mark(Channel::Official, harness, IconStyle::Glyph);
        assert_eq!(official(Harness::Claude), Some("\u{e1c6}"));
        assert_eq!(official(Harness::Codex), Some("\u{e1bd}"));
        assert_eq!(official(Harness::Cursor), None);
        assert_eq!(
            channel_mark(Channel::Dubhe, Harness::Claude, IconStyle::Glyph),
            Some("\u{e1bf}")
        );
    }

    #[test]
    fn styles_and_layouts_round_trip() {
        for style in [
            GroupStyle::Plain,
            GroupStyle::Rule,
            GroupStyle::Count,
            GroupStyle::Bar,
        ] {
            assert_eq!(GroupStyle::parse(style.as_str()), Some(style));
        }
        for layout in [LogoLayout::None, LogoLayout::Inline, LogoLayout::SecondRow] {
            assert_eq!(LogoLayout::parse(layout.as_str()), Some(layout));
        }
        assert!(GroupStyle::Count.counts_status());
        assert!(GroupStyle::Count.gaps());
        assert!(!GroupStyle::Bar.gaps());
        assert!(!GroupStyle::Rule.gaps());
    }
}
