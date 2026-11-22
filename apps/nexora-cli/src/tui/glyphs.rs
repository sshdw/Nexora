//! Unicode / ASCII glyph sets for the TUI (brief §4.4, plan §2).
//!
//! The preferred set uses simple line-drawing characters; the ASCII set is a
//! complete fallback for terminals that cannot display them. A glyph set is a
//! single value threaded through render functions so there is exactly one
//! definition of each symbol and the tests can assert both maps. No glyph is
//! the *only* carrier of meaning: every status also has a text label
//! (brief §4.3, §12.5).

/// A fixed set of symbols used by the timeline, separators and composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlyphSet {
    /// Vertical spine segment (`│` / `|`).
    pub(crate) spine: &'static str,
    /// Elbow that terminates a spine (`└─` / `+--`).
    pub(crate) elbow: &'static str,
    /// Directional arrow (`→` / `->`).
    pub(crate) arrow: &'static str,
    /// Success marker (`✓` / `[ok]`).
    pub(crate) check: &'static str,
    /// Horizontal rule unit (`─` / `-`).
    pub(crate) rule: &'static str,
    /// Composer prompt prefix (`›` / `>`).
    pub(crate) prefix: &'static str,
    /// Inline separator (`·` / `-`) used between metadata fields.
    pub(crate) dot: &'static str,
}

impl GlyphSet {
    /// The preferred Unicode set.
    #[must_use]
    pub(crate) const fn unicode() -> Self {
        Self {
            spine: "│",
            elbow: "└─",
            arrow: "→",
            check: "✓",
            rule: "─",
            prefix: "›",
            dot: "·",
        }
    }

    /// The ASCII-only fallback set (brief §4.4 map).
    #[must_use]
    pub(crate) const fn ascii() -> Self {
        Self {
            spine: "|",
            elbow: "+--",
            arrow: "->",
            check: "[ok]",
            rule: "-",
            prefix: ">",
            dot: "-",
        }
    }

    /// Build `count` copies of the horizontal rule unit (a one-row separator).
    #[must_use]
    pub(crate) fn rule_line(&self, count: u16) -> String {
        self.rule.repeat(usize::from(count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_maps_match_the_brief() {
        let uni = GlyphSet::unicode();
        let ascii = GlyphSet::ascii();
        assert_eq!((uni.spine, ascii.spine), ("│", "|"));
        assert_eq!((uni.elbow, ascii.elbow), ("└─", "+--"));
        assert_eq!((uni.arrow, ascii.arrow), ("→", "->"));
        assert_eq!((uni.check, ascii.check), ("✓", "[ok]"));
        assert_eq!(ascii.rule_line(4), "----");
    }
}
