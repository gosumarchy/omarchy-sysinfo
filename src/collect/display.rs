use super::{Row, units::dash};

/// Monitor layout as Hyprland reports it. This is the only source that knows
/// about scaling, position and refresh rate on a Wayland compositor.
pub(crate) fn rows() -> Vec<Row> {
    let Some(out) = hyprctl("monitors") else {
        return vec![Row::note("hyprctl unavailable (not running Hyprland?)")];
    };

    let mut rows = parse_monitors(&out);

    if let Some(workspaces) = hyprctl("workspaces") {
        let used = workspaces
            .lines()
            .filter(|l| l.trim_start().starts_with("workspace "))
            // Special workspaces are numbered -99, -98 and count as active
            // even when no window occupies them.
            .filter(|l| {
                l.trim_start()
                    .strip_prefix("workspace ")
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|n| n.parse::<i64>().ok())
                    .is_some_and(|n| n > 0)
            })
            .count();
        rows.push(Row::Header("Wayland compositor".into()));
        rows.push(Row::field("Active workspaces", used.to_string()));
        if let Some(focused) = hyprctl("activewindow") {
            let title = focused
                .lines()
                .find(|l| l.trim_start().starts_with("title:"))
                .map(|l| {
                    l.trim_start()
                        .trim_start_matches("title:")
                        .trim()
                        .to_string()
                });
            rows.push(Row::field("Focused window", dash(title)));
        }
    }

    if rows.is_empty() {
        rows.push(Row::note("no monitor information"));
    }
    rows
}

/// Turn `hyprctl monitors` output into a header and field rows per monitor.
///
/// The output is one `Monitor <name>:` heading followed by indented
/// `key: value` lines. Values may contain colons of their own, so only the
/// first one separates the key.
fn parse_monitors(out: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut current: Option<(String, Vec<(String, String)>)> = None;

    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Monitor ") {
            flush(&mut rows, &mut current);
            // Only the heading's own trailing colon is punctuation.
            let name = rest.strip_suffix(':').unwrap_or(rest).trim().to_string();
            if !name.is_empty() {
                current = Some((name, Vec::new()));
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_string(), value.trim().to_string());
        if matches!(key.as_str(), "description" | "make" | "model" | "serial")
            && (value.is_empty() || value == "N/A")
        {
            continue;
        }
        if let Some((_, pairs)) = current.as_mut() {
            pairs.push((key, value));
        }
    }
    flush(&mut rows, &mut current);
    rows
}

fn flush(rows: &mut Vec<Row>, current: &mut Option<(String, Vec<(String, String)>)>) {
    if let Some((name, pairs)) = current.take() {
        rows.push(Row::Header(name));
        for (k, v) in pairs {
            rows.push(Row::field(k, v));
        }
    }
}

fn hyprctl(what: &str) -> Option<String> {
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_err() {
        return None;
    }
    let out = std::process::Command::new("hyprctl")
        .arg(what)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// Panel layout of the Omarchy bar. Omarchy 4 renders the bar in QML, so this
/// reads the shell plugin manifest rather than a waybar config.
pub(crate) fn bar_panels() -> Vec<String> {
    let manifest = "/usr/share/omarchy/shell/plugins/bar/manifest.json";
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return vec!["bar plugin manifest not found".to_string()];
    };
    let widgets = super::fs::list_dir("/usr/share/omarchy/shell/plugins/bar/widgets");
    let names: Vec<String> = widgets
        .iter()
        .filter_map(|p| p.file_name()?.to_str())
        .filter(|n| n.ends_with(".qml"))
        .map(|n| n.trim_end_matches(".qml").to_string())
        .collect();
    if names.is_empty() {
        vec![format!("qml bar ({} bytes of manifest)", text.len())]
    } else {
        vec![format!("qml bar widgets: {}", names.join(", "))]
    }
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
        assert!(field(&rows, "availableModes").unwrap().contains(','));
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

    // ---- live ------------------------------------------------------------

    #[test]
    fn rows_and_bar_panels_render_without_panicking() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
        assert!(!bar_panels().is_empty());
    }
}
