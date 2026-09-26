use crate::collect::{self, Row, Section, Stats};
use crate::input::Key;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sections,
    Detail,
}

/// Text filter applied to the detail pane, typed with `/`.
#[derive(Default)]
pub struct Filter {
    pub active: bool,
    pub query: String,
}

impl Filter {
    pub fn matches(&self, row: &Row) -> bool {
        if self.query.is_empty() {
            return true;
        }

        let needle = self.query.to_lowercase();

        match row {
            Row::Header(text) => text.to_lowercase().contains(&needle),
            Row::Field { label, value, .. } => {
                label.to_lowercase().contains(&needle) || value.to_lowercase().contains(&needle)
            }
            Row::Note(text) => text.to_lowercase().contains(&needle),
            Row::Blank => false,
        }
    }
}

pub struct App {
    pub sections: Vec<Section>,
    pub selected: usize,
    pub focus: Focus,
    pub scroll: u16,
    pub should_quit: bool,
    pub show_help: bool,
    pub status: String,
    pub status_at: Instant,
    pub filter: Filter,
    pub last_refresh: Instant,
    pub refresh_interval: Duration,
    pub stats: Stats,
    pub hostname: String,
    pub kernel: String,
}

impl App {
    pub fn new() -> App {
        let mut app = App {
            sections: Vec::new(),
            selected: 0,
            focus: Focus::Sections,
            scroll: 0,
            should_quit: false,
            show_help: false,
            status: String::new(),
            status_at: Instant::now(),
            filter: Filter::default(),
            last_refresh: Instant::now(),
            refresh_interval: Duration::from_secs(2),
            stats: Stats::new(),
            hostname: collect::system::hostname(),
            kernel: collect::system::kernel(),
        };
        app.collect();
        app
    }

    /// Re-read everything. `/proc` and `/sys` reads are cheap enough to redo
    /// whole, and the only state we must keep is the CPU time series.
    pub fn collect(&mut self) {
        self.stats.sample();

        let stats = &mut self.stats;

        let mut sections = vec![
            Section::new("Overview", overview(stats)),
            Section::new("OS & kernel", collect::system::rows(stats)),
            Section::new("CPU", collect::cpu::rows(stats)),
            Section::new("Memory", stats.memory().rows()),
            Section::new("Board & firmware", collect::dmi::rows()),
            Section::new("Graphics", collect::gpu::rows()),
            Section::new("Displays", collect::display::rows()),
            Section::new("Disks", collect::storage::rows()),
            Section::new("PCI devices", collect::pci::rows()),
            Section::new("USB & wireless", collect::usb::rows()),
            Section::new("Sensors", collect::sensors::rows()),
            Section::new("Power & battery", collect::power::rows()),
            Section::new("Omarchy", collect::omarchy::rows()),
        ];

        if let Some(wireless) = wireless_section() {
            sections.insert(11, wireless);
        }

        self.sections = sections;

        if self.selected >= self.sections.len() {
            self.selected = self.sections.len().saturating_sub(1);
        }

        self.last_refresh = Instant::now();
    }

    pub fn section(&self) -> &Section {
        &self.sections[self.selected]
    }

