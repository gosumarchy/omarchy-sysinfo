//! Terminal handling with no crates: raw mode via `stty`, size via
//! `TIOCGWINSZ`, and drawing with 24-bit ANSI escapes onto a cell buffer we
//! own.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use crate::collect::Bar;
use crate::text::char_width;

/// Leave the alternate screen with the cursor visible and colours reset.
const LEAVE: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1049l";
/// Enter the alternate screen, hide the cursor, clear, and home.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Color {
    Default,
    Rgb(u8, u8, u8),
}

impl Color {
    /// Append the SGR sequence that selects this colour, without allocating.
    ///
    /// `render` calls this for every run of equal style, so the two thirds of
    /// the time the colour is `Default` must not cost a `String` each.
    fn write_fg_escape(self, out: &mut String) {
        match self {
            Color::Default => out.push_str("\x1b[39m"),
            Color::Rgb(r, g, b) => {
                let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
            }
        }
    }

    fn write_bg_escape(self, out: &mut String) {
        match self {
            Color::Default => out.push_str("\x1b[49m"),
            Color::Rgb(r, g, b) => {
                let _ = write!(out, "\x1b[48;2;{r};{g};{b}m");
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Style {
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    pub(crate) bold: bool,
    pub(crate) dim: bool,
}

impl Style {
    pub(crate) fn new(fg: Color) -> Style {
        Style {
            fg,
            bg: Color::Default,
            bold: false,
            dim: false,
        }
    }

    pub(crate) fn on(mut self, bg: Color) -> Style {
        self.bg = bg;
        self
    }

    pub(crate) fn bold(mut self) -> Style {
        self.bold = true;
        self
    }

    pub(crate) fn dim(mut self) -> Style {
        self.dim = true;
        self
    }
}

/// Marks the second cell of a double-width character. The terminal draws the
/// character across both cells, so `render` must print nothing for this one.
/// NUL can never be real content: every string is sanitised before it gets
/// here.
const CONTINUATION: char = '\0';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell {
    ch: char,
    style: Style,
}

/// A grid of styled characters that we render in one pass.
#[derive(Debug)]
pub(crate) struct Buffer {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
    blank: Style,
}

impl Buffer {
    pub(crate) fn new(width: u16, height: u16, blank: Style) -> Buffer {
        Buffer {
            width,
            height,
            cells: vec![
                Cell {
                    ch: ' ',
                    style: blank
                };
                usize::from(width) * usize::from(height)
            ],
            blank,
        }
    }

    pub(crate) fn width(&self) -> u16 {
        self.width
    }

    pub(crate) fn height(&self) -> u16 {
        self.height
    }

    /// Draw `text` at `x,y`, clipped to the buffer. Returns the x after the
    /// text.
    ///
    /// Wide characters take two cells and zero-width ones none, so the
    /// returned column is where the terminal's cursor will really be. A wide
    /// character that would straddle the right edge is drawn as a space: half
    /// of it would make the terminal wrap and shift the whole frame.
    pub(crate) fn put(&mut self, x: u16, y: u16, text: &str, style: Style) -> u16 {
        if y >= self.height {
            return x;
        }

        let mut cursor = x;
        for ch in text.chars() {
            if cursor >= self.width {
                break;
            }
            match char_width(ch) {
                0 => {}
                1 => {
                    self.set(cursor, y, ch, style);
                    cursor += 1;
                }
                _ if cursor + 1 < self.width => {
                    self.set(cursor, y, ch, style);
                    self.set(cursor + 1, y, CONTINUATION, style);
                    cursor += 2;
                }
                _ => {
                    self.set(cursor, y, ' ', style);
                    cursor += 1;
                }
            }
        }

        cursor
    }

    /// Draw `text` right-aligned so it ends at `x` (exclusive).
    pub(crate) fn put_right(&mut self, x: u16, y: u16, text: &str, style: Style) {
        let len = u16::try_from(crate::text::width(text)).unwrap_or(u16::MAX);
        if len >= x {
            return;
        }

        self.put(x - len, y, text, style);
    }

    pub(crate) fn fill_row(&mut self, y: u16, ch: char, style: Style) {
        if y >= self.height {
            return;
        }
        for x in 0..self.width {
            self.set(x, y, ch, style);
        }
    }

    /// Put one character in one cell, keeping wide characters whole.
    ///
    /// Writing over either half of a wide character would leave the other
    /// half orphaned, and the terminal would draw the survivor across a cell
    /// that now belongs to something else. The orphan is blanked instead.
    fn set(&mut self, x: u16, y: u16, ch: char, style: Style) {
        if x >= self.width || y >= self.height {
            return;
        }
        let index = self.index(x, y);

        if self.cells[index].ch == CONTINUATION && ch != CONTINUATION && x > 0 {
            self.cells[index - 1] = Cell {
                ch: ' ',
                style: self.blank,
            };
        }
        if x + 1 < self.width && self.cells[index + 1].ch == CONTINUATION {
            self.cells[index + 1] = Cell {
                ch: ' ',
                style: self.blank,
            };
        }

        self.cells[index] = Cell { ch, style };
    }

    /// Where `(x, y)` lands in the flat cell vector.
    fn index(&self, x: u16, y: u16) -> usize {
        usize::from(y) * usize::from(self.width) + usize::from(x)
    }

    /// Horizontal rule across `x..end` on row `y`.
    pub(crate) fn hline(&mut self, x: u16, y: u16, end: u16, ch: char, style: Style) {
        for cursor in x..end.min(self.width) {
            self.set(cursor, y, ch, style);
        }
    }

    /// Vertical rule down column `x` from `y` to `y_end`.
    pub(crate) fn vline(&mut self, x: u16, y: u16, y_end: u16, ch: char, style: Style) {
        for row in y..y_end.min(self.height) {
            self.set(x, row, ch, style);
        }
    }

    /// A bar `width` cells long, the fraction taken from `bar`.
    ///
    /// The clamp lives in `Bar::new`, so this cannot be handed a ratio outside
    /// `0.0..=1.0` and does not repeat the check.
    pub(crate) fn gauge(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        bar: Bar,
        style: Style,
        empty: Style,
    ) {
        let filled = crate::collect::units::round_u64(bar.frac() * f64::from(width));

        for offset in 0..width {
            let Some(column) = x.checked_add(offset) else {
                break;
            };
            if u64::from(offset) < filled {
                self.set(column, y, '█', style);
            } else {
                self.set(column, y, '░', empty);
            }
        }
    }

    /// Serialise to a string of ANSI escapes, skipping runs of equal style.
    pub(crate) fn render(&self) -> String {
        let mut out = String::with_capacity(self.cells.len() * 8);
        out.push_str("\x1b[H");
        let mut current: Option<Style> = None;

        // `chunks` walks the rows without recomputing `y * width + x` per cell.
        // The width is forced to at least one because a zero-width buffer has
        // no rows to hand out, and `chunks(0)` panics.
        for (y, cells) in self
            .cells
            .chunks(usize::from(self.width.max(1)))
            .enumerate()
        {
            if y > 0 {
                out.push_str("\r\n\x1b[K");
            }
            for cell in cells {
                if cell.ch == CONTINUATION {
                    continue;
                }
                if current != Some(cell.style) {
                    if current.is_some() {
                        out.push_str("\x1b[0m");
                    }
                    cell.style.fg.write_fg_escape(&mut out);
                    cell.style.bg.write_bg_escape(&mut out);
                    if cell.style.bold {
                        out.push_str("\x1b[1m");
                    }
                    if cell.style.dim {
                        out.push_str("\x1b[2m");
                    }
                    current = Some(cell.style);
                }
                out.push(cell.ch);
            }
            if current.is_some() {
                out.push_str("\x1b[0m");
                current = None;
            }
        }

        out
    }
}

/// Owns raw mode and the alternate screen, and gives both back on drop, on
/// panic, and on a failure halfway through entering them.
#[derive(Debug)]
pub(crate) struct Terminal {
    restore: Restore,
    size: (u16, u16),
}

/// What it takes to put the terminal back, shared with the panic hook.
///
/// The release profile aborts on panic, so `Drop` never runs then; the hook
/// is the only chance to leave the user a working shell. `Option::take`
/// makes restoring idempotent, since in a debug build both run.
#[derive(Clone, Debug)]
struct Restore(Arc<Mutex<Option<String>>>);

impl Restore {
    fn new(saved_stty: String) -> Restore {
        Restore(Arc::new(Mutex::new(Some(saved_stty))))
    }

    fn run(&self) {
        let saved = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(saved) = saved else {
            return;
        };

        let mut out = io::stdout();
        let _ = out.write_all(LEAVE);
        let _ = out.flush();
        if !saved.is_empty() {
            let _ = stty(&[&saved]);
        }
    }
}

impl Terminal {
    /// Switch to raw mode and the alternate screen.
    ///
    /// The guard exists before the first change is made, so an error on any
    /// later step (a closed stdout, say) still restores the terminal on the
    /// way out, instead of leaving the shell without echo.
    pub(crate) fn enter() -> io::Result<Terminal> {
        let saved = stty(&["-g"])
            .ok_or_else(|| io::Error::other("stty could not read the terminal settings"))?;
        let restore = Restore::new(saved);
        let terminal = Terminal {
            restore: restore.clone(),
            size: terminal_size(),
        };

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore.run();
            previous(info);
        }));

        // Raw mode with no echo; the shell is restored on the way out.
        stty(&["raw", "-echo"]).ok_or_else(|| io::Error::other("stty could not set raw mode"))?;
        let mut out = io::stdout();
        out.write_all(ENTER)?;
        out.flush()?;

        Ok(terminal)
    }

    pub(crate) fn size(&self) -> (u16, u16) {
        self.size
    }

    pub(crate) fn refresh_size(&mut self) {
        self.size = terminal_size();
    }

    #[expect(
        clippy::unused_self,
        reason = "owning the Terminal is what proves raw mode and the alternate screen are on"
    )]
    pub(crate) fn draw(&mut self, buffer: &Buffer) -> io::Result<()> {
        let mut out = io::stdout().lock();

        out.write_all(buffer.render().as_bytes())?;
        out.flush()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.restore.run();
    }
}

