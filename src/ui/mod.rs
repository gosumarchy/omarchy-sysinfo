//! Drawing the app onto a [`Buffer`]: header, sidebar, detail pane, footer,
//! and the help overlay. Pure layout; the state lives in [`App`].

pub(crate) mod theme;

use crate::app::{App, Focus, Mode};
use crate::collect::{Bar, Row};
use crate::term::{Buffer, Style};
use crate::text::{clip, width as text_width};
use theme::{Palette, heat};

/// The smallest terminal the layout draws into.
const MIN_WIDTH: u16 = 20;
const MIN_HEIGHT: u16 = 6;

/// Columns the gauge and its percentage take at the right edge.
const GAUGE_COLUMN: u16 = 12;
const GAUGE_WIDTH: u16 = 4;

/// Sidebar width, narrowed on small terminals so the detail pane survives.
fn sidebar_width(width: u16) -> u16 {
    (width / 3).clamp(12, 24)
}

/// How many detail rows fit in a terminal `height` rows tall: everything but
/// the header, the pane's title rule and the footer.
pub(crate) fn detail_rows(height: u16) -> usize {
    usize::from(height.saturating_sub(3))
}

pub(crate) fn draw(buffer: &mut Buffer, app: &App, palette: &Palette) {
    let width = buffer.width();
    let height = buffer.height();
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        let style = Style::new(palette.muted);
        buffer.put(1, 1, "terminal too small — resize to at least 20x6", style);

        return;
    }

    let body_top = 1u16;
    let footer_row = height - 1;
    let body_height = footer_row - body_top;
    let sidebar = sidebar_width(width);

    header(buffer, app, palette);
    sidebar_pane(buffer, body_top, body_height, sidebar, app, palette);
    detail(buffer, sidebar, body_top, body_height, app, palette);
    footer(buffer, footer_row, app, palette);

    if app.mode() == Mode::Help {
        help(buffer, palette);
    }
}

fn header(buffer: &mut Buffer, app: &App, palette: &Palette) {
    let width = buffer.width();
    let badge = Style::new(palette.background).on(palette.accent).bold();
    let mut x = buffer.put(0, 0, " omarchy-sysinfo ", badge) + 1;

    match app.snapshot() {
        Some(snapshot) => {
            x = buffer.put(
                x,
                0,
                &snapshot.hostname,
                Style::new(palette.foreground).bold(),
            ) + 2;
            let meta = format!(
                "· {} · uptime {}",
                snapshot.kernel,
                crate::collect::units::human_secs(snapshot.uptime)
            );
            if text_width(&meta) < usize::from(width.saturating_sub(x)) {
                buffer.put(x, 0, &meta, Style::new(palette.muted));
            }
        }
        None => {
            buffer.put(x, 0, "collecting…", Style::new(palette.muted));
        }
    }

    buffer.hline(0, 1, width, '─', Style::new(palette.muted).dim());
}

fn sidebar_pane(
    buffer: &mut Buffer,
    top: u16,
    height: u16,
    width: u16,
    app: &App,
    palette: &Palette,
) {
    let border = Style::new(match app.focus() {
        Focus::Sections => palette.accent,
        Focus::Detail => palette.muted,
    })
    .dim();
    let sections = app.sections();
    let visible = usize::from(height.saturating_sub(1));
    let selected = app.selected();

    // Scroll the sidebar so the selection stays on screen.
    let start = (selected + 1).saturating_sub(visible);

    for (y, (index, section)) in
        (top + 1..).zip(sections.iter().enumerate().skip(start).take(visible))
    {
        let is_selected = index == selected;
        let (marker, style) = if is_selected {
            (
                "▸ ",
                Style::new(palette.background).on(palette.accent).bold(),
            )
        } else {
            ("  ", Style::new(palette.foreground))
        };
        let room = usize::from(width.saturating_sub(1));
        let text = clip(&format!("{marker}{}", section.title), room);
        let end = buffer.put(0, y, &text, style);
        if is_selected {
            // Extend the highlight across the rest of the sidebar.
            buffer.hline(end, y, width - 1, ' ', style);
        }
    }

    if sections.len() > visible {
        let more = format!("  +{} more", sections.len() - visible);
        buffer.put(1, top + height, &more, Style::new(palette.muted).dim());
    }

    buffer.vline(width - 1, top, top + height, '│', border);
    buffer.put(0, top, " sections ", Style::new(palette.muted).dim());
}

