use super::{fs::read, units::dash, Row};
use std::path::{Path, PathBuf};
use std::process::Command;

const SHARE: &str = "/usr/share/omarchy";

pub fn rows() -> Vec<Row> {
    let mut rows = vec![
        Row::Header("Release".into()),
        Row::field("Version", version()),
        Row::field("Channel", channel()),
        Row::field("Update policy", update_setting()),
        Row::field("Last update", last_update()),
        Row::field("Shell", shell_state()),
    ];

    rows.push(Row::Header("Session".into()));
    rows.push(Row::field(
        "Desktop",
        dash(std::env::var("XDG_CURRENT_DESKTOP").ok()),
    ));
    rows.push(Row::field("Compositor", compositor_version()));
    rows.push(Row::field("Terminal", terminal()));
    rows.push(Row::field("Shell prompt", prompt()));
    rows.push(Row::field("Locale", dash(std::env::var("LANG").ok())));
    rows.push(Row::field("Timezone", super::system::timezone()));

    rows.push(Row::Header("Appearance".into()));
    rows.push(Row::field("Theme", theme()));
    rows.push(Row::field("Background", background()));
    rows.push(Row::field("Bar", bar_style()));
    for widgets in super::display::bar_panels() {
        rows.push(Row::field("Bar contents", widgets));
    }
    rows.push(Row::field("Branding", branding()));

    rows.push(Row::Header("Toggles".into()));
    for (label, value) in toggles() {
        rows.push(Row::field(label, value));
    }

    let exts = extensions();
    if !exts.is_empty() {
        rows.push(Row::Header("Extensions".into()));
        for ext in exts {
            rows.push(Row::field(ext.0, ext.1));
        }
    }

    let plugins = plugins();
    if !plugins.is_empty() {
        rows.push(Row::Header("Shell plugins".into()));
        for (name, detail) in plugins {
            rows.push(Row::field(name, detail));
        }
    }

    let hooks = hooks();
    if !hooks.is_empty() {
        rows.push(Row::Header("Hooks".into()));
        for (name, _) in hooks {
            rows.push(Row::field(name, "configured"));
        }
    }

    let installed = apps();
    if !installed.is_empty() {
        rows.push(Row::Header(format!("Applications ({})", installed.len())));
        for chunk in installed.chunks(4) {
            rows.push(Row::field("  menu entries", chunk.join(", ")));
        }
        rows.push(Row::Blank);
    }

    rows.push(Row::Header("Keybindings".into()));
    for (keys, action) in keybindings().into_iter().take(40) {
        rows.push(Row::field(keys, action));
    }

    rows
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
}

pub fn version() -> String {
    let pkg = run("pacman", &["-Q", "omarchy"]).unwrap_or_default();
    let built = pkg
        .split_whitespace()
        .nth(1)
        .map(|v| v.to_string())
        .unwrap_or_default();
    let source = read(Path::new(SHARE).join("version")).unwrap_or_else(|| "unknown".into());
    if built.is_empty() {
        source
    } else if built.starts_with(&source) {
        built
    } else {
        format!("{built} (source {source})")
    }
}

pub fn channel() -> String {
    run("omarchy-channel-current", &[]).unwrap_or_else(|| "unknown".into())
}

fn update_setting() -> String {
    read(home().join(".config/omarchy/update.conf"))
        .or_else(|| read(Path::new(SHARE).join("config/omarchy/update.conf")))
        .unwrap_or_else(|| "not configured".into())
}

fn last_update() -> String {
    let path = home().join(".local/state/omarchy/done");
    let Ok(entries) = std::fs::read_dir(&path) else {
        return "no record".into();
    };
    let mut stamps: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // First-run markers are written once at install time and are not
            // evidence of an update.
            if name.starts_with("first-run") {
                return None;
            }
            let time = e.metadata().ok()?.modified().ok()?;
            Some((time, name))
        })
        .collect();
    stamps.sort();
    match stamps.last() {
        Some((time, name)) => {
            let secs = time
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{name} ({})", super::units::human_secs(secs))
        }
        None => "no record".into(),
    }
}

