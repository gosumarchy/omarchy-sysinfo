//! The TUI's state: which section is selected, what the detail pane shows,
//! and what a key press does to it. Nothing here touches the terminal or the
//! machine; it only reacts to keys and to snapshots from the collector.

use std::time::{Duration, Instant};

use crate::collect::{Row, Section, Snapshot};
use crate::event::Trigger;
use crate::input::Key;

/// How long a status message stays in the footer.
const STATUS_TTL: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Focus {
    Sections,
    Detail,
}

/// What keys currently mean. One field instead of two booleans, so the app
/// cannot be filtering and showing help at the same time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Browse,
    /// Typing a filter query: every printable key goes into it.
    Filter,
    Help,
}

/// What the main loop must do after a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    Nothing,
    Quit,
    Refresh,
}

/// Text filter applied to the detail pane, typed with `/`.
///
/// The lowercased needle is kept alongside the query, so matching a row does
/// not lowercase the query again for every row on every frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    query: String,
    needle: String,
}

impl Filter {
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    fn push(&mut self, c: char) {
        self.query.push(c);
        self.needle = self.query.to_lowercase();
    }

    fn pop(&mut self) {
        self.query.pop();
        self.needle = self.query.to_lowercase();
    }

    fn clear(&mut self) {
        self.query.clear();
        self.needle.clear();
    }

    pub(crate) fn matches(&self, row: &Row) -> bool {
        if self.needle.is_empty() {
            return true;
        }

        let hit = |text: &str| text.to_lowercase().contains(&self.needle);
        match row {
            Row::Header(text) | Row::Note(text) => hit(text),
            Row::Field { label, value, .. } => hit(label) || hit(value),
            Row::Blank => false,
        }
    }
}

#[derive(Debug)]
struct Status {
    text: &'static str,
    at: Instant,
}

#[derive(Debug)]
pub(crate) struct App {
    snapshot: Option<Snapshot>,
    selected: usize,
    focus: Focus,
    mode: Mode,
    /// Index of the first visible detail row.
    scroll: usize,
    /// How many detail rows fit on screen. The UI knows, and tells the app
    /// before every frame, so the scroll limit is "last row at the bottom"
    /// rather than "last row at the top".
    viewport: usize,
    filter: Filter,
    status: Option<Status>,
    refresh_pending: bool,
}

impl App {
    pub(crate) fn new() -> App {
        App {
            snapshot: None,
            selected: 0,
            focus: Focus::Sections,
            mode: Mode::Browse,
            scroll: 0,
            viewport: 1,
            filter: Filter::default(),
            status: None,
            refresh_pending: false,
        }
    }

    // ---- what the UI reads ----------------------------------------------

    pub(crate) fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub(crate) fn sections(&self) -> &[Section] {
        self.snapshot
            .as_ref()
            .map_or(&[], |s| s.sections.as_slice())
    }

    pub(crate) fn section(&self) -> Option<&Section> {
        self.sections().get(self.selected)
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn focus(&self) -> Focus {
        self.focus
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn filter(&self) -> &Filter {
        &self.filter
    }

    pub(crate) fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text)
    }

    /// The selected section's rows that pass the filter.
    pub(crate) fn visible_rows(&self) -> Vec<&Row> {
        self.section()
            .map(|s| s.rows.iter().filter(|r| self.filter.matches(r)).collect())
            .unwrap_or_default()
    }

    // ---- what the main loop feeds in ------------------------------------

    /// Take a fresh snapshot, keeping the selection and scroll where they
    /// were as far as the new data allows.
    ///
    /// Only a snapshot the worker took because of a refresh request says
    /// "refreshed"; one that was already being collected when `r` was
    /// pressed would claim fresh data it does not have.
    pub(crate) fn on_snapshot(&mut self, snapshot: Snapshot, trigger: Trigger) {
        self.snapshot = Some(snapshot);
        self.selected = self.selected.min(self.sections().len().saturating_sub(1));
        self.clamp_scroll();

        if self.refresh_pending && trigger == Trigger::Request {
            self.refresh_pending = false;
            self.set_status("refreshed");
        }
    }