/// One rendered line of the detail pane, borrowing from the snapshot.
enum Line<'a> {
    Text(String, Style),
    Field {
        label: &'a str,
        value: &'a str,
        style: Style,
        /// The gauge travels as a `Bar` rather than a bare ratio, so the
        /// clamped invariant survives all the way to the renderer.
        bar: Option<Bar>,
    },
    Blank,
}

fn line<'a>(row: &'a Row, palette: &Palette) -> Line<'a> {
    match row {
        Row::Header(text) => Line::Text(format!("▌ {text}"), Style::new(palette.heading).bold()),
        Row::Note(text) => Line::Text(format!("  {text}"), Style::new(palette.muted)),
        Row::Blank => Line::Blank,
        Row::Field {
            label, value, bar, ..
        } => Line::Field {
            label,
            value,
            style: Style::new(bar.map_or(palette.foreground, |b| heat(b.frac(), palette))),
            bar: *bar,
        },
    }
}

fn detail(buffer: &mut Buffer, x: u16, top: u16, height: u16, app: &App, palette: &Palette) {
    let width = buffer.width();
    let inner_width = usize::from(width.saturating_sub(x + 1));
    let inner_height = usize::from(height.saturating_sub(1));
    let rows = app.visible_rows();
    let total = rows.len();

    buffer.hline(x, top, width, '─', Style::new(palette.muted).dim());
    let range = if total == 0 {
        "no rows".to_string()
    } else {
        let first = app.scroll() + 1;
        let last = (app.scroll() + inner_height).min(total);
        format!("{first}-{last} of {total}")
    };
    let title = app.section().map_or_else(
        || "waiting for the first reading".into(),
        |s| s.title.to_lowercase(),
    );
    buffer.put(
        x,
        top,
        &clip(&format!(" {title}  {range} "), inner_width),
        Style::new(palette.accent).bold(),
    );

    if inner_width < 10 || inner_height == 0 || app.section().is_none() {
        return;
    }
    if rows.is_empty() {
        buffer.put(
            x + 1,
            top + 1,
            "no rows match the filter",
            Style::new(palette.muted),
        );

        return;
    }

    let label_width = rows
        .iter()
        .filter_map(|r| match r {
            Row::Field { label, .. } => Some(text_width(label) + 2),
            Row::Header(_) | Row::Note(_) | Row::Blank => None,
        })
        .max()
        .unwrap_or(14)
        .clamp(10, 32);

    let layout = Layout::new(x, width, label_width);
    let lines = rows.iter().map(|row| line(row, palette));
    for (y, line) in (top + 1..).zip(lines.skip(app.scroll()).take(inner_height)) {
        match line {
            Line::Blank => {}
            Line::Text(text, style) => {
                buffer.put(x, y, &clip(&text, inner_width), style);
            }
            Line::Field {
                label,
                value,
                style,
                bar,
            } => field(buffer, &layout, y, (label, value, style, bar), palette),
        }
    }
}

/// Where the columns of a field row go at this width.
struct Layout {
    x: u16,
    width: u16,
    label_width: usize,
    value_x: u16,
    value_room: usize,
    gauge_x: u16,
    /// On a narrow terminal there is no room for a label column and a value
    /// column side by side, so each row falls back to a half-and-half split.
    paired: bool,
}

impl Layout {
    fn new(x: u16, width: u16, label_width: usize) -> Layout {
        let gauge_x = width.saturating_sub(GAUGE_COLUMN);
        let value_x = x.saturating_add(u16::try_from(label_width + 2).unwrap_or(u16::MAX));
        // Leave a gap so a long value never butts up against the gauge.
        let value_room = usize::from(gauge_x.saturating_sub(value_x))
            .saturating_sub(2)
            .max(4);

        Layout {
            x,
            width,
            label_width,
            value_x,
            value_room,
            gauge_x,
            paired: usize::from(value_x) + 8 <= usize::from(width),
        }
    }
}