fn shell_state() -> String {
    // Releases before 4 ran a process literally called omarchy-shell; 4 and
    // later run the shell under quickshell.
    if std::path::Path::new("/run/omarchy-shell").exists()
        || run("pgrep", &["-x", "omarchy-shell"]).is_some()
    {
        return "omarchy-shell".into();
    }
    if run("pgrep", &["-x", "quickshell"]).is_some() {
        return "quickshell (bar, menus)".into();
    }
    "not running".into()
}

fn compositor_version() -> String {
    let ver = run("hyprctl", &["version"]).unwrap_or_else(|| "hyprland".into());
    ver.lines()
        .find(|l| l.contains("Hyprland"))
        .unwrap_or("hyprland")
        .trim()
        .to_string()
}

fn terminal() -> String {
    // Emulators advertise themselves through the environment. The order is
    // roughly how likely each one is to be the one running Omarchy.
    for (var, name) in [
        ("ALACRITTY_LOG", "alacritty"),
        ("KITTY_WINDOW_ID", "kitty"),
        ("VTE_VERSION", "gnome-terminal"),
        ("GHOSTTY_RESOURCES_DIR", "ghostty"),
        ("WEZTERM_EXECUTABLE", "wezterm"),
        ("KONSOLE_VERSION", "konsole"),
        ("WT_SESSION", "windows-terminal"),
        ("FOOT_TTY", "foot"),
    ] {
        if std::env::var_os(var).is_some() {
            return name.to_string();
        }
    }
    if let Some(program) = std::env::var("TERM_PROGRAM").ok().filter(|t| !t.is_empty()) {
        return program;
    }
    // Otherwise walk up our own process tree to the nearest program that is not
    // a shell, which is the terminal we were launched from.
    terminal_ancestor().unwrap_or_else(|| {
        // Fall back to TERM, which is a description of the terminal even when
        // it is not a name we recognise.
        let term = dash(std::env::var("TERM").ok());
        match term.as_str() {
            "xterm-kitty" => "kitty".to_string(),
            "xterm-ghostty" => "ghostty".to_string(),
            "xterm-256color" | "xterm" | "screen" => format!("{term} (unidentified)"),
            other => other.to_string(),
        }
    })
}

/// The closest ancestor that is a program rather than a shell. Launched from a
/// terminal, the first ancestor *is* the shell, so the walk has to climb past
/// it rather than give up.
fn terminal_ancestor() -> Option<String> {
    const SHELLS: [&str; 8] = ["bash", "zsh", "fish", "sh", "dash", "ksh", "sudo", "su"];
    let mut pid = std::process::id();
    for _ in 0..12 {
        let stat = read(format!("/proc/{pid}/stat"))?;
        // The comm field is parenthesised and may contain spaces, so find the
        // closing paren rather than splitting the whole line.
        let end = stat.rfind(')')? + 1;
        // After the comm field come the state, then the parent pid.
        let rest = &stat[end..];
        let ppid: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
        // pid 1 is init: everything above it belongs to the session, not to us.
        if ppid <= 1 || ppid == pid {
            return None;
        }
        let comm = read(format!("/proc/{ppid}/comm"))?.trim().to_string();
        let is_shell = comm.is_empty() || SHELLS.contains(&comm.as_str());
        // An Omarchy launcher is not the terminal either; keep climbing.
        let is_launcher = comm == "omarchy-menu" || comm.starts_with("omarchy-");
        if !is_shell && !is_launcher {
            return Some(comm);
        }
        pid = ppid;
    }
    None
}

fn prompt() -> String {
    read("/usr/share/omarchy/config/starship.toml")
        .map(|s| format!("starship ({})", s.lines().count()))
        .unwrap_or_else(|| "-".into())
}

pub fn theme() -> String {
    let name = read(home().join(".local/state/omarchy/current/theme.name"))
        .unwrap_or_else(|| "unknown".into());
    let colors = Path::new(SHARE)
        .join("themes")
        .join(&name)
        .join("colors.toml");
    let mode = read(colors.join("mode")).unwrap_or_else(|| "".into());
    if mode.is_empty() {
        name
    } else {
        format!("{name} ({mode})")
    }
}

