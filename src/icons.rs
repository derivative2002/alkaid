//! Vendor marks for the sidebar identity row.
//!
//! Prefer Private Use Area glyphs from the bundled Herdr Agent Icons Max
//! face (same marks herdr-radar ships). Terminals need the codepoint map that
//! [`crate::configure::font`] writes for Ghostty / kitty; without it the
//! PUA cells render as missing glyphs.

use crate::model::Harness;

/// One-cell mark for a harness.
pub fn for_harness(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "\u{e1a0}",
        Harness::Codex => "\u{e1a1}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AgentSelection;

    #[test]
    fn every_supported_harness_has_a_one_cell_mark() {
        for harness in AgentSelection::SUPPORTED {
            let mark = for_harness(harness);
            assert_eq!(mark.chars().count(), 1, "{harness:?} -> {mark:?}");
        }
    }

    #[test]
    fn supported_harnesses_use_the_radar_pua() {
        assert_eq!(for_harness(Harness::Claude), "\u{e1a0}");
        assert_eq!(for_harness(Harness::Codex), "\u{e1a1}");
    }
}
