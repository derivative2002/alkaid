//! How Space headers and the three logo kinds (harness, channel, model) look.
//!
//! Both are owner-facing candidates (0927): the defaults keep the round-two
//! sidebar exactly as it was, and each alternative is one preference away.
//! Everything here is plain text for Herdr tokens — Herdr 0.9.1 styles a
//! token only by `fg`, `bold` and `dim`, so a "rule" or a "bar" is made of
//! characters, and a gap between Spaces is a row whose only token is a
//! zero-width space (Herdr keeps such a row as a blank line).

use crate::icons::IconStyle;

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
    /// Harness mark only; a DeepSeek base swaps it for the whale (round two).
    #[default]
    None,
    /// First row: harness · channel · model marks, then state and Tab.
    Inline,
    /// Harness mark on the first row; channel and model marks beside the
    /// role on the second.
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

/// What the first-row harness mark shows for a Claude Code seat on a
/// DeepSeek base. The owner listed "dsh" among the harnesses; Codex reads
/// the harness as the launcher (Claude Code) and the whale as the model.
/// Both are drawn for the owner to choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum DsMark {
    /// The launcher's mark: a DS-base Claude Code seat keeps the Claude
    /// mark, and the whale moves to the model mark (Codex reading).
    #[default]
    Launcher,
    /// The whale replaces the harness mark, as in round two ("dsh" as its
    /// own harness). Without a logo layout this is always the behaviour.
    Base,
}

impl DsMark {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_DS_MARK";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Base => "base",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "launcher" | "harness" => Some(Self::Launcher),
            "base" | "dsh" | "whale" => Some(Self::Base),
            _ => None,
        }
    }

    pub fn installed() -> Self {
        std::env::var(Self::ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .or_else(|| crate::prefs::read(crate::prefs::DS_MARK).and_then(|v| Self::parse(&v)))
            .unwrap_or_default()
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
    let m = model.trim().to_ascii_lowercase();
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

/// A model family's mark. Glyphs are from the bundled Herdr Agent Icons Max
/// face (U+E1A0–E1B6). GLM has no glyph there and no font tooling was
/// available to add one, so it is a text badge in every style.
pub fn model_mark(family: ModelFamily, style: IconStyle) -> &'static str {
    match (family, style) {
        (ModelFamily::Glm, _) => "GLM",
        (ModelFamily::Claude, IconStyle::Glyph) => "\u{e1a0}",
        (ModelFamily::Gpt, IconStyle::Glyph) => "\u{e1af}",
        (ModelFamily::DeepSeek, IconStyle::Glyph) => "\u{e1ad}",
        (ModelFamily::Gemini, IconStyle::Glyph) => "\u{e1ae}",
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

/// A channel's mark. Glyphs are Nerd Fonts Material Design icons carried by
/// Maple Mono NF CN, the owner's Ghostty font, so the Mac needs no new
/// codepoint map: U+F0AE2 (four-point star, for the Dubhe star) and U+F0791
/// (check decagram, "official").
pub fn channel_mark(channel: Channel, style: IconStyle) -> &'static str {
    match (channel, style) {
        (Channel::Dubhe, IconStyle::Glyph) => "\u{f0ae2}",
        (Channel::Official, IconStyle::Glyph) => "\u{f0791}",
        (Channel::Dubhe, IconStyle::Letter) => "天枢",
        (Channel::Official, IconStyle::Letter) => "官方",
    }
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
    fn marks_have_a_letter_fallback_and_glm_is_always_text() {
        assert_eq!(model_mark(ModelFamily::Glm, IconStyle::Glyph), "GLM");
        assert_eq!(
            model_mark(ModelFamily::DeepSeek, IconStyle::Glyph),
            "\u{e1ad}"
        );
        assert_eq!(model_mark(ModelFamily::DeepSeek, IconStyle::Letter), "DS");
        assert_eq!(channel_mark(Channel::Dubhe, IconStyle::Letter), "天枢");
        assert_eq!(
            channel_mark(Channel::Official, IconStyle::Glyph),
            "\u{f0791}"
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
        for mark in [DsMark::Launcher, DsMark::Base] {
            assert_eq!(DsMark::parse(mark.as_str()), Some(mark));
        }
        assert!(GroupStyle::Count.counts_status());
        assert!(GroupStyle::Count.gaps());
        assert!(!GroupStyle::Bar.gaps());
        assert!(!GroupStyle::Rule.gaps());
    }
}
