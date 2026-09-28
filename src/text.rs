//! Text that is safe to hand to a terminal, and how wide it is once there.
//!
//! Every string this program shows comes from somewhere it does not control:
//! a USB descriptor, a monitor's EDID, a window title, an extension manifest.
//! Any of them can carry an escape sequence, and a terminal obeys escape
//! sequences. [`Text`] is the one way into a report row, and building one
//! strips everything a terminal would interpret.

use std::fmt;
use std::ops::Deref;

/// Stands in for a character that must not reach the terminal.
const REPLACEMENT: char = '\u{fffd}';

/// A string with no control characters and no bidirectional overrides.
///
/// The field is private, so [`Text::new`] is the only constructor and every
/// `Text` in existence has already been cleaned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Text(String);

impl Text {
    pub(crate) fn new(raw: impl Into<String>) -> Text {
        let raw = raw.into();

        // The common case is already clean; do not rebuild it.
        if raw.chars().all(|c| !needs_replacing(c)) {
            return Text(raw);
        }

        let clean = raw
            .chars()
            .map(|c| match c {
                '\t' | '\n' | '\r' => ' ',
                c if needs_replacing(c) => REPLACEMENT,
                c => c,
            })
            .collect();

        Text(clean)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Text {
    /// `pad`, not `write_str`, so `{label:<20}` still pads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

impl From<&str> for Text {
    fn from(raw: &str) -> Text {
        Text::new(raw)
    }
}

impl From<String> for Text {
    fn from(raw: String) -> Text {
        Text::new(raw)
    }
}

impl From<&String> for Text {
    fn from(raw: &String) -> Text {
        Text::new(raw.as_str())
    }
}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// C0 and C1 controls (ESC, CSI, BEL, NUL...) drive the terminal, and the
/// bidi overrides reorder what the reader sees relative to what is there.
fn needs_replacing(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// How many terminal cells one character occupies.
///
/// Not a full `wcwidth`: combining marks and zero-width joiners take no
/// cell, East Asian wide and fullwidth characters and emoji take two, and
/// everything else takes one. That is enough to keep a CJK window title or
/// an emoji hostname from pushing the frame past the right edge.
pub(crate) fn char_width(c: char) -> usize {
    let cp = u32::from(c);

    if is_zero_width(cp) {
        0
    } else if is_wide(cp) {
        2
    } else {
        1
    }
}

/// Cells taken by a whole string.
pub(crate) fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn is_zero_width(cp: u32) -> bool {
    matches!(
        cp,
        0x0300..=0x036f
            | 0x0483..=0x0489
            | 0x0591..=0x05bd
            | 0x0610..=0x061a
            | 0x064b..=0x065f
            | 0x0e31
            | 0x0e34..=0x0e3a
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x200b..=0x200f
            | 0x20d0..=0x20ff
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
            | 0xfeff
            | 0xe0100..=0xe01ef
    )
}

fn is_wide(cp: u32) -> bool {
    matches!(
        cp,
        0x1100..=0x115f
            | 0x231a..=0x231b
            | 0x2329..=0x232a
            | 0x23e9..=0x23ec
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x26a1
            | 0x26aa..=0x26ab
            | 0x26bd..=0x26be
            | 0x26c4..=0x26c5
            | 0x26d4
            | 0x26ea
            | 0x26f2..=0x26f5
            | 0x26fa
            | 0x26fd
            | 0x2705
            | 0x270a..=0x270b
            | 0x2728
            | 0x274c
            | 0x2753..=0x2755
            | 0x2795..=0x2797
            | 0x2b1b..=0x2b1c
            | 0x2e80..=0x303e
            | 0x3041..=0x33ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xa000..=0xa4cf
            | 0xa960..=0xa97f
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f004
            | 0x1f0cf
            | 0x1f18e
            | 0x1f191..=0x1f19a
            | 0x1f200..=0x1f251
            | 0x1f300..=0x1f64f
            | 0x1f680..=0x1f6ff
            | 0x1f7e0..=0x1f7eb
            | 0x1f90c..=0x1f9ff
            | 0x1fa70..=0x1faff
            | 0x20000..=0x3fffd
    )
}

/// The longest prefix of `text` that fits in `cells`, with an ellipsis when
/// anything was cut.
pub(crate) fn clip(text: &str, cells: usize) -> String {
    if width(text) <= cells {
        return text.to_string();
    }
    if cells == 0 {
        // No room for the ellipsis either.
        return String::new();
    }

    // Reserve one cell for the ellipsis.
    let budget = cells - 1;
    let mut used = 0;
    let mut out = String::new();

    for c in text.chars() {
        let w = char_width(c);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }

    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- sanitising ------------------------------------------------------

    #[test]
    fn clean_text_passes_through_untouched() {
        assert_eq!(Text::new("Intel Arc · 日本語 🎉"), "Intel Arc · 日本語 🎉");
    }

    #[test]
    fn escape_sequences_cannot_reach_the_terminal() {
        // A USB device naming itself "\x1b]0;pwned\x07" would otherwise retitle
        // the terminal, and a CSI could clear or rewrite the screen.
        let text = Text::new("evil\x1b]0;pwned\x07\x1b[2J\u{9b}31m");

        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.starts_with("evil"));
    }

    #[test]
    fn whitespace_controls_become_spaces() {
        assert_eq!(Text::new("a\tb\nc\rd"), "a b c d");
    }

    #[test]
    fn bidi_overrides_are_replaced() {
        let text = Text::new("abc\u{202e}fed");

        assert!(!text.contains('\u{202e}'));
    }

    #[test]
    fn display_honours_width_and_alignment() {
        assert_eq!(format!("[{:<5}]", Text::new("ab")), "[ab   ]");
        assert_eq!(format!("[{:>5}]", Text::new("ab")), "[   ab]");
    }

    #[test]
    fn a_nul_is_replaced_rather_than_kept() {
        assert_eq!(Text::new("L\0i"), "L\u{fffd}i");
    }

    // ---- width -----------------------------------------------------------

    #[test]
    fn ascii_box_drawing_and_bars_are_one_cell() {
        assert_eq!(width("abc─├█░▸…·°"), 11);
    }

    #[test]
    fn cjk_and_emoji_are_two_cells() {
        assert_eq!(width("日本"), 4);
        assert_eq!(width("🎉"), 2);
        assert_eq!(width("한"), 2);
    }

    #[test]
    fn combining_marks_take_no_cell() {
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("a\u{200b}b"), 2);
    }

    // ---- clip ------------------------------------------------------------

    #[test]
    fn clip_leaves_short_text_alone() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("", 0), "");
    }

    #[test]
    fn clip_cuts_with_an_ellipsis_that_counts_toward_the_width() {
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(width(&clip("abcdef", 4)), 4);
    }

    #[test]
    fn clip_never_splits_a_wide_character_across_the_limit() {
        // Four cells: "日" (2) + "…" (1) fits, a second "日" would not.
        let clipped = clip("日本語", 4);

        assert_eq!(clipped, "日…");
        assert!(width(&clipped) <= 4);
    }

    #[test]
    fn clip_counts_cells_not_bytes() {
        assert_eq!(clip("ééééé", 3), "éé…");
        assert_eq!(clip("→→→", 3), "→→→");
    }

    #[test]
    fn clip_to_zero_is_empty() {
        assert_eq!(clip("abc", 0), "");
    }

    #[test]
    fn clip_to_one_cell_is_just_the_ellipsis() {
        assert_eq!(clip("abc", 1), "…");
    }
}