/// Run `stty` against our terminal and return what it printed. `Some("")`
/// is success with no output.
fn stty(args: &[&str]) -> Option<String> {
    let out = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Width and height: `TIOCGWINSZ`, then `stty size`, then the environment.
///
/// The ioctl is the path a redraw takes. `stty` stays only for a terminal that
/// will not answer the syscall, so a resize still cannot fork on every frame.
pub(crate) fn terminal_size() -> (u16, u16) {
    if let Some(size) = [0, 1].into_iter().find_map(crate::sys::window_size) {
        return size;
    }
    if let Some(size) = stty(&["size"]).and_then(|out| parse_size(&out)) {
        return size;
    }

    (env_dim("COLUMNS", 100), env_dim("LINES", 30))
}

fn size_if_nonzero(cols: u16, rows: u16) -> Option<(u16, u16)> {
    (cols > 0 && rows > 0).then_some((cols, rows))
}

/// `stty size` prints rows then columns, but we want width first.
fn parse_size(text: &str) -> Option<(u16, u16)> {
    let mut parts = text.split_whitespace();
    let rows = parts.next()?.parse::<u16>().ok()?;
    let cols = parts.next()?.parse::<u16>().ok()?;

    size_if_nonzero(cols, rows)
}

/// Environment dimensions are only a hint, so a zero or unparsable value must
/// not win: `COLUMNS=0` would otherwise produce a buffer nothing can draw into
/// and the app would render a blank screen forever.
fn env_dim(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::new(w, h, Style::new(Color::Default))
    }

    /// The visible characters of one row, ignoring style.
    fn row(b: &Buffer, y: u16) -> String {
        (0..b.width)
            .map(|x| b.cells[b.index(x, y)].ch)
            .filter(|ch| *ch != CONTINUATION)
            .collect()
    }

    fn cell(b: &Buffer, x: u16, y: u16) -> Cell {
        b.cells[b.index(x, y)]
    }

    // ---- construction ----------------------------------------------------

    #[test]
    fn new_buffer_reports_its_dimensions_and_starts_blank() {
        let b = buf(10, 4);
        assert_eq!((b.width(), b.height()), (10, 4));
        for y in 0..4 {
            assert_eq!(row(&b, y), "          ");
        }
    }

    #[test]
    fn new_buffer_applies_the_blank_style_to_every_cell() {
        let blank = Style::new(Color::Rgb(1, 2, 3)).on(Color::Rgb(4, 5, 6));
        let b = Buffer::new(2, 2, blank);
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(cell(&b, x, y).style, blank);
            }
        }
    }

    #[test]
    fn degenerate_buffers_are_usable_without_panicking() {
        // A terminal can report 0x0 while it is being resized, and the UI must
        // survive drawing into it rather than dividing by zero or panicking.
        let b = buf(0, 0);
        assert_eq!(b.render(), "\x1b[H");
        let mut b = buf(0, 5);
        assert_eq!(b.put(0, 0, "hi", Style::new(Color::Default)), 0);
        let mut b = buf(5, 0);
        assert_eq!(b.put(0, 0, "hi", Style::new(Color::Default)), 0);
    }

    // ---- put -------------------------------------------------------------

    #[test]
    fn put_writes_text_and_returns_the_column_after_it() {
        let mut b = buf(10, 2);
        let end = b.put(2, 0, "abc", Style::new(Color::Default));
        assert_eq!(end, 5);
        assert_eq!(row(&b, 0), "  abc     ");
    }

    #[test]
    fn put_stops_at_the_right_edge_and_returns_the_width() {
        let mut b = buf(5, 1);
        let end = b.put(3, 0, "abcdefgh", Style::new(Color::Default));
        assert_eq!(end, 5, "cursor must stop at the edge");
        assert_eq!(row(&b, 0), "   ab");
    }

    #[test]
    fn put_entirely_off_the_right_edge_writes_nothing() {
        let mut b = buf(5, 1);
        assert_eq!(b.put(9, 0, "ab", Style::new(Color::Default)), 9);
        assert_eq!(row(&b, 0), "     ");
    }

    #[test]
    fn put_below_the_buffer_writes_nothing_and_does_not_panic() {
        let mut b = buf(5, 1);
        assert_eq!(b.put(0, 7, "ab", Style::new(Color::Default)), 0);
        assert_eq!(row(&b, 0), "     ");
    }

    #[test]
    fn put_overwrites_rather_than_inserting() {
        let mut b = buf(6, 1);
        b.put(0, 0, "abcdef", Style::new(Color::Default));
        b.put(2, 0, "XY", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "abXYef");
    }

    #[test]
    fn put_advances_one_cell_per_char_for_the_glyphs_the_ui_uses() {
        // Box-drawing and bar glyphs are single-width, which is what the layout
        // math in ui/mod.rs assumes.
        let mut b = buf(8, 1);
        b.put(0, 0, "─├┤┤█░", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "─├┤┤█░  ");
    }

    // ---- put_right -------------------------------------------------------

    #[test]
    fn put_right_aligns_so_the_text_ends_before_x() {
        let mut b = buf(10, 1);
        b.put_right(8, 0, "42", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "      42  ");
    }

    #[test]
    fn put_right_draws_nothing_when_the_text_does_not_fit() {
        let mut b = buf(4, 1);
        b.put_right(2, 0, "abcd", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "    ");
    }

    #[test]
    fn put_right_with_empty_text_is_a_no_op() {
        let mut b = buf(4, 1);
        b.put_right(2, 0, "", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "    ");
    }

    #[test]
    fn put_right_measures_in_chars_not_bytes() {
        let mut b = buf(10, 1);
        b.put_right(6, 0, "é→", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "    é→    ");
    }

    // ---- fill_row / hline / vline ---------------------------------------

    #[test]
    fn fill_row_paints_the_whole_width() {
        let mut b = buf(4, 2);
        b.fill_row(0, '#', Style::new(Color::Default));
        assert_eq!(row(&b, 0), "####");
        assert_eq!(row(&b, 1), "    ", "other rows untouched");
    }

    #[test]
    fn fill_row_below_the_buffer_is_ignored() {
        let mut b = buf(4, 1);
        b.fill_row(3, '#', Style::new(Color::Default));
        assert_eq!(row(&b, 0), "    ");
    }

    #[test]
    fn hline_covers_the_given_range_only() {
        let mut b = buf(6, 1);
        b.hline(1, 0, 4, '-', Style::new(Color::Default));
        assert_eq!(row(&b, 0), " ---  ");
    }

    #[test]
    fn hline_stops_at_the_buffer_edge_when_end_is_past_it() {
        let mut b = buf(4, 1);
        b.hline(2, 0, 99, '-', Style::new(Color::Default));
        assert_eq!(row(&b, 0), "  --");
    }

    #[test]
    fn hline_with_end_before_start_draws_nothing() {
        let mut b = buf(6, 1);
        b.hline(4, 0, 2, '-', Style::new(Color::Default));
        assert_eq!(row(&b, 0), "      ");
    }

    #[test]
    fn vline_covers_the_given_rows_only() {
        let mut b = buf(3, 4);
        b.vline(1, 1, 3, '|', Style::new(Color::Default));
        assert_eq!(row(&b, 0), "   ");
        assert_eq!(row(&b, 1), " | ");
        assert_eq!(row(&b, 2), " | ");
        assert_eq!(row(&b, 3), "   ");
    }

    #[test]
    fn vline_stops_at_the_bottom_edge() {
        let mut b = buf(2, 2);
        b.vline(0, 0, 99, '|', Style::new(Color::Default));
        assert_eq!(row(&b, 0), "| ");
        assert_eq!(row(&b, 1), "| ");
    }

    // ---- gauge -----------------------------------------------------------

    #[test]
    fn gauge_splits_filled_from_empty_at_the_fraction() {
        let mut b = buf(8, 1);
        b.gauge(
            0,
            0,
            4,
            Bar::new(0.5),
            Style::new(Color::Default),
            Style::new(Color::Default),
        );
        assert_eq!(row(&b, 0), "██░░    ");
    }

    #[test]
    fn gauge_rounds_the_filled_count() {
        let cells = |frac: f64| {
            let mut b = buf(10, 1);
            b.gauge(
                0,
                0,
                4,
                Bar::new(frac),
                Style::new(Color::Default),
                Style::new(Color::Default),
            );
            row(&b, 0).trim_end().to_string()
        };
        assert_eq!(cells(0.0), "░░░░");
        assert_eq!(cells(0.01), "░░░░", "below half a cell rounds down");
        assert_eq!(cells(0.13), "█░░░", "half a cell rounds up");
        assert_eq!(cells(0.5), "██░░");
        assert_eq!(cells(1.0), "████");
    }

    #[test]
    fn a_gauge_built_from_a_nonsense_fraction_still_paints_a_sane_bar() {
        // `gauge` takes a `Bar` and so cannot be handed a raw ratio; the
        // clamping is `Bar::new`'s job. Checked here anyway, because this is
        // the path a mis-parsed sensor reading actually takes.
        let cells = |frac: f64| {
            let mut b = buf(10, 1);
            b.gauge(
                0,
                0,
                3,
                Bar::new(frac),
                Style::new(Color::Default),
                Style::new(Color::Default),
            );
            row(&b, 0).trim_end().to_string()
        };
        assert_eq!(cells(-1.0), "░░░");
        assert_eq!(cells(2.0), "███");
        assert_eq!(cells(f64::NAN), "░░░", "NaN must not paint a solid bar");
        assert_eq!(cells(f64::INFINITY), "███");
    }

    #[test]
    fn gauge_styles_filled_and_empty_cells_differently() {
        let full = Style::new(Color::Rgb(9, 9, 9));
        let empty = Style::new(Color::Rgb(1, 1, 1));
        let mut b = buf(4, 1);
        b.gauge(0, 0, 2, Bar::new(0.5), full, empty);
        assert_eq!(cell(&b, 0, 0).style, full);
        assert_eq!(cell(&b, 1, 0).style, empty);
    }

    #[test]
    fn gauge_past_the_right_edge_is_clipped() {
        let mut b = buf(3, 1);
        b.gauge(
            2,
            0,
            8,
            Bar::new(1.0),
            Style::new(Color::Default),
            Style::new(Color::Default),
        );
        assert_eq!(row(&b, 0), "  █");
    }

    // ---- style / color ---------------------------------------------------

    #[test]
    fn style_builders_compose() {
        let s = Style::new(Color::Rgb(1, 2, 3))
            .on(Color::Rgb(4, 5, 6))
            .bold()
            .dim();
        assert_eq!(s.fg, Color::Rgb(1, 2, 3));
        assert_eq!(s.bg, Color::Rgb(4, 5, 6));
        assert!(s.bold);
        assert!(s.dim);
    }

    #[test]
    fn color_escapes_are_24_bit_or_the_default() {
        let fg = |c: Color| {
            let mut out = String::new();
            c.write_fg_escape(&mut out);
            out
        };
        let bg = |c: Color| {
            let mut out = String::new();
            c.write_bg_escape(&mut out);
            out
        };
        assert_eq!(fg(Color::Rgb(10, 20, 30)), "\x1b[38;2;10;20;30m");
        assert_eq!(bg(Color::Rgb(10, 20, 30)), "\x1b[48;2;10;20;30m");
        assert_eq!(fg(Color::Default), "\x1b[39m");
        assert_eq!(bg(Color::Default), "\x1b[49m");
    }

    #[test]
    fn an_escape_appends_to_whatever_is_already_in_the_string() {
        let mut out = String::from("x");
        Color::Rgb(1, 2, 3).write_fg_escape(&mut out);
        assert_eq!(out, "x\x1b[38;2;1;2;3m");
    }

    // ---- render ----------------------------------------------------------

    #[test]
    fn render_homes_the_cursor_and_emits_every_cell() {
        let mut b = buf(3, 2);
        b.put(0, 0, "ab", Style::new(Color::Default));
        b.put(0, 1, "cd", Style::new(Color::Default));
        let out = b.render();
        assert!(out.starts_with("\x1b[H"), "must home the cursor first");
        assert!(out.contains("ab"), "row 0 text missing: {out:?}");
        assert!(out.contains("cd"), "row 1 text missing: {out:?}");
        assert_eq!(out.matches('\r').count(), 1, "one newline between two rows");
    }

    #[test]
    fn render_ends_every_row_with_a_reset() {
        // Without the per-row reset, style would bleed into the next row.
        let b = buf(2, 3);
        let out = b.render();
        assert_eq!(out.matches("\x1b[0m").count(), 3);
    }

    #[test]
    fn render_collapses_runs_of_equal_style() {
        let style = Style::new(Color::Rgb(1, 1, 1));
        let b = Buffer::new(20, 1, style);
        let out = b.render();
        assert_eq!(
            out.matches("\x1b[38;2;1;1;1m").count(),
            1,
            "a uniform row must not re-emit its colour per cell: {out:?}"
        );
    }

    #[test]
    fn render_switches_style_when_the_colour_changes() {
        let mut b = buf(4, 1);
        b.put(0, 0, "a", Style::new(Color::Rgb(1, 0, 0)));
        b.put(1, 0, "b", Style::new(Color::Rgb(2, 0, 0)));
        let out = b.render();
        assert!(out.contains("\x1b[38;2;1;0;0m"));
        assert!(out.contains("\x1b[38;2;2;0;0m"));
        assert!(
            out.contains("\x1b[0m"),
            "must reset before switching colour"
        );
    }

    #[test]
    fn render_emits_bold_and_dim_attributes() {
        let mut b = buf(2, 1);
        b.put(0, 0, "h", Style::new(Color::Default).bold().dim());
        let out = b.render();
        assert!(out.contains("\x1b[1m"), "bold attribute missing");
        assert!(out.contains("\x1b[2m"), "dim attribute missing");
    }

    #[test]
    fn render_of_an_empty_buffer_is_just_the_home_sequence() {
        assert_eq!(buf(0, 0).render(), "\x1b[H");
    }

    // ---- size parsing ----------------------------------------------------

    #[test]
    fn parse_size_swaps_rows_and_columns() {
        // `stty size` prints "rows cols"; we want (width, height).
        assert_eq!(parse_size("24 80"), Some((80, 24)));
        assert_eq!(parse_size(" 50  120 \n"), Some((120, 50)));
    }

    #[test]
    fn a_zero_dimension_is_not_a_terminal_size() {
        assert_eq!(size_if_nonzero(80, 24), Some((80, 24)));
        assert_eq!(size_if_nonzero(0, 24), None);
        assert_eq!(size_if_nonzero(80, 0), None);
    }

    #[test]
    fn parse_size_rejects_junk_and_degenerate_values() {
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("80"), None, "missing column count");
        assert_eq!(parse_size("x y"), None);
        assert_eq!(parse_size("0 80"), None, "zero rows");
        assert_eq!(parse_size("24 0"), None, "zero columns");
        assert_eq!(parse_size("-1 80"), None);
        assert_eq!(parse_size("99999 80"), None, "out of u16 range");
    }

    // ---- wide characters -----------------------------------------------------

    #[test]
    fn a_wide_character_takes_two_cells() {
        let mut b = buf(6, 1);
        let end = b.put(0, 0, "日本", Style::new(Color::Default));

        assert_eq!(end, 4);
        assert_eq!(row(&b, 0), "日本  ");
        assert_eq!(cell(&b, 1, 0).ch, CONTINUATION);
    }

    #[test]
    fn a_wide_character_that_would_straddle_the_edge_becomes_a_space() {
        let mut b = buf(3, 1);
        let end = b.put(0, 0, "a日本", Style::new(Color::Default));

        assert_eq!(end, 3);
        assert_eq!(row(&b, 0), "a日");
        let mut b = buf(2, 1);
        b.put(1, 0, "日", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "  ", "half a wide char would wrap the line");
    }

    #[test]
    fn a_combining_mark_takes_no_cell() {
        let mut b = buf(4, 1);

        assert_eq!(b.put(0, 0, "e\u{301}x", Style::new(Color::Default)), 2);
    }

    #[test]
    fn overwriting_half_of_a_wide_character_blanks_the_other_half() {
        let mut b = buf(4, 1);
        b.put(0, 0, "日", Style::new(Color::Default));
        b.put(1, 0, "x", Style::new(Color::Default));
        assert_eq!(row(&b, 0), " x  ");

        let mut b = buf(4, 1);
        b.put(0, 0, "日", Style::new(Color::Default));
        b.put(0, 0, "y", Style::new(Color::Default));
        assert_eq!(row(&b, 0), "y   ");
    }

    #[test]
    fn render_skips_the_continuation_cell() {
        let mut b = buf(3, 1);
        b.put(0, 0, "日x", Style::new(Color::Default));
        let out = b.render();

        assert!(out.contains("日x"), "{out:?}");
        assert!(!out.contains('\0'), "{out:?}");
    }

    #[test]
    fn put_right_measures_display_width() {
        let mut b = buf(6, 1);
        b.put_right(6, 0, "日", Style::new(Color::Default));

        assert_eq!(row(&b, 0), "    日");
    }

    // ---- restore -------------------------------------------------------------

    #[test]
    fn restoring_twice_is_harmless() {
        // The panic hook and Drop both restore in a debug build.
        let restore = Restore::new(String::new());
        restore.run();
        restore.run();

        assert!(restore.0.lock().expect("lock").is_none());
    }
}