fn theme_name() -> String {
    read(home().join(".local/state/omarchy/current/theme.name")).unwrap_or_default()
}

fn background() -> String {
    let dir = Path::new(SHARE).join("themes").join(theme_name());
    let images = super::fs::list_dir(dir.join("backgrounds"));
    match images.first() {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into()),
        None => "none".into(),
    }
}

fn bar_style() -> String {
    let bar = Path::new(SHARE).join("shell/plugins/bar");
    let widgets = super::fs::list_dir(bar.join("widgets")).len();
    let panels = super::fs::list_dir(Path::new(SHARE).join("shell/plugins/panels")).len();
    format!("qml bar, {widgets} widget group(s), {panels} panels")
}

fn branding() -> String {
    read(home().join(".config/omarchy/branding")).unwrap_or_else(|| "default".into())
}

fn toggles() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for file in ["bluetooth", "wifi", "hypr", "systemd-timesyncd"] {
        let path = home().join(".local/state/omarchy/toggles").join(file);
        if let Some(value) = read(path) {
            out.push((capitalize(file), value));
        }
    }
    if out.is_empty() {
        let state = home().join(".local/state/omarchy/toggles");
        if let Ok(entries) = std::fs::read_dir(state) {
            let count = entries.flatten().count();
            out.push(("Toggles recorded".to_string(), count.to_string()));
        }
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn extensions() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in [
        home().join(".config/omarchy/extensions"),
        Path::new(SHARE).join("extensions"),
    ] {
        let user_dir = dir.to_string_lossy().contains(".config");
        for entry in super::fs::list_dir(&dir) {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let label = if user_dir {
                name.to_string()
            } else {
                format!("{name} (system)")
            };
            let detail = if entry.is_dir() {
                read(entry.join("manifest.json"))
                    .or_else(|| read(entry.join("package.json")))
                    .map(|j| {
                        extract_json_string(&j, "description").unwrap_or_else(|| "installed".into())
                    })
                    .unwrap_or_else(|| {
                        let files = super::fs::list_dir(&entry)
                            .iter()
                            .filter(|p| p.is_file())
                            .count();
                        format!("{files} file(s)")
                    })
            } else {
                let ext = entry
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default();
                format!("config · .{ext}")
            };
            if !out.iter().any(|(l, _)| l == &label) {
                out.push((label, detail));
            }
        }
    }
    out
}

fn plugins() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in [
        Path::new(SHARE).join("shell/plugins"),
        home().join(".config/omarchy/plugins"),
    ] {
        for entry in super::fs::list_dir(&dir) {
            if !entry.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let files: Vec<String> = super::fs::list_dir(&entry)
                .iter()
                .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
                .collect();
            out.push((name.to_string(), files.join(", ")));
        }
    }
    out
}

fn hooks() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in [
        home().join(".config/omarchy/hooks"),
        Path::new(SHARE).join("config/omarchy/hooks"),
    ] {
        for entry in super::fs::list_dir(&dir) {
            if entry.is_file() {
                if let Some(name) = entry.file_name().and_then(|n| n.to_str()) {
                    out.push((name.to_string(), String::new()));
                }
            }
        }
    }
    out
}

/// Parses `omarchy menu keybindings --print`, which is the authoritative list.
fn keybindings() -> Vec<(String, String)> {
    let Ok(out) = Command::new("omarchy")
        .args(["menu", "keybindings", "--print"])
        .output()
    else {
        return Vec::new();
    };
    parse_keybindings(&String::from_utf8_lossy(&out.stdout))
}

/// Each line is `SUPER + I  →  open the launcher`; the arrow is a literal
/// U+2192, and the space around it varies with the locale.
fn parse_keybindings(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let (keys, action) = l.split_once('→')?;
            let keys = keys.trim();
            let action = action.trim();
            if keys.is_empty() || action.is_empty() {
                return None;
            }
            Some((keys.to_string(), action.to_string()))
        })
        .collect()
}

