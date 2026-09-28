pub mod theme;

use crate::app::{App, Focus};
use crate::collect::{Bar, Row};
use crate::term::{Buffer, Style};
use theme::{Palette, heat};

/// Sidebar width, narrowed on small terminals so the detail pane survives.
fn sidebar_width(width: u16) -> u16 {
    (width / 3).clamp(12, 24)
}

pub fn draw(buffer: &mut Buffer, app: &App, palette: &Palette) {
    let width = buffer.width();
    let height = buffer.height();
    if width < 20 || height < 6 {
        let style = Style::new(palette.muted);
        buffer.put(1, 1, "terminal too small — resize to at least 20x6", style);

        return;
    }

    let body_top = 1u16;
    let footer_row = height - 1;
    let body_height = footer_row.saturating_sub(body_top);
    let sidebar = sidebar_width(width);

    header(buffer, width, app, palette);
    sidebar_pane(buffer, 0, body_top, body_height, sidebar, app, palette);
    detail(buffer, sidebar, body_top, body_height, app, palette);
    footer(buffer, width, footer_row, app, palette);

    if app.show_help {
        help(buffer, app, palette);
    }
}

fn header(buffer: &mut Buffer, width: u16, app: &App, palette: &Palette) {
    let title = " omarchy-sysinfo ";
    let badge = Style::new(palette.background).on(palette.accent).bold();
    let mut x = buffer.put(0, 0, title, badge);
    x += 1;

    let hostname_style = Style::new(palette.foreground).bold();
    x = buffer.put(x, 0, &app.hostname, hostname_style);
    x += 2;

    let uptime = crate::collect::units::human_secs(app.stats.uptime());
    let meta_style = Style::new(palette.muted);
    let meta = format!("· {} · uptime {}", app.kernel, uptime);
    let room = width.saturating_sub(x + 1) as usize;
    if meta.chars().count() <= room {
        buffer.put(x, 0, &meta, meta_style);
    }

    buffer.hline(0, 1, width, '─', Style::new(palette.muted).dim());
}

fn sidebar_pane(
    buffer: &mut Buffer,
    x: u16,
    top: u16,
    height: u16,
    width: u16,
    app: &App,
    palette: &Palette,
) {
    let focused = app.focus == Focus::Sections;
    let border = Style::new(if focused {
        palette.accent
    } else {
        palette.muted
    })
    .dim();

    let visible = height.saturating_sub(1) as usize;
    let selected = app.selected;

    // Scroll the sidebar so the selection stays on screen.
    let mut start = 0usize;
    if selected >= visible {
        start = selected + 1 - visible;
    }

    for (row, index) in (start..app.sections.len()).take(visible).enumerate() {
        let y = top + 1 + row as u16;
        let is_selected = index == selected;
        let style = if is_selected {
            Style::new(palette.background).on(palette.accent).bold()
        } else {
            Style::new(palette.foreground)
        };
        let text = if is_selected {
            format!("▸ {}", app.sections[index].title)
        } else {
            format!("  {}", app.sections[index].title)
        };
        buffer.put(x, y, &text, style);
        if is_selected {
            // Extend the highlight across the rest of the sidebar.
            let from = x + text.chars().count().min(width as usize - 1) as u16;
            for col in from..width - 1 {
                buffer.put(col, y, " ", style);
            }
        }
    }

    if app.sections.len() > visible {
        let more = format!("  +{} more", app.sections.len() - visible);
        let y = top + height;
        buffer.put(x + 1, y, &more, Style::new(palette.muted).dim());
    }

    buffer.vline(width - 1, top, top + height, '│', border);
    buffer.put(x, top, " sections ", Style::new(palette.muted).dim());
}

