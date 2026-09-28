//! Monitor layout as Hyprland reports it. This is the only source that knows
//! about scaling, position and refresh rate on a Wayland compositor.

use super::{Host, Row};

pub(crate) fn rows(host: &Host) -> Vec<Row> {
    let Some(monitors) = host.hypr("monitors") else {
        return vec![Row::note("Hyprland not reachable (not running Hyprland?)")];
    };

    let mut rows = parse_monitors(&monitors);

    if let Some(workspaces) = host.hypr("workspaces") {
        rows.push(Row::header("Wayland compositor"));
        rows.push(Row::field(
            "Active workspaces",
            count_workspaces(&workspaces).to_string(),
        ));
        if let Some(title) = host.hypr("activewindow").and_then(|w| window_title(&w)) {
            // Whatever the user is looking at: a document name, a chat, a
            // web page title. Shown in the TUI, withheld from the plain
            // report unless asked for.
            rows.push(Row::identifier("Focused window", title));
        }
    }

    if rows.is_empty() {
        rows.push(Row::note("no monitor information"));
    }

    rows
}

/// Regular workspaces. Special workspaces are numbered -99, -98 and count as
/// active even when no window occupies them.
fn count_workspaces(workspaces: &str) -> usize {
    workspaces
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("workspace ID "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|n| n.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .count()
}

fn window_title(activewindow: &str) -> Option<String> {
    activewindow
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("title:"))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Turn `hyprctl monitors` output into a header and field rows per monitor.
///
/// The output is one `Monitor <name>:` heading followed by indented
/// `key: value` lines. Values may contain colons of their own, so only the
/// first one separates the key.
fn parse_monitors(out: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut in_monitor = false;

    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Monitor ") {
            // Only the heading's own trailing colon is punctuation.
            let name = rest.strip_suffix(':').unwrap_or(rest).trim();
            in_monitor = !name.is_empty();
            if in_monitor {
                rows.push(Row::header(name));
            }
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let identity = matches!(key, "description" | "make" | "model" | "serial");
        if !in_monitor || (identity && (value.is_empty() || value == "N/A")) {
            continue;
        }

        // Hyprland builds `description` from make, model and serial, so it
        // carries the serial as surely as the `serial` line does.
        rows.push(if matches!(key, "serial" | "description") {
            Row::identifier(key, value)
        } else {
            Row::field(key, value)
        });
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = "\
Monitor eDP-1 (eDP-1 2560x1600) (0x1e) at 0x0:
	description: Internal 2560x1600@59.951Hz
	make: Chimei Innolux
	model: 0x14D4
	serial: N/A
	monitor: 0
	mode: 2560x1600@59.951Hz
	scale: 1.00
	transform: 0
	position: 0,0
	mirror: false
	focus: true
	dpmsStatus: On
	vrr: capable
	activelyTearing: false
	disabled: false
	currentFormat: XRGB8888
	availableModes: 2560x1600@59.95Hz, 1920x1200@59.95Hz
	[BPP8]
Monitor DP-2 (DP-2 3440x1440) (0x1f) at 2560x140:
	description:
	make: Dell Inc.
	model: DELL U3419LW
	serial: ABC123
	monitor: 1
	mode: 3440x1440@59.951Hz
	scale: 1.00
	position: 2560,0
	disabled: false
";

    fn field<'a>(rows: &'a [Row], key: &str) -> Option<&'a str> {
        rows.iter().find_map(|r| match r {
            Row::Field { label, value, .. } if label == key => Some(value.as_str()),
            _ => None,
        })
    }

    // ---- parse_monitors --------------------------------------------------

    #[test]
    fn parse_monitors_creates_a_header_per_monitor() {
        let rows = parse_monitors(MONITORS);
        let headers: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Header(h) => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![
                "eDP-1 (eDP-1 2560x1600) (0x1e) at 0x0",
                "DP-2 (DP-2 3440x1440) (0x1f) at 2560x140"
            ]
        );
    }

    #[test]
    fn parse_monitors_strips_only_the_headings_own_colon() {
        let rows = parse_monitors("Monitor DP-1:\n\tscale: 1.00\n");
        assert!(matches!(&rows[0], Row::Header(h) if h == "DP-1"));
    }

    #[test]
    fn parse_monitors_reads_the_fields() {
        let rows = parse_monitors(MONITORS);
        assert_eq!(field(&rows, "make"), Some("Chimei Innolux"));
        assert_eq!(field(&rows, "scale"), Some("1.00"));
        assert_eq!(field(&rows, "position"), Some("0,0"));
    }

    #[test]
    fn parse_monitors_keeps_a_value_that_contains_a_colon() {
        let rows = parse_monitors("Monitor X:\n\tdescription: DP-1: XRGB8888\n");
        assert_eq!(field(&rows, "description"), Some("DP-1: XRGB8888"));
    }

    #[test]
    fn parse_monitors_keeps_a_value_that_contains_commas() {
        let rows = parse_monitors(MONITORS);
        assert!(field(&rows, "availableModes").expect("modes").contains(','));
    }

    #[test]
    fn parse_monitors_drops_empty_and_not_applicable_identifiers() {
        let rows = parse_monitors(MONITORS);
        assert_eq!(
            field(&rows, "serial"),
            Some("ABC123"),
            "the real serial survives"
        );
        // "N/A" and an empty description are both dropped, so a monitor with
        // no EDID does not show a row of N/A.
        let n_a = rows
            .iter()
            .filter(|r| matches!(r, Row::Field { value, .. } if value == "N/A"))
            .count();
        assert_eq!(n_a, 0);
    }

    #[test]
    fn parse_monitors_ignores_a_line_with_no_colon() {
        let rows = parse_monitors(MONITORS);
        // The [BPP8] marker has no colon, so it must not become a field.
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, Row::Field { label, .. } if label == "[BPP8]")),
            "{rows:?}"
        );
    }

    #[test]
    fn parse_monitors_ignores_fields_that_precede_any_heading() {
        let rows = parse_monitors("stray: value\nMonitor X:\n\tscale: 2.00\n");
        assert_eq!(field(&rows, "stray"), None);
        assert_eq!(field(&rows, "scale"), Some("2.00"));
    }

    #[test]
    fn parse_monitors_skips_a_heading_with_no_name() {
        let rows = parse_monitors("Monitor :\n\tscale: 1.00\nMonitor DP-1:\n\tscale: 1.00\n");
        let headers = rows.iter().filter(|r| matches!(r, Row::Header(_))).count();
        assert_eq!(headers, 1, "{rows:?}");
    }

    #[test]
    fn parse_monitors_of_junk_is_empty() {
        assert!(parse_monitors("").is_empty());
        assert!(parse_monitors("no colons here at all").is_empty());
        assert!(parse_monitors("\n\n   \n").is_empty());
    }

    #[test]
    fn a_monitor_serial_is_an_identifier_wherever_it_appears() {
        let rows = parse_monitors(
            "Monitor DP-2 (ID 1):\n\tdescription: Dell Inc. DELL U2723QE 5KC1234 (DP-2)\n\tmake: Dell Inc.\n\tserial: 5KC1234\n",
        );

        assert!(rows.contains(&Row::identifier("serial", "5KC1234")));
        assert!(rows.contains(&Row::identifier(
            "description",
            "Dell Inc. DELL U2723QE 5KC1234 (DP-2)"
        )));
        assert!(rows.contains(&Row::field("make", "Dell Inc.")));
    }

    // ---- workspaces and windows ---------------------------------------------

    #[test]
    fn only_regular_workspaces_are_counted() {
        let out = "\
workspace ID 1 (1) on monitor eDP-1:
	windows: 2
workspace ID 3 (3) on monitor eDP-1:
	windows: 1
workspace ID -98 (special:magic) on monitor eDP-1:
	windows: 0
";
        assert_eq!(count_workspaces(out), 2);
        assert_eq!(count_workspaces(""), 0);
    }

    #[test]
    fn the_window_title_is_read_and_an_empty_one_is_none() {
        assert_eq!(
            window_title("Window 5 -> kitty:\n\tclass: kitty\n\ttitle: ~/src: vim\n").as_deref(),
            Some("~/src: vim")
        );
        assert_eq!(window_title("Invalid\n"), None);
        assert_eq!(window_title("\ttitle: \n"), None);
    }

    #[test]
    fn without_hyprland_the_section_says_so() {
        let fx = crate::collect::fixture::Fixture::new();

        assert_eq!(
            rows(&fx.host()),
            [Row::note("Hyprland not reachable (not running Hyprland?)")]
        );
    }
}