    /// How many detail rows the UI can show; called before every frame so a
    /// resize is picked up at once.
    pub(crate) fn set_viewport(&mut self, rows: usize) {
        self.viewport = rows.max(1);
        self.clamp_scroll();
    }

    /// Expire the status message. Returns true when the screen changed.
    pub(crate) fn tick(&mut self) -> bool {
        // A status line that never goes away stops being information.
        if self
            .status
            .as_ref()
            .is_some_and(|s| s.at.elapsed() > STATUS_TTL)
        {
            self.status = None;

            return true;
        }

        false
    }

    pub(crate) fn on_key(&mut self, key: Key) -> Action {
        // Ctrl+C is an exit request wherever it lands, including mid-query:
        // swallowing it inside the filter made the app look hung.
        if key == Key::CtrlC {
            return Action::Quit;
        }

        match self.mode {
            Mode::Filter => {
                self.filter_key(key);

                Action::Nothing
            }
            Mode::Help | Mode::Browse => self.browse_key(key),
        }
    }

    fn filter_key(&mut self, key: Key) {
        match key {
            Key::Esc => {
                self.mode = Mode::Browse;
                self.filter.clear();
                self.status = None;
            }
            Key::Enter => self.mode = Mode::Browse,
            Key::Backspace => self.filter.pop(),
            // The query is drawn in the footer, so a control character (a C1
            // code point arrives as ordinary UTF-8) must never get into it.
            Key::Char(c) if !c.is_control() => self.filter.push(c),
            Key::Char(_)
            | Key::Alt(_)
            | Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Tab
            | Key::CtrlC
            | Key::Unknown => return,
        }

        // The query changed, so the old scroll position means nothing.
        self.scroll = 0;
    }

    fn browse_key(&mut self, key: Key) -> Action {
        match key {
            // Esc backs out one layer at a time, then quits.
            Key::Esc if self.mode == Mode::Help => self.mode = Mode::Browse,
            Key::Esc | Key::CtrlC => return Action::Quit,
            Key::Down => self.select_relative(1),
            Key::Up => self.select_relative(-1),
            Key::Home => self.select(0),
            Key::End => self.select_last(),
            Key::PageDown => self.scroll_by(self.page()),
            Key::PageUp => self.scroll_by(-self.page()),
            // Panes are addressed the same way in every direction: left is the
            // sidebar, right is the detail, tab swaps between them.
            Key::Left => self.focus = Focus::Sections,
            Key::Right => self.focus = Focus::Detail,
            Key::Tab => self.toggle_focus(),
            Key::Char(c) => return self.char_key(c),
            Key::Alt(_) | Key::Enter | Key::Backspace | Key::Unknown => {}
        }

        Action::Nothing
    }

    fn char_key(&mut self, c: char) -> Action {
        match c {
            'q' => return Action::Quit,
            'j' => self.select_relative(1),
            'k' => self.select_relative(-1),
            'g' => self.select(0),
            'G' => self.select_last(),
            'h' => self.focus = Focus::Sections,
            'l' => self.focus = Focus::Detail,
            'r' => {
                self.refresh_pending = true;
                self.set_status("refreshing…");

                return Action::Refresh;
            }
            '/' => self.mode = Mode::Filter,
            '?' => {
                self.mode = match self.mode {
                    Mode::Help => Mode::Browse,
                    Mode::Browse | Mode::Filter => Mode::Help,
                };
            }
            // Every other character is unbound.
            _ => {}
        }

        Action::Nothing
    }

    // ---- movement ---------------------------------------------------------

    fn select(&mut self, index: usize) {
        if index < self.sections().len() {
            self.selected = index;
            self.scroll = 0;
        }
    }

    fn select_last(&mut self) {
        if let Some(last) = self.sections().len().checked_sub(1) {
            self.select(last);
        }
    }

    fn move_by(&mut self, delta: isize) {
        // With no sections loaded there is nowhere to move.
        let Some(last) = self.sections().len().checked_sub(1) else {
            return;
        };

        self.select(self.selected.saturating_add_signed(delta).min(last));
    }

