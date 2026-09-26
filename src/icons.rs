//! Vendor marks for the sidebar identity row.
//!
//! Prefer Private Use Area glyphs from the bundled Herdr Agent Icons Max
//! face (same marks herdr-radar ships). Terminals need the codepoint map that
//! [`crate::configure::font`] writes for Ghostty / kitty; without it the
//! PUA cells render as missing glyphs. For a terminal that cannot show them,
//! [`IconStyle::Letter`] swaps every mark for a two-letter ASCII badge.
use crate::model::Harness;

/// DeepSeek's whale in the same face (post-table glyph name `deepseek`).
const DEEPSEEK_MARK: &str = "\u{e1ad}";

/// How the vendor mark is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconStyle {
    /// One PUA glyph from Herdr Agent Icons Max.
    #[default]
    Glyph,
    /// Two ASCII letters, for a terminal without the icon face.
    Letter,
}

impl IconStyle {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_ICON_STYLE";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Glyph => "glyph",
            Self::Letter => "letter",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "glyph" | "icon" | "font" => Some(Self::Glyph),
            "letter" | "letters" | "ascii" | "text" => Some(Self::Letter),
            _ => None,
        }
    }

    /// The installed style: environment first (direct CLI runs), then the
    /// plugin preference `configure` wrote, then glyphs.
    pub fn installed() -> Self {
        std::env::var(Self::ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .or_else(|| crate::prefs::read(crate::prefs::ICON_STYLE).and_then(|v| Self::parse(&v)))
            .unwrap_or_default()
    }
}

/// One-cell mark for a harness.
pub fn for_harness(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "\u{e1a0}",
        Harness::Codex => "\u{e1a1}",
        Harness::Cursor => "\u{e1ab}",
    }
}

/// The harness mark alone, in a style: with logos on, a DS base still shows
/// Claude Code here and the whale moves to the model mark.
pub fn for_harness_styled(harness: Harness, style: IconStyle) -> &'static str {
    match style {
        IconStyle::Glyph => for_harness(harness),
        IconStyle::Letter => match harness {
            Harness::Claude => "CC",
            Harness::Codex => "CX",
            Harness::Cursor => "CU",
        },
    }
}

/// Does this model short name belong to a DeepSeek base?
///
/// The short name comes from the pane label's model segment (naming-spec §5).
/// A DS seat runs the Claude Code binary against a DeepSeek model, so the
/// harness alone cannot tell; the model segment can.
pub fn is_deepseek(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("ds") || model.contains("deepseek")
}

/// The mark a pane shows: the DeepSeek whale for a DS base, otherwise the
/// harness mark, in the installed style.
pub fn for_pane(harness: Harness, model: &str, style: IconStyle) -> &'static str {
    let deepseek = is_deepseek(model);
    match style {
        IconStyle::Glyph if deepseek => DEEPSEEK_MARK,
        IconStyle::Glyph => for_harness(harness),
        IconStyle::Letter if deepseek => "DS",
        IconStyle::Letter => match harness {
            Harness::Claude => "CC",
            Harness::Codex => "CX",
            Harness::Cursor => "CU",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AgentSelection;

    #[test]
    fn every_supported_harness_has_a_one_cell_mark() {
        for harness in AgentSelection::SUPPORTED
            .into_iter()
            .chain([Harness::Cursor])
        {
            let mark = for_harness(harness);
            assert_eq!(mark.chars().count(), 1, "{harness:?} -> {mark:?}");
        }
    }

    #[test]
    fn supported_harnesses_use_the_radar_pua() {
        assert_eq!(for_harness(Harness::Claude), "\u{e1a0}");
        assert_eq!(for_harness(Harness::Codex), "\u{e1a1}");
        assert_eq!(for_harness(Harness::Cursor), "\u{e1ab}");
    }

    #[test]
    fn a_deepseek_model_gets_the_whale_whatever_the_harness() {
        for model in [
            "ds-v4-1-flash",
            "dsflash",
            "deepseek-v4.1-flash",
            "DeepSeek",
        ] {
            assert!(is_deepseek(model), "{model}");
            assert_eq!(
                for_pane(Harness::Claude, model, IconStyle::Glyph),
                "\u{e1ad}"
            );
            assert_eq!(for_pane(Harness::Claude, model, IconStyle::Letter), "DS");
        }
        for model in ["", "opus-5-5", "fable-5-1", "glm-5-3", "gpt-6-astra"] {
            assert!(!is_deepseek(model), "{model}");
            assert_eq!(
                for_pane(Harness::Claude, model, IconStyle::Glyph),
                for_harness(Harness::Claude)
            );
        }
    }

    #[test]
    fn letter_badges_are_two_ascii_cells() {
        for harness in [Harness::Claude, Harness::Codex, Harness::Cursor] {
            let badge = for_pane(harness, "", IconStyle::Letter);
            assert_eq!(badge.len(), 2);
            assert!(badge.is_ascii());
        }
    }

    #[test]
    fn an_icon_style_round_trips() {
        for style in [IconStyle::Glyph, IconStyle::Letter] {
            assert_eq!(IconStyle::parse(style.as_str()), Some(style));
        }
        assert_eq!(IconStyle::parse("nope"), None);
    }
}