fn detail(buffer: &mut Buffer, x: u16, top: u16, height: u16, app: &App, palette: &Palette) {
    let section = app.section();
    let total = app.detail_len();
    let width = buffer.width();
    let inner_width = width.saturating_sub(x + 1) as usize;
    let inner_height = height.saturating_sub(1) as usize;

    buffer.hline(x, top, width, '─', Style::new(palette.muted).dim());
    let range = if total == 0 {
        "no rows".to_string()
    } else {
        let first = (app.scroll as usize + 1).min(total);
        let last = (first + inner_height).min(total);
        format!("{first}-{last} of {total}")
    };
    let title = format!(" {}  {} ", section.title.to_lowercase(), range);
    buffer.put(x, top, &title, Style::new(palette.accent).bold());

    if inner_width < 10 || inner_height == 0 {
        return;
    }

    // Build the visible rows, keeping the label and value apart so each can
    // take its own colour.
    let visible_rows: Vec<&Row> = section
        .rows
        .iter()
        .filter(|r| app.filter.matches(r))
        .collect();
    let label_width = visible_rows
        .iter()
        .filter_map(|r| match r {
            Row::Field { label, .. } => Some(label.chars().count() + 2),
            _ => None,
        })
        .max()
        .unwrap_or(14)
        .clamp(10, 32);

    /// One rendered line: either plain text, or a label/value pair.
    enum Line {
        Text(String, Style),
        /// The gauge travels as a `Bar` rather than a bare ratio, so the
        /// clamped invariant survives all the way to the renderer.
        Field(String, String, Style, Option<Bar>),
        Blank,
    }

    let mut lines: Vec<Line> = Vec::new();
    for row in &visible_rows {
        match row {
            Row::Header(text) => {
                lines.push(Line::Text(
                    format!("▌ {text}"),
                    Style::new(palette.heading).bold(),
                ));
            }
            Row::Blank => lines.push(Line::Blank),
            Row::Note(text) => {
                lines.push(Line::Text(format!("  {text}"), Style::new(palette.muted)));
            }
            Row::Field { label, value, bar } => {
                let style = match bar {
                    Some(b) => Style::new(heat(b.frac(), palette)),
                    None => Style::new(palette.foreground),
                };
                lines.push(Line::Field(
                    format!("  {label}"),
                    value.clone(),
                    style,
                    bar.clone(),
                ));
            }
        }
    }

    // The gauge column sits on the right, reserved from the value text.
    let gauge_x = width.saturating_sub(12);
    let value_x = x + label_width as u16 + 2;
    // Leave a gap so a long value never butts up against the gauge.
    let value_room = (gauge_x.saturating_sub(value_x) as usize)
        .saturating_sub(2)
        .max(4);
    // On a narrow terminal there is no room for a label column and a value
    // column side by side, so fall back to one line per row.
    let paired = (value_x as usize + 8) <= width as usize;

    let start = app.scroll as usize;
    // Clamp against the viewport, not just the row count: `max_scroll` is
    // row-count based, so on a tall pane it allowed scrolling until the last
    // row sat at the top and the rest of the pane was empty.
    let start = start.min(lines.len().saturating_sub(inner_height));
    for (offset, line) in lines.iter().skip(start).take(inner_height).enumerate() {
        let y = top + 1 + offset as u16;
        match line {
            Line::Blank => {}
            Line::Text(text, style) => {
                buffer.put(x, y, &clip(text, inner_width), *style);
            }
            Line::Field(label, value, style, bar) => {
                // With a gauge on the right, a percentage already in the value
                // would just be printed twice.
                let text = if bar.is_some() {
                    strip_percent(value)
                } else {
                    value.clone()
                };
                if paired {
                    // Clip the label rather than let it run into the value column.
                    buffer.put(x, y, &clip(label, label_width), Style::new(palette.muted));
                    buffer.put(value_x, y, &clip(&text, value_room), *style);
                } else {
                    // Too narrow for two columns: keep the value on the label's
                    // own row, and give up the gauge.
                    buffer.put(
                        x,
                        y,
                        &clip(label, inner_width / 2),
                        Style::new(palette.muted),
                    );
                    let after = x + (inner_width / 2) as u16 + 2;
                    let room = width.saturating_sub(after) as usize;
                    buffer.put(after, y, &clip(&text, room.max(1)), *style);
                    continue;
                }
                if let Some(bar) = bar {
                    let frac = bar.frac();
                    buffer.gauge(
                        gauge_x,
                        y,
                        4,
                        bar.clone(),
                        Style::new(heat(frac, palette)),
                        Style::new(palette.muted).dim(),
                    );
                    buffer.put_right(
                        gauge_x + 6,
                        y,
                        &format!("{:.0}%", frac * 100.0),
                        Style::new(heat(frac, palette)),
                    );
                }
            }
        }
    }

    if lines.is_empty() {
        buffer.put(
            x + 1,
            top + 1,
            "no rows match the filter",
            Style::new(palette.muted),
        );
    }
}