    /// On the sidebar this changes section; in the detail pane it scrolls.
    fn select_relative(&mut self, delta: isize) {
        match self.focus {
            Focus::Sections => self.move_by(delta),
            Focus::Detail => self.scroll_by(delta),
        }
    }

    /// A page is a screenful, less one row of overlap for context.
    fn page(&self) -> isize {
        isize::try_from(self.viewport.saturating_sub(1).max(1)).unwrap_or(isize::MAX)
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
        self.clamp_scroll();
    }

    fn max_scroll(&self) -> usize {
        self.visible_rows().len().saturating_sub(self.viewport)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sections => Focus::Detail,
            Focus::Detail => Focus::Sections,
        };
    }

    fn set_status(&mut self, text: &'static str) {
        self.status = Some(Status {
            text,
            at: Instant::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Text;

    const VIEWPORT: usize = 10;

    fn snapshot(sections: Vec<Section>) -> Snapshot {
        Snapshot {
            sections,
            hostname: Text::new("testhost"),
            kernel: Text::new("6.1.0-test"),
            uptime: 0,
        }
    }

    /// An App with three 25-row sections and a 10-row viewport, so the detail
    /// pane can scroll by exactly 15.
    fn app() -> App {
        let many = |prefix: &str| {
            (0..25)
                .map(|i| Row::field(format!("{prefix} {i}"), format!("v{i}")))
                .collect::<Vec<Row>>()
        };
        let mut app = App::new();
        app.on_snapshot(
            snapshot(vec![
                Section::new("One", many("one")),
                Section::new("Two", many("two")),
                Section::new("Three", many("three")),
            ]),
            Trigger::Timer,
        );
        app.set_viewport(VIEWPORT);
        app
    }

    fn filter(query: &str) -> Filter {
        let mut f = Filter::default();
        for c in query.chars() {
            f.push(c);
        }
        f
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.on_key(Key::Char(c));
        }
    }

    // ---- Filter::matches -------------------------------------------------

    #[test]
    fn an_empty_filter_matches_every_row_including_blanks() {
        let f = Filter::default();
        assert!(f.matches(&Row::header("anything")));
        assert!(f.matches(&Row::field("l", "v")));
        assert!(f.matches(&Row::note("n")));
        assert!(f.matches(&Row::Blank));
    }

    #[test]
    fn filter_searches_labels_values_headers_and_notes() {
        let f = filter("gpu");
        assert!(f.matches(&Row::field("GPU", "whatever")));
        assert!(f.matches(&Row::field("label", "Intel GPU")));
        assert!(f.matches(&Row::header("GPU section")));
        assert!(f.matches(&Row::note("no gpu here")));
        assert!(!f.matches(&Row::field("Memory", "16 GiB")));
    }

    #[test]
    fn filter_is_case_insensitive() {
        assert!(filter("AmD").matches(&Row::field("gpu", "AMD Radeon 780M")));
        assert!(filter("amd").matches(&Row::field("GPU", "AMD Radeon 780M")));
    }

    #[test]
    fn filter_hides_blank_rows_once_a_query_is_typed() {
        // A blank separator row matches nothing, so a query never leaves stray
        // gaps in the output.
        assert!(!filter("a").matches(&Row::Blank));
    }

    #[test]
    fn popping_the_last_character_matches_everything_again() {
        let mut f = filter("x");
        f.pop();
        assert!(f.matches(&Row::field("anything", "at all")));
    }

    // ---- before the first snapshot -----------------------------------------

    #[test]
    fn an_app_without_a_snapshot_is_inert_rather_than_panicking() {
        let mut a = App::new();

        assert!(a.section().is_none());
        assert!(a.visible_rows().is_empty());
        for key in [Key::Down, Key::Up, Key::End, Key::Home, Key::PageDown] {
            a.on_key(key);
        }
        press(&mut a, "jkgG");
        assert_eq!(a.selected(), 0);
        assert_eq!(a.scroll(), 0);
    }

    // ---- movement --------------------------------------------------------

    #[test]
    fn move_by_walks_between_sections() {
        let mut a = app();
        a.move_by(1);
        assert_eq!(a.selected, 1);
        a.move_by(1);
        assert_eq!(a.selected, 2);
        a.move_by(-1);
        assert_eq!(a.selected, 1);
    }

    #[test]
    fn move_by_clamps_at_both_ends_instead_of_wrapping() {
        let mut a = app();
        a.move_by(-5);
        assert_eq!(a.selected, 0, "must not go below the first section");
        a.move_by(99);
        assert_eq!(a.selected, 2, "must not go past the last section");
        a.move_by(-99);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn move_by_survives_an_extreme_delta_without_overflowing() {
        let mut a = app();
        a.move_by(isize::MAX);
        assert_eq!(a.selected, 2);
        a.move_by(isize::MIN);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn select_ignores_an_out_of_range_index_and_resets_scroll() {
        let mut a = app();
        a.selected = 1;
        a.scroll = 7;
        a.select(2);
        assert_eq!(a.selected, 2);
        assert_eq!(a.scroll, 0, "changing section resets the detail scroll");
        a.scroll = 7;
        a.select(99);
        assert_eq!(a.selected, 2, "an invalid index must be ignored");
    }

    #[test]
    fn select_relative_changes_section_or_scroll_depending_on_focus() {
        let mut a = app();
        a.select_relative(1);
        assert_eq!(a.selected, 1, "sidebar focus moves the selection");
        assert_eq!(a.scroll, 0);

        a.focus = Focus::Detail;
        a.select_relative(1);
        assert_eq!(a.selected, 1, "selection must not move");
        assert_eq!(a.scroll, 1, "detail focus scrolls instead");
    }

    #[test]
    fn scrolling_stops_when_the_last_row_reaches_the_bottom() {
        // The bug: the limit was "last row at the top", so the pane scrolled
        // into emptiness and the next Up presses did nothing visible.
        let mut a = app();
        a.scroll_by(50);
        assert_eq!(a.scroll, 25 - VIEWPORT);

        a.scroll_by(-1);
        assert_eq!(a.scroll, 25 - VIEWPORT - 1, "the first Up moves at once");
    }

    #[test]
    fn page_moves_a_screenful_and_clamps() {
        let mut a = app();
        a.on_key(Key::PageDown);
        assert_eq!(a.scroll, VIEWPORT - 1);
        a.on_key(Key::PageDown);
        assert_eq!(a.scroll, 15, "must clamp, not overshoot");
        a.on_key(Key::PageUp);
        assert_eq!(a.scroll, 15 - (VIEWPORT - 1));
        a.on_key(Key::PageUp);
        assert_eq!(a.scroll, 0, "must clamp at the top too");
    }

    #[test]
    fn a_section_that_fits_does_not_scroll() {
        let mut a = app();
        a.set_viewport(100);
        a.scroll_by(10);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn growing_the_viewport_pulls_the_scroll_back() {
        let mut a = app();
        a.scroll_by(50);
        a.set_viewport(20);
        assert_eq!(a.scroll, 5);
    }

    #[test]
    fn visible_rows_counts_only_rows_the_filter_keeps() {
        let mut a = app();
        a.on_snapshot(
            snapshot(vec![Section::new(
                "Only",
                vec![
                    Row::field("alpha", "1"),
                    Row::Blank,
                    Row::field("beta", "2"),
                ],
            )]),
            Trigger::Timer,
        );
        assert_eq!(a.visible_rows().len(), 3);
        a.filter = filter("a");
        assert_eq!(a.visible_rows().len(), 2, "the blank row drops out");
        a.filter = filter("zzz");
        assert!(a.visible_rows().is_empty());
    }

    #[test]
    fn a_smaller_snapshot_pulls_the_selection_back_into_range() {
        let mut a = app();
        a.select(2);
        a.on_snapshot(snapshot(vec![Section::new("Only", vec![])]), Trigger::Timer);
        assert_eq!(a.selected, 0);
        assert!(a.section().is_some());
    }

    // ---- focus / status / tick ------------------------------------------

    #[test]
    fn toggle_focus_flips_between_panes() {
        let mut a = app();
        assert_eq!(a.focus, Focus::Sections);
        a.toggle_focus();
        assert_eq!(a.focus, Focus::Detail);
        a.toggle_focus();
        assert_eq!(a.focus, Focus::Sections);
    }

    #[test]
    fn an_expired_status_message_clears_itself() {
        let mut a = app();
        a.set_status("refreshed");
        assert!(!a.tick(), "a fresh status must not redraw on its own");
        assert_eq!(a.status(), Some("refreshed"));

        a.status = Some(Status {
            text: "refreshed",
            at: Instant::now()
                .checked_sub(Duration::from_secs(4))
                .expect("the clock is past four seconds"),
        });
        assert!(a.tick(), "an expired status must trigger a redraw");
        assert_eq!(
            a.status(),
            None,
            "a status line that never goes away is noise"
        );
    }

    #[test]
    fn no_status_never_schedules_a_redraw() {
        assert!(!app().tick());
    }

    // ---- on_key ----------------------------------------------------------

    #[test]
    fn q_esc_and_ctrl_c_quit() {
        for key in [Key::Char('q'), Key::Esc, Key::CtrlC] {
            assert_eq!(app().on_key(key), Action::Quit, "{key:?} should quit");
        }
    }

    #[test]
    fn an_alt_chord_does_not_quit() {
        // Alt+x used to decode as Esc, then quit.
        assert_eq!(app().on_key(Key::Alt('x')), Action::Nothing);
    }

    #[test]
    fn ctrl_c_quits_even_while_the_filter_is_open() {
        // It used to be swallowed by the filter's catch-all arm, which made
        // the app look unresponsive mid-query.
        let mut a = app();
        a.on_key(Key::Char('/'));
        press(&mut a, "gpu");
        assert_eq!(a.on_key(Key::CtrlC), Action::Quit);
    }

    #[test]
    fn esc_closes_help_before_it_quits() {
        let mut a = app();
        a.on_key(Key::Char('?'));
        assert_eq!(
            a.on_key(Key::Esc),
            Action::Nothing,
            "esc closes the help first"
        );
        assert_eq!(a.mode(), Mode::Browse);
        assert_eq!(a.on_key(Key::Esc), Action::Quit, "a second esc quits");
    }

    #[test]
    fn question_mark_toggles_help() {
        let mut a = app();
        a.on_key(Key::Char('?'));
        assert_eq!(a.mode(), Mode::Help);
        a.on_key(Key::Char('?'));
        assert_eq!(a.mode(), Mode::Browse);
    }

    #[test]
    fn jk_and_arrows_both_move_the_selection() {
        for (vim, arrow) in [(Key::Char('j'), Key::Down), (Key::Char('k'), Key::Up)] {
            let mut a = app();
            a.selected = 1;
            a.on_key(vim);
            a.on_key(vim);
            let after_vim = a.selected;
            a.selected = 1;
            a.on_key(arrow);
            a.on_key(arrow);
            assert_eq!(after_vim, a.selected, "{vim:?} and {arrow:?} must agree");
        }
    }

    #[test]
    fn g_and_capital_g_jump_to_the_ends() {
        let mut a = app();
        a.on_key(Key::Char('G'));
        assert_eq!(a.selected, 2);
        a.on_key(Key::Char('g'));
        assert_eq!(a.selected, 0);
        a.on_key(Key::End);
        assert_eq!(a.selected, 2);
        a.on_key(Key::Home);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn hl_and_arrows_address_the_panes_and_tab_swaps() {
        let mut a = app();
        a.on_key(Key::Right);
        assert_eq!(a.focus, Focus::Detail);
        a.on_key(Key::Char('h'));
        assert_eq!(a.focus, Focus::Sections);
        a.on_key(Key::Char('l'));
        assert_eq!(a.focus, Focus::Detail);
        a.on_key(Key::Left);
        assert_eq!(a.focus, Focus::Sections);
        a.on_key(Key::Tab);
        assert_eq!(a.focus, Focus::Detail);
        a.on_key(Key::Tab);
        assert_eq!(a.focus, Focus::Sections);
    }

    #[test]
    fn r_asks_for_a_refresh_and_reports_when_it_lands() {
        let mut a = app();
        assert_eq!(a.on_key(Key::Char('r')), Action::Refresh);
        assert_eq!(a.status(), Some("refreshing…"));

        // A snapshot already in flight when `r` was pressed is not the answer.
        a.on_snapshot(snapshot(vec![Section::new("One", vec![])]), Trigger::Timer);
        assert_eq!(a.status(), Some("refreshing…"));

        a.on_snapshot(
            snapshot(vec![Section::new("One", vec![])]),
            Trigger::Request,
        );
        assert_eq!(a.status(), Some("refreshed"));
    }

    #[test]
    fn a_timed_snapshot_does_not_claim_a_manual_refresh() {
        let mut a = app();
        a.on_snapshot(snapshot(vec![Section::new("One", vec![])]), Trigger::Timer);
        assert_eq!(a.status(), None);
    }

    // ---- filter interaction ---------------------------------------------

    #[test]
    fn slash_opens_the_filter_and_typing_goes_into_the_query() {
        let mut a = app();
        a.on_key(Key::Char('/'));
        assert_eq!(a.mode(), Mode::Filter);
        press(&mut a, "gpu");
        assert_eq!(a.filter().query(), "gpu");
    }

    #[test]
    fn typing_in_the_filter_never_quits_or_navigates() {
        let mut a = app();
        a.on_key(Key::Char('/'));
        let actions: Vec<Action> = "qjkgGr?/".chars().map(|c| a.on_key(Key::Char(c))).collect();
        assert!(actions.iter().all(|x| *x == Action::Nothing), "{actions:?}");
        assert_eq!(
            a.filter().query(),
            "qjkgGr?/",
            "every key belongs to the query"
        );
        assert_eq!(
            a.mode(),
            Mode::Filter,
            "? must not open help while filtering"
        );
        assert_eq!(a.selected, 0, "navigation keys must be inert");
    }

    #[test]
    fn enter_applies_the_filter_and_leaves_filter_mode() {
        let mut a = app();
        a.on_key(Key::Char('/'));
        press(&mut a, "gpu");
        a.on_key(Key::Enter);
        assert_eq!(a.mode(), Mode::Browse);
        assert_eq!(a.filter().query(), "gpu", "enter keeps the query");
    }

    #[test]
    fn esc_clears_the_query_and_leaves_filter_mode() {
        let mut a = app();
        a.on_key(Key::Char('r'));
        a.on_key(Key::Char('/'));
        press(&mut a, "gpu");
        assert_eq!(
            a.on_key(Key::Esc),
            Action::Nothing,
            "esc in the filter must not quit"
        );
        assert_eq!(a.mode(), Mode::Browse);
        assert_eq!(a.filter().query(), "");
        assert_eq!(
            a.status(),
            None,
            "clearing the filter clears its status too"
        );
    }

    #[test]
    fn backspace_edits_the_query_and_resets_the_scroll() {
        let mut a = app();
        a.focus = Focus::Detail;
        a.scroll_by(3);
        a.on_key(Key::Char('/'));
        press(&mut a, "one");
        a.on_key(Key::Backspace);
        assert_eq!(a.filter().query(), "on");
        assert_eq!(a.scroll, 0, "editing the query must rewind the pane");

        for _ in 0..5 {
            a.on_key(Key::Backspace);
        }
        assert_eq!(
            a.filter().query(),
            "",
            "backspacing past the start is harmless"
        );
    }

    #[test]
    fn navigation_keys_are_inert_while_filtering() {
        let mut a = app();
        a.selected = 1;
        a.on_key(Key::Char('/'));
        a.on_key(Key::Down);
        a.on_key(Key::Tab);
        a.on_key(Key::PageDown);
        assert_eq!(a.selected, 1, "arrows must not move the selection");
        assert_eq!(a.focus, Focus::Sections, "tab must not swap panes");
    }

    #[test]
    fn a_filter_query_survives_a_refresh() {
        let mut a = app();
        a.on_key(Key::Char('/'));
        a.on_key(Key::Char('g'));
        a.on_key(Key::Enter);
        a.on_snapshot(snapshot(vec![Section::new("One", vec![])]), Trigger::Timer);
        assert_eq!(
            a.filter().query(),
            "g",
            "a new snapshot must not drop the filter"
        );
    }
}