fn field(
    buffer: &mut Buffer,
    layout: &Layout,
    y: u16,
    (label, value, style, bar): (&str, &str, Style, Option<Bar>),
    palette: &Palette,
) {
    let label = format!("  {label}");
    // With a gauge on the right, a percentage already in the value would
    // just be printed twice.
    let value = if bar.is_some() {
        strip_percent(value)
    } else {
        value
    };
    let label_style = Style::new(palette.muted);

    if !layout.paired {
        let half = usize::from(layout.width.saturating_sub(layout.x + 1)) / 2;
        buffer.put(layout.x, y, &clip(&label, half), label_style);
        let after = layout.x + u16::try_from(half).unwrap_or(u16::MAX) + 2;
        let room = usize::from(layout.width.saturating_sub(after)).max(1);
        buffer.put(after, y, &clip(value, room), style);

        return;
    }

    // Clip the label rather than let it run into the value column.
    buffer.put(layout.x, y, &clip(&label, layout.label_width), label_style);
    buffer.put(layout.value_x, y, &clip(value, layout.value_room), style);

    if let Some(bar) = bar {
        let heat_style = Style::new(heat(bar.frac(), palette));
        buffer.gauge(
            layout.gauge_x,
            y,
            GAUGE_WIDTH,
            bar,
            heat_style,
            Style::new(palette.muted).dim(),
        );
        buffer.put_right(
            layout.gauge_x + GAUGE_WIDTH + 2,
            y,
            &format!("{}%", bar.percent()),
            heat_style,
        );
    }
}

/// Drop a percentage the gauge column already shows, so it is not printed twice.
fn strip_percent(value: &str) -> &str {
    let trimmed = value.trim();
    // A value that is nothing but a percentage is fully covered by the gauge.
    if is_percent(trimmed) {
        return "";
    }
    // Otherwise drop a leading "45% " and keep whatever followed it.
    match trimmed.split_once(' ') {
        Some((first, rest)) if is_percent(first) => rest.trim_start(),
        _ => value,
    }
}

fn is_percent(token: &str) -> bool {
    let Some(digits) = token.strip_suffix('%') else {
        return false;
    };

    // At least one real digit, so "..%" and "%" are not mistaken for a value.
    digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.chars().any(|c| c.is_ascii_digit())
}

fn footer(buffer: &mut Buffer, y: u16, app: &App, palette: &Palette) {
    let width = buffer.width();
    let key = Style::new(palette.accent);
    let hint = Style::new(palette.muted);

    buffer.fill_row(y, ' ', Style::new(palette.muted).on(palette.background));

    if app.mode() == Mode::Filter {
        let mut x = buffer.put(0, y, " /", key);
        x = buffer.put(x, y, app.filter().query(), Style::new(palette.foreground));
        x = buffer.put(x, y, "▏", key);
        buffer.put(x + 1, y, "enter to apply · esc to clear", hint);

        return;
    }

    // ↑↓ means different things depending on which pane has focus, so say so.
    let move_hint = match app.focus() {
        Focus::Sections => " section  ",
        Focus::Detail => " scroll  ",
    };
    let mut x = 0;
    for (k, d) in [
        (" q", " quit  "),
        ("↑↓", move_hint),
        ("tab", " pane  "),
        ("/", " filter  "),
        ("r", " refresh  "),
        ("?", " help  "),
    ] {
        if usize::from(x) + text_width(k) + text_width(d) > usize::from(width) {
            break;
        }
        x = buffer.put(x, y, k, key);
        x = buffer.put(x, y, d, hint);
    }

    if let Some(status) = app.status() {
        buffer.put_right(width, y, status, Style::new(palette.green));
    }
}