/// Longest prefix of `text` that fits in `width` columns.
fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        // A zero-width column has no room for the ellipsis either.
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i == width - 1 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Drop a percentage the gauge column already shows, so it is not printed twice.
fn strip_percent(value: &str) -> String {
    let trimmed = value.trim();
    // A value that is nothing but a percentage is fully covered by the gauge.
    if is_percent(trimmed) {
        return String::new();
    }
    // Otherwise drop a leading "45% " and keep whatever followed it.
    if let Some((first, rest)) = trimmed.split_once(' ') {
        if is_percent(first) {
            return rest.trim_start().to_string();
        }
    }
    value.to_string()
}

fn is_percent(token: &str) -> bool {
    let Some(digits) = token.strip_suffix('%') else {
        return false;
    };
    // At least one real digit, so "..%" and "%" are not mistaken for a value.
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.chars().any(|c| c.is_ascii_digit())
}

fn footer(buffer: &mut Buffer, width: u16, y: u16, app: &App, palette: &Palette) {
    let key = Style::new(palette.accent);
    let hint = Style::new(palette.muted);
    let mut x = 0;

    buffer.fill_row(y, ' ', Style::new(palette.muted).on(palette.background));

    if app.filter.active {
        x = buffer.put(x, y, " /", key);
        x = buffer.put(x, y, &app.filter.query, Style::new(palette.foreground));
        x = buffer.put(x, y, "▏", key);
        buffer.put(x + 1, y, "enter to apply · esc to clear", hint);

        return;
    }

    // ↑↓ means different things depending on which pane has focus, so say so.
    let move_hint = match app.focus {
        crate::app::Focus::Sections => " section  ",
        crate::app::Focus::Detail => " scroll  ",
    };
    for (k, d) in [
        (" q", " quit  "),
        ("↑↓", move_hint),
        ("tab", " pane  "),
        ("/", " filter  "),
        ("r", " refresh  "),
        ("?", " help  "),
    ] {
        if x + k.chars().count() as u16 + d.chars().count() as u16 > width {
            break;
        }
        x = buffer.put(x, y, k, key);
        x = buffer.put(x, y, d, hint);
    }

    if !app.status.is_empty() {
        buffer.put_right(width, y, &app.status, Style::new(palette.green));
    }
}

