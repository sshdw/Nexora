//! Pure breakpoint resolver for the four-zone TUI (brief §5.5, plan §2).
//!
//! Every concrete threshold is a named constant here — no universal width is
//! assumed — and each breakpoint is covered by a `TestBackend` test. The
//! resolver returns a `Resolution` that the renderer consumes; it never looks
//! at application state, so a resize cannot lose content, scroll, draft or
//! focus (those live in `App`, not here).

/// Maximum readable width of the central feed on a wide terminal.
pub(crate) const FEED_MAX_COLS: u16 = 100;
/// At or above this width the feed is capped and centred.
pub(crate) const WIDE_MIN_COLS: u16 = 120;
/// At or above this width a single column is used (no right detail panel).
pub(crate) const MEDIUM_MIN_COLS: u16 = 80;
/// Below this width the terminal is too small to render the full UI.
pub(crate) const NARROW_MIN_COLS: u16 = 40;
/// Below this row count the terminal is too small to render the full UI.
pub(crate) const TOO_SMALL_ROWS: u16 = 10;
/// At or below this row count the UI switches to its compact form.
pub(crate) const SHORT_MAX_ROWS: u16 = 24;
/// Rows reserved for the status line / rule / key hints (one each).
pub(crate) const STATUS_ROWS: u16 = 1;
pub(crate) const RULE_ROWS: u16 = 1;
pub(crate) const HINTS_ROWS: u16 = 1;
/// Maximum composer text lines (top rule + hints live outside these).
pub(crate) const COMPOSER_TEXT_MAX: u16 = 4;
pub(crate) const COMPOSER_TEXT_MAX_SHORT: u16 = 2;
/// Minimum timeline rows.
pub(crate) const TIMELINE_MIN: u16 = 4;
pub(crate) const TIMELINE_MIN_SHORT: u16 = 3;
/// Fixed, honest message for a terminal below the minimum dimensions.
pub(crate) const TOO_SMALL_MESSAGE: &str =
    "Terminal too small — enlarge window or switch to compact mode";

/// Width class of the usable layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WidthClass {
    Wide,
    Medium,
    Narrow,
}

/// A resolvable layout: either the too-small message or concrete metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    TooSmall,
    Usable(Metrics),
}

/// Concrete geometry and visibility decisions for a usable terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Metrics {
    pub(crate) width_class: WidthClass,
    /// Compact vertical form (`rows <= SHORT_MAX_ROWS`).
    pub(crate) short: bool,
    /// Width of the central feed (capped/centred when wide).
    pub(crate) feed_width: u16,
    /// Left offset used to centre the feed.
    pub(crate) feed_x: u16,
    /// Composer text height.
    pub(crate) composer_lines: u16,
    /// Minimum rows for the timeline viewport.
    pub(crate) timeline_min: u16,
    /// Show optional metadata (git branch, autonomy label) in the status line.
    pub(crate) show_metadata: bool,
    /// Wrap long labels/paths instead of truncating them.
    pub(crate) wrap_paths: bool,
}

/// Resolve a terminal size into a layout. Pure and total for all `u16` inputs.
#[must_use]
pub(crate) fn resolve(cols: u16, rows: u16) -> Resolution {
    if cols < NARROW_MIN_COLS || rows < TOO_SMALL_ROWS {
        return Resolution::TooSmall;
    }
    let short = rows <= SHORT_MAX_ROWS;
    let width_class = if cols >= WIDE_MIN_COLS {
        WidthClass::Wide
    } else if cols >= MEDIUM_MIN_COLS {
        WidthClass::Medium
    } else {
        WidthClass::Narrow
    };
    let feed_width = if width_class == WidthClass::Wide {
        cols.min(FEED_MAX_COLS)
    } else {
        cols
    };
    let feed_x = (cols.saturating_sub(feed_width)) / 2;
    let narrow = width_class == WidthClass::Narrow;
    Resolution::Usable(Metrics {
        width_class,
        short,
        feed_width,
        feed_x,
        composer_lines: if short {
            COMPOSER_TEXT_MAX_SHORT
        } else {
            COMPOSER_TEXT_MAX
        },
        timeline_min: if short {
            TIMELINE_MIN_SHORT
        } else {
            TIMELINE_MIN
        },
        show_metadata: !narrow,
        wrap_paths: narrow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_contract() {
        assert_eq!(FEED_MAX_COLS, 100);
        assert_eq!(WIDE_MIN_COLS, 120);
        assert_eq!(MEDIUM_MIN_COLS, 80);
        assert_eq!(NARROW_MIN_COLS, 40);
        assert_eq!(TOO_SMALL_ROWS, 10);
        assert_eq!(SHORT_MAX_ROWS, 24);
        assert_eq!(STATUS_ROWS, 1);
        assert_eq!(RULE_ROWS, 1);
        assert_eq!(HINTS_ROWS, 1);
        assert_eq!(COMPOSER_TEXT_MAX, 4);
        assert_eq!(COMPOSER_TEXT_MAX_SHORT, 2);
        assert_eq!(TIMELINE_MIN, 4);
        assert_eq!(TIMELINE_MIN_SHORT, 3);
    }

    #[test]
    fn breakpoints_are_ordered() {
        assert!(matches!(resolve(39, 30), Resolution::TooSmall));
        assert!(matches!(resolve(100, 9), Resolution::TooSmall));
        let Resolution::Usable(m) = resolve(40, 10) else {
            panic!("exact minimum is usable")
        };
        assert_eq!(m.width_class, WidthClass::Narrow);
        let Resolution::Usable(m) = resolve(80, 25) else {
            panic!("medium")
        };
        assert_eq!(m.width_class, WidthClass::Medium);
        let Resolution::Usable(m) = resolve(120, 25) else {
            panic!("wide")
        };
        assert_eq!(m.width_class, WidthClass::Wide);
    }
}
