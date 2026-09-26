//! The active Omarchy theme, parsed from its `colors.toml`.
//!
//! That file is a flat list of `key = "value"` pairs, so a full TOML parser
//! would be overkill; this reads the handful of keys the UI needs.

use crate::term::Color;

#[derive(Clone)]
pub struct Palette {
    pub accent: Color,
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,
    pub heading: Color,
    pub red: Color,
    pub yellow: Color,
    pub green: Color,
    pub selection: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            accent: Color::Rgb(0x88, 0xc0, 0xd0),
            background: Color::Rgb(0x1c, 0x20, 0x2b),
            foreground: Color::Rgb(0xd8, 0xde, 0xe9),
            muted: Color::Rgb(0x6b, 0x74, 0x88),
            heading: Color::Rgb(0x88, 0xc0, 0xd0),
            red: Color::Rgb(0xbf, 0x61, 0x6a),
            yellow: Color::Rgb(0xeb, 0xcb, 0x8b),
            green: Color::Rgb(0xa3, 0xbe, 0x8c),
            selection: Color::Rgb(0x43, 0x4c, 0x5e),
        }
    }
}

impl Palette {
    /// Read the theme Omarchy says is current, falling back to a dark default.
    pub fn from_omarchy() -> Palette {
        let mut palette = Palette::default();

        let theme_name = std::fs::read_to_string(
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".local/state/omarchy/current/theme.name"),
        )
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
        if theme_name.is_empty() {
            return palette;
        }

        let path = format!("/usr/share/omarchy/themes/{theme_name}/colors.toml");
        let Ok(text) = std::fs::read_to_string(path) else {
            return palette;
        };
        let table = parse_pairs(&text);
        apply_table(&mut palette, &table);
        palette
    }
}

/// Copy the keys we care about out of a parsed `colors.toml`, leaving anything
/// missing (or malformed) at the default.
fn apply_table(palette: &mut Palette, table: &[(String, String)]) {
    let lookup = |keys: &[&str]| -> Option<Color> {
        keys.iter()
            .find_map(|k| table.iter().find(|(name, _)| name == k))
            .and_then(|(_, v)| hex(v))
    };

    if let Some(c) = lookup(&["accent"]) {
        palette.accent = c;
    }
    if let Some(c) = lookup(&["background"]) {
        palette.background = c;
    }
    if let Some(c) = lookup(&["foreground"]) {
        palette.foreground = c;
    }
    if let Some(c) = lookup(&["muted", "dark_foreground"]) {
        palette.muted = c;
    }
    if let Some(c) = lookup(&["cyan"]) {
        palette.heading = c;
    }
    if let Some(c) = lookup(&["selection"]) {
        palette.selection = c;
    }
    if let Some(c) = lookup(&["red"]) {
        palette.red = c;
    }
    if let Some(c) = lookup(&["yellow"]) {
        palette.yellow = c;
    }
    if let Some(c) = lookup(&["green"]) {
        palette.green = c;
    }
}

/// `key = "value"` pairs, in file order.
fn parse_pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        out.push((key.trim().to_string(), parse_value(value)));
    }
    out
}

/// A single TOML value.
///
/// A quoted value ends at its closing quote, so a trailing comment cannot leak
/// into the colour: without this, `accent = "#88c0d0" # note` yielded
/// `#88c0d0" # note` and `hex` silently took the first six characters.
fn parse_value(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    // A bare value keeps any leading '#', since that is how an unquoted hex
    // colour looks. Only whitespace-preceded '#' starts a comment.
    match raw.find(" #") {
        Some(i) => raw[..i].trim().to_string(),
        None => raw.to_string(),
    }
}