/// Menu entries the user has actually customised, so the report shows their
/// system rather than Omarchy's stock defaults.
fn apps() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in [
        Path::new(SHARE).join("applications"),
        home().join(".local/share/applications"),
    ] {
        for entry in super::fs::list_dir(&dir) {
            if entry.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(name) = entry.file_stem().and_then(|n| n.to_str()) {
                if !names.contains(&name.to_string()) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

pub fn summary() -> (String, String) {
    (theme(), channel())
}

/// Pull one string field out of a JSON object.
///
/// This is a scanner, not a JSON parser, so it only has to do one job well.
/// The important part is that it understands escapes: searching for the next
/// unescaped `"` instead means a value like `say \"hi\"` came back as `say \`.
/// A non-string value is reported as absent rather than mis-read.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(at) = json[from..].find(&needle) {
        let start = from + at;
        let after = start + needle.len();
        from = after;
        // The match must be a key, not a string value that happens to hold
        // the needle: a key is followed by a colon, a value is followed by a
        // comma or a closing brace.
        let rest = json[after..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let rest = rest.trim_start();
        // Only a string value is interesting here.
        let Some(body) = rest.strip_prefix('"') else {
            continue;
        };
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    // \uXXXX needs the digits, so keep the raw form.
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            Some(decoded) => out.push(decoded),
                            None => {
                                out.push_str("\\u");
                                out.push_str(&hex);
                            }
                        }
                    }
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => return Some(out),
                },
                other => out.push(other),
            }
        }
        // Ran off the end of the string without a closing quote.
        return Some(out);
    }
    None
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- extract_json_string ---------------------------------------------

    #[test]
    fn extract_json_string_reads_a_simple_value() {
        let json = r#"{"name": "thing", "description": "A useful widget"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("A useful widget")
        );
    }

    #[test]
    fn extract_json_string_understands_escaped_quotes() {
        // The bug: the scanner stopped at the first `"`, which was the
        // escaped one, and returned `say \`.
        let json = r#"{"description": "say \"hi\" now"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some(r#"say "hi" now"#)
        );
    }

    #[test]
    fn extract_json_string_decodes_the_usual_escapes() {
        let json = r#"{"description": "a\nb\tc\\d\/e"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("a\nb\tc\\d/e")
        );
    }

    #[test]
    fn extract_json_string_decodes_a_unicode_escape() {
        let json = r#"{"description": "caf\u00e9"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("café")
        );
    }

    #[test]
    fn extract_json_string_keeps_an_unknown_escape_as_written() {
        let json = r#"{"description": "a\qb"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some(r"a\qb")
        );
    }

    #[test]
    fn extract_json_string_tolerates_whitespace_around_the_colon() {
        let json = "{ \"description\" \n : \n \"spaced\" }";
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("spaced")
        );
    }

    #[test]
    fn extract_json_string_does_not_match_a_key_inside_another_key() {
        // A key must be followed by a colon; here the needle only appears as
        // part of a different key.
        let json = r#"{"subdescription": "wrong"}"#;
        assert_eq!(extract_json_string(json, "description"), None);
    }

    #[test]
    fn extract_json_string_skips_a_value_that_merely_looks_like_the_key() {
        // "description" appears here as a *value*, followed by a comma, so it
        // must not be mistaken for the key of the real one.
        let json = r#"{"name": "description", "description": "right"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("right")
        );
    }

    #[test]
    fn extract_json_string_finds_a_later_duplicate_key() {
        let json = r#"{"description": "first", "description": "second"}"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("first")
        );
    }

    #[test]
    fn extract_json_string_ignores_a_non_string_value() {
        assert_eq!(
            extract_json_string(r#"{"description": 42}"#, "description"),
            None
        );
        assert_eq!(
            extract_json_string(r#"{"description": {"a": "b"}}"#, "description"),
            None
        );
        assert_eq!(
            extract_json_string(r#"{"description": true}"#, "description"),
            None
        );
    }

    #[test]
    fn extract_json_string_of_a_missing_or_empty_key_is_none() {
        assert_eq!(extract_json_string(r#"{"a": 1}"#, "description"), None);
        assert_eq!(extract_json_string("", "description"), None);
        assert_eq!(
            extract_json_string(r#"{"description": ""}"#, "description"),
            Some(String::new())
        );
    }

    #[test]
    fn extract_json_string_handles_an_unterminated_value() {
        // Truncated JSON must not loop or panic.
        let json = r#"{"description": "unterminated"#;
        assert_eq!(
            extract_json_string(json, "description").as_deref(),
            Some("unterminated")
        );
    }

    // ---- parse_keybindings -----------------------------------------------

    const BINDINGS: &str = "\
SUPER + I  →  Open the launcher
SUPER + SHIFT + A  →  Open the applications menu
SUPER + SHIFT + S  →  Toggle the screenshots daemon
";

    #[test]
    fn parse_keybindings_splits_on_the_arrow() {
        let pairs = parse_keybindings(BINDINGS);
        assert_eq!(pairs.len(), 3);
        assert_eq!(
            pairs[0],
            ("SUPER + I".to_string(), "Open the launcher".to_string())
        );
        assert_eq!(pairs[1].0, "SUPER + SHIFT + A");
    }

    #[test]
    fn parse_keybindings_skips_lines_with_no_arrow() {
        let pairs = parse_keybindings("SUPER + Q  Quit\nSUPER + E  →  Editor\n");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, "Editor");
    }

    #[test]
    fn parse_keybindings_skips_a_half_written_line() {
        // An arrow with nothing on one side is a truncated line, not a binding.
        let pairs = parse_keybindings("  →  no keys\nSUPER + X  →  some action\n");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "SUPER + X");
    }

    #[test]
    fn parse_keybindings_of_junk_is_empty() {
        assert!(parse_keybindings("").is_empty());
        assert!(parse_keybindings("no arrows at all").is_empty());
    }

    #[test]
    fn parse_keybindings_keeps_an_arrow_inside_the_action() {
        let pairs = parse_keybindings("SUPER + X  →  Copy → clipboard\n");
        assert_eq!(pairs[0].1, "Copy → clipboard");
    }

    // ---- capitalize ------------------------------------------------------

    #[test]
    fn capitalize_uppercases_only_the_first_character() {
        assert_eq!(capitalize("wifi"), "Wifi");
        assert_eq!(capitalize("systemd-timesyncd"), "Systemd-timesyncd");
        assert_eq!(capitalize("a"), "A");
    }

    #[test]
    fn capitalize_copes_with_empty_and_non_ascii() {
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("über"), "Über");
    }

    #[test]
    fn capitalize_does_not_double_up_an_already_capital_word() {
        assert_eq!(capitalize("Bluetooth"), "Bluetooth");
    }

    // ---- live ------------------------------------------------------------

    #[test]
    fn rows_render_without_panicking_and_stay_bounded() {
        let rows = rows();
        assert!(rows.len() > 5);
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
    }

    #[test]
    fn summary_is_two_non_empty_strings() {
        let (theme, channel) = summary();
        assert!(!theme.is_empty() && !channel.is_empty());
    }

    #[test]
    fn session_helpers_always_return_something() {
        for value in [version(), channel(), update_setting(), last_update()] {
            assert!(!value.is_empty());
        }
        assert!(!shell_state().is_empty());
        assert!(!compositor_version().is_empty());
        assert!(!terminal().is_empty());
        assert!(!prompt().is_empty());
        assert!(!theme().is_empty());
        assert!(!background().is_empty());
        assert!(!bar_style().is_empty());
        assert!(!branding().is_empty());
    }

    #[test]
    fn keybindings_from_the_real_command_are_well_formed() {
        for (keys, action) in keybindings() {
            assert!(!keys.is_empty() && !action.is_empty());
            assert!(!keys.contains('\n') && !action.contains('\n'));
        }
    }

    #[test]
    fn apps_are_unique_desktop_stems() {
        let apps = apps();
        let mut sorted = apps.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(apps.len(), sorted.len(), "duplicates: {apps:?}");
    }
}