    pub fn select(&mut self, index: usize) {
        if index < self.sections.len() {
            self.selected = index;
            self.scroll = 0;
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        // With no sections loaded there is nowhere to move; `clamp` would
        // panic here because its range would be inverted.
        let Some(last) = self.sections.len().checked_sub(1) else {
            return;
        };
        let next = (self.selected as isize)
            .saturating_add(delta)
            .clamp(0, last as isize);

        self.select(next as usize);
    }

    /// On the sidebar this changes section; in the detail pane it scrolls.
    pub fn select_relative(&mut self, delta: isize) {
        if self.focus == Focus::Sections {
            self.move_by(delta);
        } else {
            self.scroll_by(delta);
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.max_scroll();
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as u16;
    }

    pub fn page(&mut self, forward: bool) {
        self.scroll_by(if forward { 10 } else { -10 });
    }

    /// How many rows the filter leaves visible, used for the scroll bounds.
    pub fn detail_len(&self) -> usize {
        self.section()
            .rows
            .iter()
            .filter(|r| self.filter.matches(r))
            .count()
    }

    pub fn max_scroll(&self) -> u16 {
        self.detail_len().saturating_sub(1) as u16
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sections => Focus::Detail,
            Focus::Detail => Focus::Sections,
        };
    }

    pub fn set_status(&mut self, text: &str) {
        self.status = text.to_string();
        self.status_at = Instant::now();
    }

    /// Called every loop iteration. Returns true when the screen needs redrawing.
    pub fn tick(&mut self) -> bool {
        let mut dirty = false;

        if self.last_refresh.elapsed() >= self.refresh_interval {
            self.collect();
            dirty = true;
        }

        // A status line that never goes away stops being information.
        if !self.status.is_empty() && self.status_at.elapsed() > Duration::from_secs(3) {
            self.status.clear();
            dirty = true;
        }
        dirty
    }

    pub fn on_key(&mut self, key: Key) {
        // Ctrl+C is an exit request wherever it lands, including mid-query:
        // swallowing it inside the filter made the app look hung.
        if key == Key::CtrlC {
            self.should_quit = true;

            return;
        }

        if self.filter.active {
            match key {
                Key::Esc => {
                    self.filter.active = false;
                    self.filter.query.clear();
                    self.status.clear();
                }
                Key::Enter => self.filter.active = false,
                Key::Backspace => {
                    self.filter.query.pop();
                    self.scroll = 0;
                }
                Key::Char(c) => {
                    self.filter.query.push(c);
                    self.scroll = 0;
                }
                _ => {}
            }

            return;
        }

        match key {
            // Esc backs out one layer at a time, then quits.
            Key::Esc if self.show_help => self.show_help = false,
            Key::Char('q') | Key::Esc | Key::CtrlC => self.should_quit = true,
            Key::Char('j') | Key::Down => self.select_relative(1),
            Key::Char('k') | Key::Up => self.select_relative(-1),
            Key::Char('g') | Key::Home => self.select(0),
            Key::Char('G') | Key::End => self.select(self.sections.len().saturating_sub(1)),
            Key::PageDown => self.page(true),
            Key::PageUp => self.page(false),
            // Panes are addressed the same way in every direction: left is the
            // sidebar, right is the detail, tab swaps between them.
            Key::Left | Key::Char('h') => self.focus = Focus::Sections,
            Key::Right | Key::Char('l') => self.focus = Focus::Detail,
            Key::Tab => self.toggle_focus(),
            Key::Char('r') => {
                self.collect();
                self.set_status("refreshed");
            }
            Key::Char('/') => self.filter.active = true,
            Key::Char('?') => self.show_help = !self.show_help,
            _ => {}
        }
    }
}

fn wireless_section() -> Option<Section> {
    let rows = collect::usb::wireless();
    (!rows.is_empty()).then(|| Section::new("Wireless", rows))
}

/// The condensed view: the handful of numbers you check first.
fn overview(stats: &mut Stats) -> Vec<Row> {
    let (theme, channel) = collect::omarchy::summary();
    let (battery, battery_state) = collect::power::summary();
    let memory = stats.memory();

    let mut rows = vec![Row::Header("Identity".into())];
    rows.push(Row::field("Host", collect::system::hostname()));
    rows.push(Row::field("Distro", collect::system::distro()));
    rows.push(Row::field("Kernel", collect::system::kernel()));
    rows.push(Row::field("Chassis", collect::dmi::chassis()));
    rows.push(Row::field("Board", board()));
    rows.push(Row::field(
        "Omarchy",
        format!("{} · {}", collect::omarchy::version(), channel),
    ));
    rows.push(Row::field("Theme", theme));

    rows.push(Row::Header("Live".into()));
    let cores = stats.per_core().len();
    if cores > 0 {
        let (load1, load5, load15) = stats.load();
        if stats.primed() {
            rows.push(Row::field_with(
                "CPU",
                format!(
                    "{} · {cores} thread(s) · load {load1:.2} {load5:.2} {load15:.2}",
                    collect::cpu::brand()
                ),
                stats.cpu_usage() / 100.0,
            ));
        } else {
            rows.push(Row::field(
                "CPU",
                format!("{} · {cores} thread(s) · sampling", collect::cpu::brand()),
            ));
        }
    }
    rows.push(Row::field_with(
        "Memory",
        format!(
            "{} / {}",
            collect::units::human_bytes(memory.used()),
            collect::units::human_bytes(memory.total)
        ),
        fraction(memory.used(), memory.total),
    ));
    if memory.swap_total > 0 {
        rows.push(Row::field_with(
            "Swap",
            format!(
                "{} / {}",
                collect::units::human_bytes(memory.swap_used()),
                collect::units::human_bytes(memory.swap_total)
            ),
            fraction(memory.swap_used(), memory.swap_total),
        ));
    }
    if let Some(peak) = collect::sensors::peak_temperature() {
        // No bar here: the fraction would be peak/100, the same number the
        // value already shows, so the plain rendering read "62 °C  62%". The
        // per-sensor gauges in the Sensors section carry that detail.
        rows.push(Row::field("Peak temperature", format!("{peak:.0} °C")));
    }
    rows.push(Row::field(
        "Uptime",
        collect::units::human_secs(stats.uptime()),
    ));
    rows.push(Row::field(
        "Battery",
        format!("{battery} · {battery_state}"),
    ));

    rows
}

fn fraction(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

fn board() -> String {
    collect::fs::read("/sys/class/dmi/id/board_name")
        .or_else(|| collect::fs::read("/sys/class/dmi/id/product_name"))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// An App with no hardware behind it, so behaviour does not depend on the
    /// machine running the tests. The sections carry enough rows that the
    /// detail pane can actually scroll.
    fn app() -> App {
        let many = |prefix: &str| {
            (0..25)
                .map(|i| Row::field(format!("{prefix} {i}"), format!("v{i}")))
                .collect::<Vec<Row>>()
        };
        App {
            sections: vec![
                Section::new("One", many("one")),
                Section::new("Two", many("two")),
                Section::new("Three", many("three")),
            ],
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

    // ---- Filter::matches -------------------------------------------------

    #[test]
    fn an_empty_filter_matches_every_row_including_blanks() {
        let f = Filter::default();
        assert!(f.matches(&Row::Header("anything".into())));
        assert!(f.matches(&Row::field("l", "v")));
        assert!(f.matches(&Row::note("n")));
        assert!(f.matches(&Row::Blank));
    }

    #[test]
    fn filter_searches_labels_values_headers_and_notes() {
        let f = Filter {
            active: true,
            query: "gpu".into(),
        };
        assert!(f.matches(&Row::field("GPU", "whatever")));
        assert!(f.matches(&Row::field("label", "Intel GPU")));
        assert!(f.matches(&Row::Header("GPU section".into())));
        assert!(f.matches(&Row::note("no gpu here")));
        assert!(!f.matches(&Row::field("Memory", "16 GiB")));
    }

    #[test]
    fn filter_is_case_insensitive() {
        let f = Filter {
            active: true,
            query: "AmD".into(),
        };
        assert!(f.matches(&Row::field("gpu", "AMD Radeon 780M")));
        let f = Filter {
            active: true,
            query: "amd".into(),
        };
        assert!(f.matches(&Row::field("GPU", "AMD Radeon 780M")));
    }

    #[test]
    fn filter_hides_blank_rows_once_a_query_is_typed() {
        // A blank separator row matches nothing, so a query never leaves stray
        // gaps in the output.
        let f = Filter {
            active: true,
            query: "a".into(),
        };
        assert!(!f.matches(&Row::Blank));
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
    fn move_by_on_an_app_with_no_sections_does_not_panic() {
        // `clamp(0, len - 1)` with len == 0 has an inverted range and panics.
        let mut a = app();
        a.sections.clear();
        a.selected = 0;
        a.move_by(1);
        a.move_by(-1);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn select_ignores_an_out_of_range_index_and_keeps_scroll_reset() {
        let mut a = app();
        a.selected = 1;
        a.scroll = 7;
        a.select(2);
        assert_eq!(a.selected, 2);
        assert_eq!(a.scroll, 0, "changing section resets the detail scroll");
        a.scroll = 7;
        a.select(99);
        assert_eq!(a.selected, 2, "an invalid index must be ignored");
        a.select(0);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn select_relative_changes_section_or_scroll_depending_on_focus() {
        let mut a = app();
        a.focus = Focus::Sections;
        a.select_relative(1);
        assert_eq!(a.selected, 1, "sidebar focus moves the selection");
        assert_eq!(a.scroll, 0);

        a.focus = Focus::Detail;
        a.scroll = 0;
        a.select_relative(1);
        assert_eq!(a.selected, 1, "selection must not move");
        assert_eq!(a.scroll, 1, "detail focus scrolls instead");
    }

    #[test]
    fn scroll_by_clamps_to_the_available_rows() {
        let mut a = app();
        a.scroll = 0;
        a.scroll_by(-5);
        assert_eq!(a.scroll, 0);
        a.scroll_by(50);
        assert_eq!(a.scroll, a.max_scroll());
    }

    #[test]
    fn page_moves_ten_rows_and_clamps() {
        let mut a = app();
        a.scroll = 0;
        a.page(true);
        assert_eq!(a.scroll, 10);
        a.page(true);
        assert_eq!(a.scroll, 20);
        a.page(true);
        assert_eq!(a.scroll, a.max_scroll(), "must clamp, not overshoot");
        a.page(false);
        assert_eq!(a.scroll, 14);
        a.page(false);
        a.page(false);
        assert_eq!(a.scroll, 0, "must clamp at the top too");
    }

    #[test]
    fn max_scroll_is_saturating_for_an_empty_section() {
        let mut a = app();
        a.sections[0].rows.clear();
        a.select(0);
        assert_eq!(a.detail_len(), 0);
        assert_eq!(a.max_scroll(), 0, "an empty section must not underflow");
        a.scroll_by(10);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn detail_len_counts_only_rows_the_filter_keeps() {
        let mut a = app();
        a.sections[0].rows = vec![
            Row::field("alpha", "1"),
            Row::Blank,
            Row::field("beta", "2"),
        ];
        a.select(0);
        assert_eq!(a.detail_len(), 3);
        a.filter.query = "a".into();
        assert_eq!(a.detail_len(), 2, "the blank row drops out");
        a.filter.query = "zzz".into();
        assert_eq!(a.detail_len(), 0);
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
    fn tick_leaves_the_screen_alone_before_the_interval_elapses() {
        let mut a = app();
        a.refresh_interval = Duration::from_secs(3600);
        assert!(!a.tick(), "nothing should be redrawn immediately");
    }

    #[test]
    fn tick_collects_once_the_refresh_interval_has_passed() {
        let mut a = app();
        a.refresh_interval = Duration::from_secs(2);
        a.last_refresh = Instant::now() - Duration::from_secs(3);
        assert!(a.tick(), "an elapsed interval must force a redraw");
    }

    #[test]
    fn an_expired_status_message_clears_itself() {
        let mut a = app();
        a.refresh_interval = Duration::from_secs(3600);
        a.set_status("refreshed");
        assert!(!a.tick(), "a fresh status must not redraw on its own");
        assert_eq!(a.status, "refreshed");

        a.status_at = Instant::now() - Duration::from_secs(4);
        assert!(a.tick(), "an expired status must trigger a redraw");
        assert_eq!(a.status, "", "a status line that never goes away is noise");
    }

    #[test]
    fn an_empty_status_never_schedules_a_redraw() {
        let mut a = app();
        a.refresh_interval = Duration::from_secs(3600);
        a.status_at = Instant::now() - Duration::from_secs(60);
        assert!(!a.tick());
    }

    // ---- on_key ----------------------------------------------------------

    #[test]
    fn q_and_esc_quit() {
        for key in [Key::Char('q'), Key::Esc] {
            let mut a = app();
            a.on_key(key);
            assert!(a.should_quit, "{key:?} should quit");
        }
    }

    #[test]
    fn ctrl_c_quits_even_while_the_filter_is_open() {
        // It used to be swallowed by the filter's catch-all arm, which made
        // the app look unresponsive mid-query.
        let mut a = app();
        a.filter.active = true;
        a.filter.query = "gpu".into();
        a.on_key(Key::CtrlC);
        assert!(a.should_quit);
    }

    #[test]
    fn esc_closes_help_before_it_quits() {
        let mut a = app();
        a.show_help = true;
        a.on_key(Key::Esc);
        assert!(!a.show_help, "esc must close the help first");
        assert!(!a.should_quit, "and must not quit on the same press");

        a.on_key(Key::Esc);
        assert!(a.should_quit, "a second esc quits");
    }

    #[test]
    fn question_mark_toggles_help() {
        let mut a = app();
        a.on_key(Key::Char('?'));
        assert!(a.show_help);
        a.on_key(Key::Char('?'));
        assert!(!a.show_help);
    }

    #[test]
    fn jk_and_arrows_both_move_the_selection() {
        for (a_key, arrow) in [(Key::Char('j'), Key::Down), (Key::Char('k'), Key::Up)] {
            let mut a = app();
            a.selected = 1;
            a.on_key(a_key);
            a.on_key(a_key);
            let after_vim = a.selected;
            a.selected = 1;
            a.on_key(arrow);
            a.on_key(arrow);
            assert_eq!(after_vim, a.selected, "{a_key:?} and {arrow:?} must agree");
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
    fn capital_g_on_an_app_with_no_sections_does_not_underflow() {
        // `len() - 1` on an empty vec underflows; in a debug build it panics.
        let mut a = app();
        a.sections.clear();
        a.on_key(Key::Char('G'));
        a.on_key(Key::End);
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
    fn r_refreshes_and_says_so() {
        let mut a = app();
        a.refresh_interval = Duration::from_secs(3600);
        a.on_key(Key::Char('r'));
        assert_eq!(a.status, "refreshed");
        // A manual refresh resets the timer, so tick must not immediately
        // collect all over again.
        assert!(!a.tick());
    }

    // ---- filter interaction ---------------------------------------------

    #[test]
    fn slash_opens_the_filter_and_typing_goes_into_the_query() {
        let mut a = app();
        a.on_key(Key::Char('/'));
        assert!(a.filter.active);
        for c in "gpu".chars() {
            a.on_key(Key::Char(c));
        }
        assert_eq!(a.filter.query, "gpu");
    }

    #[test]
    fn typing_in_the_filter_never_quits_or_navigates() {
        let mut a = app();
        a.filter.active = true;
        for c in "qjkgGr?/".chars() {
            a.on_key(Key::Char(c));
        }
        assert_eq!(a.filter.query, "qjkgGr?/", "every key belongs to the query");
        assert!(!a.should_quit, "q must not quit while filtering");
        assert!(!a.show_help, "? must not open help while filtering");
        assert_eq!(a.selected, 0, "navigation keys must be inert");
    }

    #[test]
    fn enter_applies_the_filter_and_leaves_filter_mode() {
        let mut a = app();
        a.filter.active = true;
        a.filter.query = "gpu".into();
        a.on_key(Key::Enter);
        assert!(!a.filter.active);
        assert_eq!(a.filter.query, "gpu", "enter keeps the query");
    }

    #[test]
    fn esc_clears_the_query_and_leaves_filter_mode() {
        let mut a = app();
        a.filter.active = true;
        a.filter.query = "gpu".into();
        a.status = "x".into();
        a.on_key(Key::Esc);
        assert!(!a.filter.active);
        assert_eq!(a.filter.query, "");
        assert_eq!(
            a.status, "",
            "clearing the filter must clear its status too"
        );
        assert!(!a.should_quit, "esc in the filter must not quit the app");
    }

    #[test]
    fn backspace_edits_the_query_and_resets_the_scroll() {
        let mut a = app();
        a.filter.active = true;
        a.filter.query = "gpux".into();
        a.scroll = 3;
        a.on_key(Key::Backspace);
        assert_eq!(a.filter.query, "gpu");
        assert_eq!(a.scroll, 0, "editing the query must rewind the pane");

        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        a.on_key(Key::Backspace);
        assert_eq!(a.filter.query, "", "backspacing past the start is harmless");
    }

    #[test]
    fn navigation_keys_are_inert_while_filtering() {
        let mut a = app();
        a.filter.active = true;
        a.selected = 1;
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
        a.collect();
        assert_eq!(
            a.filter.query, "g",
            "re-collecting must not drop the filter"
        );
    }
}