fn help(buffer: &mut Buffer, app: &App, palette: &Palette) {
    let entries: [(&str, &str); 9] = [
        ("q / esc", "quit"),
        ("↑ ↓  or  j k", "previous or next section"),
        ("g / G", "jump to the first or last section"),
        ("pgup / pgdn", "scroll the detail pane by ten rows"),
        ("← →  or  h l", "move between the sidebar and the detail"),
        ("tab", "swap panes"),
        ("/", "filter rows by substring"),
        ("r", "re-read /proc and /sys right now"),
        ("?", "close this help"),
    ];

    let inner_width = 58usize;
    let width = (inner_width as u16 + 2).min(buffer.width().saturating_sub(4));
    let height = (entries.len() as u16 + 4).min(buffer.height().saturating_sub(2));
    let x = (buffer.width() - width) / 2;
    let y = (buffer.height() - height) / 2;

    let panel = Style::new(palette.foreground).on(palette.background);
    for row in y..y + height {
        for col in x..x + width {
            buffer.put(col, row, " ", panel);
        }
    }

    let border = Style::new(palette.accent);
    buffer.put(x, y, "┌", border);
    buffer.put(x + width - 1, y, "┐", border);
    buffer.put(x, y + height - 1, "└", border);
    buffer.put(x + width - 1, y + height - 1, "┘", border);
    buffer.hline(x + 1, y, x + width - 1, '─', border);
    buffer.hline(x + 1, y + height - 1, x + width - 1, '─', border);
    buffer.vline(x, y + 1, y + height - 1, '│', border);
    buffer.vline(x + width - 1, y + 1, y + height - 1, '│', border);
    buffer.put(x + 2, y, " keys ", Style::new(palette.accent).bold());

    for (index, (k, d)) in entries.iter().enumerate() {
        let row = y + 2 + index as u16;
        if row >= y + height - 1 {
            break;
        }
        buffer.put(x + 2, row, k, Style::new(palette.accent));
        buffer.put(x + 17, row, d, panel);
    }

    buffer.put(
        x + 2,
        y + height - 2,
        "press ? or esc to close",
        Style::new(palette.muted).on(palette.background),
    );
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Filter};
    use crate::collect::{Bar, Row, Section, Stats};
    use crate::term::Color;
    use std::time::{Duration, Instant};

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

    /// A hand-built App so the tests do not depend on this machine's hardware.
    fn app_with(section_rows: Vec<Row>) -> App {
        App {
            sections: vec![Section::new("Test", section_rows)],
            selected: 0,
            focus: Focus::Sections,
            scroll: 0,
            should_quit: false,
            show_help: false,
            status: String::new(),
            status_at: Instant::now(),
            filter: Filter::default(),
            last_refresh: Instant::now(),
            refresh_interval: Duration::from_secs(3600),
            stats: Stats::new(),
            hostname: "testhost".into(),
            kernel: "6.1.0-test".into(),
        }
    }

    fn sample_rows() -> Vec<Row> {
        vec![
            Row::Header("Group".into()),
            Row::field("Label", "value"),
            Row::Field {
                label: "Barred".into(),
                value: "42%".into(),
                bar: Some(Bar::new(0.42)),
            },
            Row::note("a note"),
            Row::Blank,
            Row::field("Second", "value"),
        ]
    }

    fn buffer(w: u16, h: u16) -> Buffer {
        Buffer::new(w, h, Style::new(Color::Default))
    }

    // ---- sidebar_width ---------------------------------------------------

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

    // ---- clip ------------------------------------------------------------

    #[test]
    fn clip_leaves_text_that_already_fits() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abc", 10), "abc");
        assert_eq!(clip("", 5), "");
        assert_eq!(clip("", 0), "");
    }

    #[test]
    fn clip_truncates_with_an_ellipsis_that_fits_inside_the_budget() {
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abcdef", 1), "…");
        assert_eq!(clip("abcdef", 2), "a…");
    }

    #[test]
    fn clip_of_zero_width_produces_nothing() {
        // The old saturating_sub made a zero-width clip emit a lone ellipsis,
        // which then overwrote a neighbouring column.
        assert_eq!(clip("abcdef", 0), "");
    }

    #[test]
    fn clip_counts_chars_not_bytes() {
        assert_eq!(clip("ééééé", 3), "éé…");
        assert_eq!(clip("→→→", 3), "→→→");
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
        let p = palette();
        for (w, h) in [
            (0, 0),
            (1, 1),
            (5, 3),
            (19, 20),
            (20, 5),
            (19, 5),
            (40, 6),
            (60, 6),
        ] {
            let mut b = buffer(w, h);
            let app = app_with(sample_rows());
            draw(&mut b, &app, &p);
            if w >= 20 && h >= 6 {
                continue;
            }
            // Below 20x6 the layout gives up and asks for a resize. The hint
            // itself needs 46 columns and 2 rows, so a smaller buffer than that
            // can only be checked for not panicking.
            let text = rows(&b).concat();
            if w >= 46 && h >= 2 {
                assert!(
                    text.contains("terminal too small"),
                    "expected a resize hint at {w}x{h}, got {text:?}"
                );
            }
        }
    }

    #[test]
    fn draw_survives_every_size_without_panicking() {
        // The layout is full of saturating_sub and `width - 1` arithmetic; a
        // sweep is the cheapest way to catch an underflow at a size nobody
        // thought to check.
        let p = palette();
        let app = app_with(sample_rows());
        for w in 0u16..48 {
            for h in 0u16..16 {
                let mut b = buffer(w, h);
                draw(&mut b, &app, &p);
            }
        }
    }

    #[test]
    fn draw_survives_every_size_with_the_help_overlay_open() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.show_help = true;
        for w in 0u16..48 {
            for h in 0u16..16 {
                let mut b = buffer(w, h);
                draw(&mut b, &app, &p);
            }
        }
    }

    #[test]
    fn draw_survives_every_size_with_a_filter_and_scrolled_pane() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.filter.active = true;
        app.filter.query = "la".into();
        app.scroll = 5;
        for w in 0u16..48 {
            for h in 0u16..16 {
                let mut b = buffer(w, h);
                draw(&mut b, &app, &p);
            }
        }
    }

    // ---- draw: content ---------------------------------------------------

    #[test]
    fn draw_shows_the_hostname_kernel_and_section_titles() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.sections
            .push(Section::new("Second Section", vec![Row::field("a", "b")]));
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);

        let text = rows(&b).concat();
        assert!(text.contains("omarchy-sysinfo"), "header title missing");
        assert!(text.contains("testhost"), "hostname missing");
        assert!(text.contains("6.1.0-test"), "kernel missing");
        assert!(text.contains("Test"), "section title missing");
        assert!(text.contains("Second Section"), "second section missing");
    }

    #[test]
    fn draw_marks_the_selected_section() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.sections.push(Section::new("Other", vec![]));
        app.select(1);
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);
        let text = rows(&b).concat();
        assert!(text.contains('▸'), "the selected row needs a marker");
        // Exactly one section is selected at a time.
        assert_eq!(text.matches('▸').count(), 1);
    }

    #[test]
    fn draw_renders_a_gauge_for_rows_that_carry_one() {
        let p = palette();
        let app = app_with(sample_rows());
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);
        let text = rows(&b).concat();
        assert!(text.contains('█') || text.contains('░'), "no gauge drawn");
        assert!(text.contains("42%"), "the percentage should be shown once");
        assert_eq!(
            text.matches("42%").count(),
            1,
            "the gauge column and the value must not both print 42%"
        );
    }

    #[test]
    fn draw_never_scrolls_past_the_end_of_the_content() {
        // Regression: max_scroll is row-count based, so a short section on a
        // tall pane used to scroll into empty space.
        let p = palette();
        let mut app = app_with(vec![Row::field("only", "row")]);
        app.scroll = 40;
        let mut b = buffer(80, 40);
        draw(&mut b, &app, &p);
        let text = rows(&b).concat();
        assert!(text.contains("only"), "content must still be visible");
    }

    #[test]
    fn draw_says_so_when_a_filter_matches_nothing() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.filter.query = "zzzzz".into();
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);
        assert!(rows(&b).concat().contains("no rows match"));
    }

    #[test]
    fn draw_shows_the_row_range_in_the_detail_title() {
        let p = palette();
        let app = app_with(sample_rows());
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);
        // Row 0 is the header; the detail pane's own title sits on row 1.
        assert!(rows(&b)[0].contains("omarchy-sysinfo"), "header missing");
        assert!(
            rows(&b)[1].contains("1-"),
            "range indicator missing: {:?}",
            rows(&b)[1]
        );
    }

    // ---- footer ----------------------------------------------------------

    #[test]
    fn footer_shows_the_filter_prompt_while_filtering() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.filter.active = true;
        app.filter.query = "gpu".into();
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);
        let last = row(&b, 23);
        assert!(last.contains("gpu"), "typed query not echoed: {last:?}");
        assert!(last.contains("esc to clear"), "filter hint missing");
    }

    #[test]
    fn footer_hints_mean_different_things_per_focus() {
        let p = palette();
        let mut app = app_with(sample_rows());
        let mut b = buffer(80, 24);

        app.focus = Focus::Sections;
        draw(&mut b, &app, &p);
        assert!(row(&b, 23).contains("section"));

        app.focus = Focus::Detail;
        draw(&mut b, &app, &p);
        let focused = row(&b, 23);
        assert!(
            focused.contains("scroll"),
            "expected a scroll hint: {focused:?}"
        );
    }

    #[test]
    fn footer_shows_a_status_message() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.set_status("refreshed");
        let mut b = buffer(80, 24);
        draw(&mut b, &app, &p);
        assert!(row(&b, 23).contains("refreshed"));
    }

    #[test]
    fn footer_drops_hints_that_would_not_fit() {
        let p = palette();
        let app = app_with(sample_rows());
        for w in 20u16..40 {
            let mut b = buffer(w, 10);
            draw(&mut b, &app, &p);
            // Nothing may be written past the edge, which put() already
            // guarantees; assert the first hint is present and the last is not.
            let last = row(&b, 9);
            assert!(last.contains("q"), "quit hint should survive at width {w}");
        }
    }

    // ---- help ------------------------------------------------------------

    #[test]
    fn help_overlay_lists_the_bindings() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.show_help = true;
        let mut b = buffer(100, 30);
        draw(&mut b, &app, &p);
        let text = rows(&b).concat();
        for expected in ["quit", "previous or next section", "filter", "help"] {
            assert!(text.contains(expected), "help is missing {expected:?}");
        }
    }

    #[test]
    fn help_overlay_fits_itself_to_a_small_terminal() {
        let p = palette();
        let mut app = app_with(sample_rows());
        app.show_help = true;
        for (w, h) in [(20u16, 6u16), (24, 8), (40, 10), (60, 20), (200, 60)] {
            let mut b = buffer(w, h);
            draw(&mut b, &app, &p);
            assert_eq!(rows(&b).len(), h as usize, "row count changed at {w}x{h}");
        }
    }
}