fn help(buffer: &mut Buffer, palette: &Palette) {
    const ENTRIES: [(&str, &str); 9] = [
        ("q / esc", "quit"),
        ("↑ ↓  or  j k", "previous or next section, or scroll"),
        ("g / G", "jump to the first or last section"),
        ("pgup / pgdn", "scroll the detail pane a page"),
        ("← →  or  h l", "move between the sidebar and the detail"),
        ("tab", "swap panes"),
        ("/", "filter rows by substring"),
        ("r", "re-read /proc and /sys right now"),
        ("?", "close this help"),
    ];
    const INNER_WIDTH: u16 = 58;

    let entries = u16::try_from(ENTRIES.len()).unwrap_or(u16::MAX);
    let width = (INNER_WIDTH + 2).min(buffer.width().saturating_sub(4));
    let height = (entries + 4).min(buffer.height().saturating_sub(2));
    if width < 4 || height < 4 {
        return;
    }
    let x = (buffer.width() - width) / 2;
    let y = (buffer.height() - height) / 2;
    let (right, bottom) = (x + width - 1, y + height - 1);

    let panel = Style::new(palette.foreground).on(palette.background);
    for row in y..=bottom {
        buffer.hline(x, row, right + 1, ' ', panel);
    }

    let border = Style::new(palette.accent);
    buffer.put(x, y, "┌", border);
    buffer.put(right, y, "┐", border);
    buffer.put(x, bottom, "└", border);
    buffer.put(right, bottom, "┘", border);
    buffer.hline(x + 1, y, right, '─', border);
    buffer.hline(x + 1, bottom, right, '─', border);
    buffer.vline(x, y + 1, bottom, '│', border);
    buffer.vline(right, y + 1, bottom, '│', border);
    buffer.put(x + 2, y, " keys ", Style::new(palette.accent).bold());

    for (row, (k, d)) in (y + 2..bottom).zip(ENTRIES) {
        buffer.put(x + 2, row, k, Style::new(palette.accent));
        buffer.put(x + 17, row, d, panel);
    }

    buffer.put(
        x + 2,
        bottom - 1,
        "press ? or esc to close",
        Style::new(palette.muted).on(palette.background),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{Section, Snapshot};
    use crate::event::Trigger;
    use crate::input::Key;
    use crate::term::Color;
    use crate::text::Text;

    /// Drop ANSI escapes so a rendered buffer can be compared as text.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Every rendered row as plain text, exactly as a terminal would show it.
    fn rows(buffer: &Buffer) -> Vec<String> {
        strip_ansi(&buffer.render())
            .split('\n')
            .map(|r| r.trim_end_matches('\r').to_string())
            .collect()
    }

    fn row(buffer: &Buffer, y: usize) -> String {
        rows(buffer).get(y).cloned().unwrap_or_default()
    }

    fn palette() -> Palette {
        Palette::default()
    }

    fn snapshot(sections: Vec<Section>) -> Snapshot {
        Snapshot {
            sections,
            hostname: Text::new("testhost"),
            kernel: Text::new("6.1.0-test"),
            uptime: 3_661,
        }
    }

    /// A hand-built App so the tests do not depend on this machine's hardware.
    fn app_with(sections: Vec<Section>, height: u16) -> App {
        let mut app = App::new();
        app.on_snapshot(snapshot(sections), Trigger::Timer);
        app.set_viewport(detail_rows(height));
        app
    }

    fn sample() -> Vec<Section> {
        vec![Section::new(
            "Test",
            vec![
                Row::header("Group"),
                Row::field("Label", "value"),
                Row::field_with("Barred", "42%", 0.42),
                Row::note("a note"),
                Row::Blank,
                Row::field("Second", "value"),
            ],
        )]
    }

    fn buffer(w: u16, h: u16) -> Buffer {
        Buffer::new(w, h, Style::new(Color::Default))
    }

    fn drawn(app: &App, w: u16, h: u16) -> Buffer {
        let mut b = buffer(w, h);
        draw(&mut b, app, &palette());
        b
    }

    fn press(app: &mut App, keys: &[Key]) {
        for key in keys {
            app.on_key(*key);
        }
    }

    // ---- layout helpers --------------------------------------------------

    #[test]
    fn sidebar_width_is_clamped_at_both_ends() {
        assert_eq!(sidebar_width(200), 24, "wide terminal caps at 24");
        assert_eq!(sidebar_width(72), 24);
        assert_eq!(sidebar_width(60), 20);
        assert_eq!(sidebar_width(45), 15);
        assert_eq!(sidebar_width(36), 12);
        assert_eq!(sidebar_width(30), 12, "narrow terminal keeps a floor of 12");
        assert_eq!(sidebar_width(0), 12, "never collapses to zero");
    }

    #[test]
    fn detail_rows_leaves_room_for_the_chrome() {
        assert_eq!(detail_rows(24), 21);
        assert_eq!(detail_rows(2), 0);
    }

    // ---- strip_percent / is_percent ---------------------------------------

    #[test]
    fn is_percent_only_accepts_something_that_looks_like_a_number() {
        assert!(is_percent("0%"));
        assert!(is_percent("45%"));
        assert!(is_percent("45.5%"));
        assert!(is_percent("100%"));
        assert!(!is_percent(""));
        assert!(!is_percent("%"));
        assert!(!is_percent("45"));
        assert!(!is_percent("45 %"));
        assert!(!is_percent("abc%"));
        assert!(!is_percent("4a5%"));
        assert!(!is_percent("..%"), "dots alone are not a percentage");
        assert!(!is_percent(".%"));
    }

    #[test]
    fn strip_percent_drops_a_value_the_gauge_already_shows() {
        assert_eq!(strip_percent("42%"), "");
        assert_eq!(strip_percent("  42%  "), "");
        assert_eq!(strip_percent("42% of 8"), "of 8");
        assert_eq!(strip_percent("42% 12.3 GiB"), "12.3 GiB");
    }

    #[test]
    fn strip_percent_leaves_other_values_alone() {
        assert_eq!(strip_percent("12.3 GiB"), "12.3 GiB");
        assert_eq!(strip_percent(""), "");
        assert_eq!(strip_percent("45"), "45");
        assert_eq!(
            strip_percent("  spaced  "),
            "  spaced  ",
            "keeps the original spacing"
        );
    }

    // ---- draw: degenerate sizes -----------------------------------------

    #[test]
    fn draw_reports_a_too_small_terminal_instead_of_mangling_the_screen() {
        for (w, h) in [
            (0, 0),
            (1, 1),
            (5, 3),
            (19, 20),
            (20, 5),
            (19, 5),
            (46, 5),
            (60, 3),
        ] {
            let b = drawn(&app_with(sample(), h), w, h);
            // The hint itself needs 46 columns and 2 rows, so a smaller buffer
            // than that can only be checked for not panicking.
            if w >= 46 && h >= 2 {
                let text = rows(&b).concat();
                assert!(
                    text.contains("terminal too small"),
                    "expected a resize hint at {w}x{h}, got {text:?}"
                );
            }
        }
    }

    #[test]
    fn draw_survives_every_size_in_every_mode() {
        // The layout is full of offset arithmetic; a sweep is the cheapest
        // way to catch an underflow at a size nobody thought to check.
        let modes: [&[Key]; 4] = [
            &[],
            &[Key::Char('?')],
            &[Key::Char('/'), Key::Char('l'), Key::Char('a')],
            &[Key::Right, Key::PageDown, Key::PageDown],
        ];
        for keys in modes {
            for w in 0u16..48 {
                for h in 0u16..16 {
                    let mut app = app_with(sample(), h);
                    press(&mut app, keys);
                    drawn(&app, w, h);
                }
            }
        }
    }

    #[test]
    fn draw_before_the_first_snapshot_says_it_is_collecting() {
        let text = rows(&drawn(&App::new(), 80, 24)).concat();

        assert!(text.contains("collecting"), "{text}");
        assert!(text.contains("waiting for the first reading"), "{text}");
    }

    // ---- draw: content ---------------------------------------------------

    #[test]
    fn draw_shows_the_hostname_kernel_uptime_and_section_titles() {
        let mut sections = sample();
        sections.push(Section::new("Second Section", vec![Row::field("a", "b")]));
        let text = rows(&drawn(&app_with(sections, 24), 80, 24)).concat();

        assert!(text.contains("omarchy-sysinfo"), "header title missing");
        assert!(text.contains("testhost"), "hostname missing");
        assert!(text.contains("6.1.0-test"), "kernel missing");
        assert!(text.contains("1h 1m 1s"), "uptime missing");
        assert!(text.contains("Test"), "section title missing");
        assert!(text.contains("Second Section"), "second section missing");
    }

    #[test]
    fn draw_marks_exactly_the_selected_section() {
        let mut sections = sample();
        sections.push(Section::new("Other", vec![]));
        let mut app = app_with(sections, 24);
        app.on_key(Key::Down);
        let text = rows(&drawn(&app, 80, 24)).concat();

        assert!(text.contains("▸ Other"), "{text}");
        assert_eq!(text.matches('▸').count(), 1);
    }

    #[test]
    fn draw_renders_a_gauge_and_prints_its_percentage_once() {
        let text = rows(&drawn(&app_with(sample(), 24), 80, 24)).concat();

        assert!(text.contains('█') || text.contains('░'), "no gauge drawn");
        assert_eq!(
            text.matches("42%").count(),
            1,
            "the gauge column and the value must not both print 42%"
        );
    }

    #[test]
    fn a_wide_value_does_not_push_the_frame_out_of_shape() {
        // A CJK window title used to be measured in chars, overrun the right
        // edge, and make the terminal wrap the row.
        let sections = vec![Section::new(
            "Wide",
            vec![Row::field(
                "Focused window",
                "日本語のウィンドウタイトル".repeat(4),
            )],
        )];
        let b = drawn(&app_with(sections, 24), 60, 24);

        for line in rows(&b) {
            assert!(
                text_width(&line) <= 60,
                "{line:?} is {} cells",
                text_width(&line)
            );
        }
    }

    #[test]
    fn the_last_rows_stay_on_screen_at_maximum_scroll() {
        let many: Vec<Row> = (0..40)
            .map(|i| Row::field(format!("row {i}"), "v"))
            .collect();
        let mut app = app_with(vec![Section::new("Long", many)], 24);
        press(
            &mut app,
            &[Key::Right, Key::PageDown, Key::PageDown, Key::PageDown],
        );
        let b = drawn(&app, 80, 24);

        assert!(row(&b, 22).contains("row 39"), "{:?}", rows(&b));
        assert!(row(&b, 1).contains("20-40 of 40"), "{:?}", row(&b, 1));
    }

    #[test]
    fn draw_says_so_when_a_filter_matches_nothing() {
        let mut app = app_with(sample(), 24);
        press(&mut app, &[Key::Char('/'), Key::Char('z'), Key::Char('z')]);

        assert!(
            rows(&drawn(&app, 80, 24))
                .concat()
                .contains("no rows match")
        );
    }

    #[test]
    fn draw_shows_the_row_range_in_the_detail_title() {
        let b = drawn(&app_with(sample(), 24), 80, 24);

        assert!(row(&b, 0).contains("omarchy-sysinfo"), "header missing");
        assert!(row(&b, 1).contains("1-6 of 6"), "{:?}", row(&b, 1));
    }

    // ---- footer ----------------------------------------------------------

    #[test]
    fn footer_shows_the_filter_prompt_while_filtering() {
        let mut app = app_with(sample(), 24);
        press(
            &mut app,
            &[
                Key::Char('/'),
                Key::Char('g'),
                Key::Char('p'),
                Key::Char('u'),
            ],
        );
        let last = row(&drawn(&app, 80, 24), 23);

        assert!(last.contains("gpu"), "typed query not echoed: {last:?}");
        assert!(last.contains("esc to clear"), "filter hint missing");
    }

    #[test]
    fn footer_hints_mean_different_things_per_focus() {
        let mut app = app_with(sample(), 24);
        assert!(row(&drawn(&app, 80, 24), 23).contains("section"));

        app.on_key(Key::Tab);
        let focused = row(&drawn(&app, 80, 24), 23);
        assert!(
            focused.contains("scroll"),
            "expected a scroll hint: {focused:?}"
        );
    }

    #[test]
    fn footer_shows_a_status_message() {
        let mut app = app_with(sample(), 24);
        app.on_key(Key::Char('r'));

        assert!(row(&drawn(&app, 80, 24), 23).contains("refreshing"));
    }

    #[test]
    fn footer_keeps_the_quit_hint_at_every_width() {
        let app = app_with(sample(), 10);
        for w in 20u16..40 {
            let last = row(&drawn(&app, w, 10), 9);
            assert!(last.contains('q'), "quit hint should survive at width {w}");
        }
    }

    // ---- help ------------------------------------------------------------

    #[test]
    fn help_overlay_lists_the_bindings() {
        let mut app = app_with(sample(), 30);
        app.on_key(Key::Char('?'));
        let text = rows(&drawn(&app, 100, 30)).concat();

        for expected in ["quit", "previous or next section", "filter", "help"] {
            assert!(text.contains(expected), "help is missing {expected:?}");
        }
    }

    #[test]
    fn help_overlay_fits_itself_to_a_small_terminal() {
        for (w, h) in [(20u16, 6u16), (24, 8), (40, 10), (60, 20), (200, 60)] {
            let mut app = app_with(sample(), h);
            app.on_key(Key::Char('?'));
            let b = drawn(&app, w, h);
            assert_eq!(
                rows(&b).len(),
                usize::from(h),
                "row count changed at {w}x{h}"
            );
        }
    }
}
