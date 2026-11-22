//! Terminal capability resolution with honest fallbacks (brief §12, plan §2/§7).
//!
//! TTY detection itself lives in `main` and is the *only* gate for entering the
//! TUI — `--color always` never overrides it. This module decides, once the TUI
//! has been entered, whether to emit colour, which glyph map to use and whether
//! decorative motion is enabled. Every decision has a text/glyph equivalent, so
//! no information is lost in `NO_COLOR`, ASCII or `TERM=dumb` modes.

use crate::run::CapabilityArgs;

use super::glyphs::GlyphSet;

/// Inputs to the pure capability resolver, kept separate from the environment
/// so the precedence rules are directly unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapabilityInput<'a> {
    /// Raw global `--color` value (already parsed by `run::parse_color_mode`).
    pub(crate) color: &'a str,
    /// Global `--ascii` flag.
    pub(crate) ascii: bool,
    /// Raw global `--motion` value.
    pub(crate) motion: &'a str,
    /// `NO_COLOR` environment value, when present.
    pub(crate) no_color: Option<&'a str>,
    /// `TERM` environment value, when present.
    pub(crate) term: Option<&'a str>,
    /// `LANG` environment value, when present (used for the C-locale ASCII hint).
    pub(crate) lang: Option<&'a str>,
}

/// The resolved capabilities consumed by layout, rendering and the input loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// Emit styling. Monochrome only — grey/dim, never an accent hue.
    pub(crate) color: bool,
    /// Use the ASCII glyph map.
    pub(crate) ascii: bool,
    /// Decorative motion is disabled but all information is kept.
    pub(crate) motion_none: bool,
    /// The glyph map derived from `ascii`.
    pub(crate) glyphs: GlyphSet,
}

/// Whether a `NO_COLOR`-style variable is "set" per the de-facto standard: a
/// non-empty value disables colour (an explicitly empty value does not).
fn env_disables_color(value: Option<&str>) -> bool {
    value.is_some_and(|raw| !raw.is_empty())
}

/// Whether the environment requests ASCII-only output.
fn env_requests_ascii(term: Option<&str>, lang: Option<&str>) -> bool {
    let dumb = term.is_some_and(|value| value.eq_ignore_ascii_case("dumb"));
    let c_locale = lang.is_some_and(|value| {
        let value = value.trim();
        value == "C" || value == "POSIX" || value.starts_with("C.")
    });
    dumb || c_locale
}

/// Resolve the raw global flags plus environment into a single capability set.
///
/// Precedence (plan §2/§7): non-TTY is handled before this function; here
/// `--color always` wins over `NO_COLOR`/`TERM=dumb`, `--color never` and
/// `NO_COLOR` disable colour, and `--ascii`, `TERM=dumb` or a C locale select
/// the ASCII map. `--motion none` disables decoration only.
#[must_use]
pub(crate) fn resolve(input: CapabilityInput<'_>) -> Capabilities {
    let always = input.color == "always";
    let never = input.color == "never";
    let term_dumb = input
        .term
        .is_some_and(|value| value.eq_ignore_ascii_case("dumb"));
    let color = always || (!never && !env_disables_color(input.no_color) && !term_dumb);
    let ascii = input.ascii || env_requests_ascii(input.term, input.lang) || term_dumb;
    let motion_none = input.motion == "none";
    Capabilities {
        color,
        ascii,
        motion_none,
        glyphs: if ascii {
            GlyphSet::ascii()
        } else {
            GlyphSet::unicode()
        },
    }
}

/// Build the resolver input from the parsed global flags and the live process
/// environment. TTY-ness of each stream is checked by the caller in `main`.
#[must_use]
pub(crate) fn from_env(caps: &CapabilityArgs) -> Capabilities {
    let no_color = std::env::var("NO_COLOR").ok();
    let term = std::env::var("TERM").ok();
    let lang = std::env::var("LANG").ok();
    resolve(CapabilityInput {
        color: caps.color.as_str(),
        ascii: caps.ascii,
        motion: caps.motion.as_str(),
        no_color: no_color.as_deref(),
        term: term.as_deref(),
        lang: lang.as_deref(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CapabilityInput<'static> {
        CapabilityInput {
            color: "auto",
            ascii: false,
            motion: "quiet",
            no_color: None,
            term: Some("xterm-256color"),
            lang: Some("en_US.UTF-8"),
        }
    }

    #[test]
    fn default_auto_detection_uses_color_and_unicode() {
        let caps = resolve(input());
        assert!(caps.color && !caps.ascii && !caps.motion_none);
        assert_eq!(caps.glyphs, GlyphSet::unicode());
    }

    #[test]
    fn color_always_wins_over_no_color_and_dumb_term() {
        let mut i = input();
        i.color = "always";
        i.no_color = Some("1");
        i.term = Some("dumb");
        let caps = resolve(i);
        assert!(
            caps.color,
            "--color always must win over NO_COLOR/TERM=dumb"
        );
        assert!(caps.ascii, "TERM=dumb still forces ASCII glyphs");
    }

    #[test]
    fn no_color_never_and_dumb_disable_color() {
        let mut i = input();
        i.no_color = Some("1");
        assert!(!resolve(i).color);
        let mut i = input();
        i.color = "never";
        assert!(!resolve(i).color);
        let mut i = input();
        i.term = Some("dumb");
        assert!(!resolve(i).color);
        // An explicitly empty NO_COLOR does not disable colour.
        let mut i = input();
        i.no_color = Some("");
        assert!(resolve(i).color);
    }

    #[test]
    fn ascii_flag_dumb_term_and_c_locale_select_ascii() {
        let mut i = input();
        i.ascii = true;
        assert_eq!(resolve(i).glyphs, GlyphSet::ascii());
        let mut i = input();
        i.term = Some("dumb");
        assert_eq!(resolve(i).glyphs, GlyphSet::ascii());
        for locale in ["C", "POSIX"] {
            let mut i = input();
            i.lang = Some(locale);
            assert_eq!(resolve(i).glyphs, GlyphSet::ascii());
        }
    }

    #[test]
    fn motion_none_disables_decoration_only() {
        let mut i = input();
        i.motion = "none";
        let caps = resolve(i);
        assert!(caps.motion_none);
        assert!(
            caps.color && !caps.ascii,
            "motion must not change glyphs/color"
        );
    }
}