fn hex(value: &str) -> Option<Color> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Cool while low, warm when hot, so a glance at a bar tells you the level.
pub fn heat(fraction: f64, palette: &Palette) -> Color {
    match fraction {
        f if f < 0.5 => palette.green,
        f if f < 0.75 => palette.yellow,
        _ => palette.red,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(table: &'a [(String, String)], key: &str) -> Option<&'a str> {
        table
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    const REAL_THEME: &str = r##"
# Nord-like palette
background = "#2e3440"
foreground = "#eceff4"

accent = "#88c0d0"      # the cyan accent
selection = "#434c5e"
cyan = "#8fbcbb"
dark_foreground = "#4c566a"
red = "#bf616a"
yellow = "#ebcb8b"
green = "#a3be8c"
"##;

    // ---- parse_pairs -----------------------------------------------------

    #[test]
    fn parse_pairs_reads_quoted_values() {
        let table = parse_pairs(REAL_THEME);
        assert_eq!(get(&table, "background"), Some("#2e3440"));
        assert_eq!(get(&table, "red"), Some("#bf616a"));
    }

    #[test]
    fn parse_pairs_ignores_comments_and_blank_lines() {
        let table =
            parse_pairs("# just a comment\n\n   \n  # indented comment\naccent = \"#ffffff\"\n");
        assert_eq!(table.len(), 1);
        assert_eq!(get(&table, "accent"), Some("#ffffff"));
    }

    #[test]
    fn parse_pairs_stops_a_quoted_value_at_the_closing_quote() {
        // The bug this guards: a trailing comment used to be glued onto the
        // value, and hex() then read a wrong colour out of it silently.
        let table = parse_pairs("accent = \"#88c0d0\" # the accent\n");
        assert_eq!(get(&table, "accent"), Some("#88c0d0"));
        assert_eq!(
            hex(get(&table, "accent").unwrap()),
            Some(Color::Rgb(0x88, 0xc0, 0xd0))
        );
    }

    #[test]
    fn parse_pairs_handles_bare_values_and_whitespace() {
        let table = parse_pairs("  accent = #88c0d0  \nbackground=#000000\n");
        assert_eq!(get(&table, "accent"), Some("#88c0d0"));
        assert_eq!(get(&table, "background"), Some("#000000"));
    }

    #[test]
    fn parse_value_keeps_a_leading_hash_in_a_bare_value() {
        assert_eq!(parse_value("#88c0d0"), "#88c0d0");
        assert_eq!(parse_value("\"#88c0d0\""), "#88c0d0");
        assert_eq!(parse_value("  \"#abc\"  "), "#abc");
        assert_eq!(parse_value("\"#abc\" trailing words"), "#abc");
        // An unterminated quote must not swallow the file.
        assert_eq!(parse_value("\"#abc"), "#abc");
        assert_eq!(parse_value(""), "");
    }

    #[test]
    fn parse_pairs_skips_lines_without_an_equals_sign() {
        let table = parse_pairs("garbage line\naccent = \"#ffffff\"\nalso garbage\n");
        assert_eq!(table.len(), 1);
        assert_eq!(get(&table, "accent"), Some("#ffffff"));
    }

    #[test]
    fn parse_pairs_tolerates_an_empty_file() {
        assert!(parse_pairs("").is_empty());
        assert!(parse_pairs("\n\n").is_empty());
    }

    // ---- hex -------------------------------------------------------------

    #[test]
    fn hex_parses_six_digit_colours_with_or_without_a_hash() {
        assert_eq!(hex("#88c0d0"), Some(Color::Rgb(0x88, 0xc0, 0xd0)));
        assert_eq!(hex("88c0d0"), Some(Color::Rgb(0x88, 0xc0, 0xd0)));
        assert_eq!(hex("  #88C0D0  "), Some(Color::Rgb(0x88, 0xc0, 0xd0)));
        assert_eq!(hex("#000000"), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(hex("#ffffff"), Some(Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn hex_accepts_an_alpha_suffix_by_reading_the_first_six_digits() {
        // Some themes carry 8-digit values; the first six are still the colour.
        assert_eq!(hex("#88c0d0ff"), Some(Color::Rgb(0x88, 0xc0, 0xd0)));
    }

    #[test]
    fn hex_rejects_short_and_non_hex_values() {
        assert_eq!(hex(""), None);
        assert_eq!(hex("#abc"), None, "fewer than six digits");
        assert_eq!(hex("#gggggg"), None, "not hex digits");
        assert_eq!(hex("#88c0dz"), None);
        assert_eq!(hex("nonsense"), None);
    }

    #[test]
    fn hex_never_panics_on_multibyte_input() {
        // `hex` slices by byte index; a non-ASCII value must be rejected, not
        // sliced mid-character.
        for bad in ["#é", "🎉🎉🎉", "#12🎉45", "  "] {
            let _ = hex(bad);
        }
    }

    // ---- apply_table -----------------------------------------------------

    #[test]
    fn apply_table_maps_every_key_the_ui_uses() {
        let mut p = Palette::default();
        apply_table(&mut p, &parse_pairs(REAL_THEME));
        assert_eq!(p.background, Color::Rgb(0x2e, 0x34, 0x40));
        assert_eq!(p.foreground, Color::Rgb(0xec, 0xef, 0xf4));
        assert_eq!(p.accent, Color::Rgb(0x88, 0xc0, 0xd0));
        assert_eq!(p.selection, Color::Rgb(0x43, 0x4c, 0x5e));
        assert_eq!(
            p.heading,
            Color::Rgb(0x8f, 0xbc, 0xbb),
            "heading comes from cyan"
        );
        assert_eq!(
            p.muted,
            Color::Rgb(0x4c, 0x56, 0x6a),
            "muted falls back to dark_foreground"
        );
        assert_eq!(p.red, Color::Rgb(0xbf, 0x61, 0x6a));
        assert_eq!(p.yellow, Color::Rgb(0xeb, 0xcb, 0x8b));
        assert_eq!(p.green, Color::Rgb(0xa3, 0xbe, 0x8c));
    }

    #[test]
    fn apply_table_prefers_muted_over_dark_foreground() {
        let mut p = Palette::default();
        apply_table(
            &mut p,
            &parse_pairs("muted = \"#111111\"\ndark_foreground = \"#222222\"\n"),
        );
        assert_eq!(p.muted, Color::Rgb(0x11, 0x11, 0x11));
    }

    #[test]
    fn apply_table_leaves_defaults_for_missing_keys() {
        let before = Palette::default();
        let mut p = Palette::default();
        apply_table(&mut p, &parse_pairs("accent = \"#123456\"\n"));
        assert_eq!(p.accent, Color::Rgb(0x12, 0x34, 0x56));
        let mut after = before.clone();
        after.accent = Color::Rgb(0x12, 0x34, 0x56);
        assert_eq!(p.accent, after.accent);
        assert_eq!(p.background, after.background);
        assert_eq!(p.foreground, after.foreground);
        assert_eq!(p.muted, after.muted);
        assert_eq!(p.heading, after.heading);
    }

    #[test]
    fn apply_table_ignores_malformed_values_instead_of_painting_them() {
        let mut p = Palette::default();
        apply_table(&mut p, &parse_pairs("accent = \"nope\"\nred = \"#12\"\n"));
        let d = Palette::default();
        assert_eq!(p.accent, d.accent);
        assert_eq!(p.red, d.red);
    }

    #[test]
    fn from_omarchy_always_yields_a_usable_palette() {
        // Whatever the machine has installed, this must not panic and must
        // return something renderable.
        let p = Palette::from_omarchy();
        for (name, c) in [
            ("accent", p.accent),
            ("background", p.background),
            ("foreground", p.foreground),
            ("muted", p.muted),
            ("heading", p.heading),
            ("red", p.red),
            ("yellow", p.yellow),
            ("green", p.green),
        ] {
            assert!(
                !matches!(c, Color::Default),
                "{name} fell back to the terminal default"
            );
        }
    }

    // ---- heat ------------------------------------------------------------

    #[test]
    fn heat_maps_low_green_high_red() {
        let p = Palette::default();
        assert_eq!(heat(0.0, &p), p.green);
        assert_eq!(heat(0.49, &p), p.green);
        assert_eq!(heat(0.5, &p), p.yellow);
        assert_eq!(heat(0.74, &p), p.yellow);
        assert_eq!(heat(0.75, &p), p.red);
        assert_eq!(heat(1.0, &p), p.red);
    }

    #[test]
    fn heat_treats_nan_as_hot_rather_than_falling_through_quietly() {
        // Every comparison against NaN is false, so without this the `red`
        // arm is what NaN lands in. That is the safe direction: a NaN reading
        // should look alarming, not calm.
        let p = Palette::default();
        assert_eq!(heat(f64::NAN, &p), p.red);
    }

    #[test]
    fn heat_handles_out_of_range_fractions() {
        let p = Palette::default();
        assert_eq!(heat(-1.0, &p), p.green);
        assert_eq!(heat(5.0, &p), p.red);
    }
}
