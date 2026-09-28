//! Omarchy itself: release, session, theme, and what the user has added.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::fs::{file_name, list_dir, read};
use super::units::{capitalise, dash, human_secs};
use super::{Host, Row};

const SHARE: &str = "/usr/share/omarchy";

/// How many keybindings the section lists; the full table runs to hundreds.
const KEYBINDINGS_SHOWN: usize = 40;

/// Facts that only change with a package upgrade, read once per process.
/// Two of them come from running programs, which is too slow to repeat on
/// every refresh.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fixed {
    version: String,
    channel: String,
    keybindings: Vec<Keybinding>,
}

impl Fixed {
    pub(crate) fn read(host: &Host) -> Fixed {
        Fixed {
            version: version(host),
            channel: host
                .run("omarchy-channel-current", &[])
                .unwrap_or_else(|| "unknown".into()),
            keybindings: host
                .run("omarchy", &["menu", "keybindings", "--print"])
                .map(|out| parse_keybindings(&out))
                .unwrap_or_default(),
        }
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

/// One line of `omarchy menu keybindings --print`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Keybinding {
    keys: String,
    action: String,
}

pub(crate) fn rows(host: &Host, fixed: &Fixed) -> Vec<Row> {
    let mut rows = vec![
        Row::header("Release"),
        Row::field("Version", fixed.version.as_str()),
        Row::field("Channel", fixed.channel.as_str()),
        Row::field("Update policy", update_setting(host)),
        Row::field("Last update", last_update(host)),
        Row::field("Shell", shell_state(host)),
    ];

    // Locale and timezone are in the OS section; they were printed twice.
    rows.push(Row::header("Session"));
    rows.push(Row::field(
        "Desktop",
        dash(std::env::var("XDG_CURRENT_DESKTOP").ok()),
    ));
    rows.push(Row::field("Compositor", compositor_version(host)));
    rows.push(Row::field("Terminal", terminal(host)));
    rows.push(Row::field("Shell prompt", prompt(host)));

    rows.push(Row::header("Appearance"));
    rows.push(Row::field("Theme", theme(host)));
    rows.push(Row::field("Background", background(host)));
    rows.push(Row::field("Bar", bar_style(host)));
    rows.push(Row::field("Bar contents", bar_widgets(host)));
    rows.push(Row::field(
        "Branding",
        host.home_path(".config/omarchy/branding")
            .and_then(read)
            .unwrap_or_else(|| "default".into()),
    ));

    let toggles = toggles(host);
    if !toggles.is_empty() {
        rows.push(Row::header("Toggles"));
        rows.extend(toggles);
    }

    let extensions = extensions(host);
    if !extensions.is_empty() {
        rows.push(Row::header("Extensions"));
        rows.extend(extensions);
    }

    let plugins = plugins(host);
    if !plugins.is_empty() {
        rows.push(Row::header("Shell plugins"));
        rows.extend(plugins);
    }

    let hooks = hooks(host);
    if !hooks.is_empty() {
        rows.push(Row::header("Hooks"));
        rows.extend(hooks.into_iter().map(|name| Row::field(name, "configured")));
    }

    let apps = apps(host);
    if !apps.is_empty() {
        rows.push(Row::header(format!("Applications ({})", apps.len())));
        let apps: Vec<String> = apps.into_iter().collect();
        for chunk in apps.chunks(4) {
            rows.push(Row::field("  menu entries", chunk.join(", ")));
        }
        rows.push(Row::Blank);
    }

    rows.push(Row::header("Keybindings"));
    if fixed.keybindings.is_empty() {
        rows.push(Row::note("omarchy menu keybindings --print gave nothing"));
    }
    rows.extend(
        fixed
            .keybindings
            .iter()
            .take(KEYBINDINGS_SHOWN)
            .map(|k| Row::field(k.keys.as_str(), k.action.as_str())),
    );

    rows
}

/// The installed package version, from pacman's local database rather than
/// by running `pacman -Q`, and the version the source tree claims.
fn version(host: &Host) -> String {
    let package = installed_version(host, "omarchy");
    let source = host
        .read(format!("{SHARE}/version"))
        .unwrap_or_else(|| "unknown".into());

    match package {
        None => source,
        Some(built) if built.starts_with(&source) => built,
        Some(built) => format!("{built} (source {source})"),
    }
}

/// `%VERSION%` of an installed package, found by its `%NAME%` so that
/// `omarchy-keyring` is not mistaken for `omarchy`.
fn installed_version(host: &Host, package: &str) -> Option<String> {
    host.list_dir("/var/lib/pacman/local")
        .into_iter()
        .filter(|dir| file_name(dir).starts_with(package))
        .filter_map(|dir| read(dir.join("desc")))
        .find_map(|desc| {
            if desc_field(&desc, "NAME")? != package {
                return None;
            }

            desc_field(&desc, "VERSION").map(str::to_string)
        })
}

/// One `%KEY%` section of a pacman `desc` file: the heading, then the value
/// on the next line.
fn desc_field<'a>(desc: &'a str, key: &str) -> Option<&'a str> {
    let heading = format!("%{key}%");
    let mut lines = desc.lines();

    lines.by_ref().find(|l| l.trim() == heading)?;
    lines.next().map(str::trim).filter(|v| !v.is_empty())
}

fn update_setting(host: &Host) -> String {
    host.home_path(".config/omarchy/update.conf")
        .and_then(read)
        .or_else(|| host.read(format!("{SHARE}/config/omarchy/update.conf")))
        .unwrap_or_else(|| "not configured".into())
}

/// The most recent migration marker, and how long ago it was written.
fn last_update(host: &Host) -> String {
    let Some(dir) = host.home_path(".local/state/omarchy/done") else {
        return "no record".into();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return "no record".into();
    };

    let newest = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // First-run markers are written once at install time and are not
            // evidence of an update.
            if name.starts_with("first-run") {
                return None;
            }
            let modified = e.metadata().ok()?.modified().ok()?;

            Some((modified, name))
        })
        .max();

    match newest {
        Some((modified, name)) => format!("{name} ({})", ago(modified, SystemTime::now())),
        None => "no record".into(),
    }
}

/// `3d 4h 5m ago`. The old row printed the time since 1970.
fn ago(then: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(then).map_or(0, |d| d.as_secs());

    format!("{} ago", human_secs(secs))
}

fn shell_state(host: &Host) -> &'static str {
    // Releases before 4 ran a process literally called omarchy-shell; 4 and
    // later run the shell under quickshell.
    if host.exists("/run/omarchy-shell") || process_running(host, "omarchy-shell") {
        "omarchy-shell"
    } else if process_running(host, "quickshell") {
        "quickshell (bar, menus)"
    } else {
        "not running"
    }
}

/// Whether any process has this exact `comm`, by walking `/proc` rather than
/// forking `pgrep` on every refresh.
fn process_running(host: &Host, comm: &str) -> bool {
    host.list_dir("/proc").iter().any(|dir| {
        file_name(dir).chars().all(|c| c.is_ascii_digit())
            && read(dir.join("comm")).as_deref() == Some(comm)
    })
}

fn compositor_version(host: &Host) -> String {
    host.hypr("version")
        .and_then(|v| {
            v.lines()
                .find(|l| l.contains("Hyprland"))
                .map(|l| l.trim().to_string())
        })
        .unwrap_or_else(|| "-".into())
}

fn terminal(host: &Host) -> String {
    // Emulators advertise themselves through the environment. The order is
    // roughly how likely each one is to be the one running Omarchy.
    for (var, name) in [
        ("ALACRITTY_LOG", "alacritty"),
        ("KITTY_WINDOW_ID", "kitty"),
        ("GHOSTTY_RESOURCES_DIR", "ghostty"),
        ("VTE_VERSION", "gnome-terminal"),
        ("WEZTERM_EXECUTABLE", "wezterm"),
        ("KONSOLE_VERSION", "konsole"),
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
    terminal_ancestor(host, std::process::id()).unwrap_or_else(|| {
        // Fall back to TERM, which is a description of the terminal even when
        // it is not a name we recognise.
        let term = dash(std::env::var("TERM").ok());
        match term.as_str() {
            "xterm-kitty" => "kitty".to_string(),
            "xterm-ghostty" => "ghostty".to_string(),
            "xterm-256color" | "xterm" | "screen" => format!("{term} (unidentified)"),
            _ => term,
        }
    })
}

/// The closest ancestor of `pid` that is a program rather than a shell.
/// Launched from a terminal, the first ancestor *is* the shell, so the walk
/// has to climb past it rather than give up.
fn terminal_ancestor(host: &Host, mut pid: u32) -> Option<String> {
    const SHELLS: [&str; 8] = ["bash", "zsh", "fish", "sh", "dash", "ksh", "sudo", "su"];

    for _ in 0..12 {
        let stat = host.read(format!("/proc/{pid}/stat"))?;
        let ppid = parent_pid(&stat)?;
        // pid 1 is init: everything above it belongs to the session, not to us.
        if ppid <= 1 || ppid == pid {
            return None;
        }

        let comm = host.read(format!("/proc/{ppid}/comm"))?;
        // An Omarchy launcher is not the terminal either; keep climbing.
        if !SHELLS.contains(&comm.as_str()) && !comm.starts_with("omarchy-") {
            return Some(comm);
        }
        pid = ppid;
    }

    None
}

/// The parent pid out of `/proc/<pid>/stat`. The comm field is parenthesised
/// and may itself contain spaces and parentheses, so the fields are counted
/// from the last closing paren.
fn parent_pid(stat: &str) -> Option<u32> {
    let after_comm = &stat[stat.rfind(')')? + 1..];

    // State, then the parent pid.
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

fn prompt(host: &Host) -> String {
    host.read(format!("{SHARE}/config/starship.toml"))
        .map_or_else(
            || "-".into(),
            |s| format!("starship ({} lines)", s.lines().count()),
        )
}

/// The name of the active theme, if Omarchy has recorded one.
fn theme_name(host: &Host) -> Option<String> {
    host.home_path(".local/state/omarchy/current/theme.name")
        .and_then(read)
}

/// Where the active theme lives: a user-installed theme under
/// `~/.config/omarchy/themes` wins over one Omarchy ships.
pub(crate) fn theme_dir(host: &Host) -> Option<PathBuf> {
    let name = theme_name(host)?;
    // A theme name is a directory name; anything else is not one to join.
    if name.contains('/') || name == ".." || name == "." {
        return None;
    }

    host.home_path(".config/omarchy/themes")
        .map(|dir| dir.join(&name))
        .into_iter()
        .chain(std::iter::once(host.path(format!("{SHARE}/themes/{name}"))))
        .find(|dir| dir.is_dir())
}

/// `tokyo-night (dark)`. Omarchy marks a light theme with a `light.mode`
/// file in its directory; the old code looked for `colors.toml/mode`, a
/// path that cannot exist, so the mode never showed.
fn theme(host: &Host) -> String {
    let Some(name) = theme_name(host) else {
        return "unknown".into();
    };

    match theme_dir(host) {
        Some(dir) if dir.join("light.mode").exists() => format!("{name} (light)"),
        Some(_) => format!("{name} (dark)"),
        None => name,
    }
}

fn background(host: &Host) -> String {
    let Some(dir) = theme_dir(host) else {
        return "none".into();
    };

    list_dir(dir.join("backgrounds"))
        .first()
        .map_or_else(|| "none".into(), |p| file_name(p))
}

fn bar_style(host: &Host) -> String {
    let widgets = host
        .list_dir(format!("{SHARE}/shell/plugins/bar/widgets"))
        .len();
    let panels = host.list_dir(format!("{SHARE}/shell/plugins/panels")).len();

    format!("qml bar, {widgets} widget group(s), {panels} panels")
}

/// Omarchy 4 renders the bar in QML, so this reads the shell plugin's widget
/// directory rather than a waybar config.
fn bar_widgets(host: &Host) -> String {
    if !host.exists(format!("{SHARE}/shell/plugins/bar/manifest.json")) {
        return "bar plugin manifest not found".into();
    }

    let names: Vec<String> = host
        .list_dir(format!("{SHARE}/shell/plugins/bar/widgets"))
        .iter()
        .filter_map(|p| file_name(p).strip_suffix(".qml").map(str::to_string))
        .collect();

    if names.is_empty() {
        "qml bar, no widgets".into()
    } else {
        names.join(", ")
    }
}

fn toggles(host: &Host) -> Vec<Row> {
    let Some(dir) = host.home_path(".local/state/omarchy/toggles") else {
        return Vec::new();
    };

    let known: Vec<Row> = ["bluetooth", "wifi", "hypr", "systemd-timesyncd"]
        .into_iter()
        .filter_map(|file| Some(Row::field(capitalise(file), read(dir.join(file))?)))
        .collect();
    if !known.is_empty() {
        return known;
    }

    match std::fs::read_dir(&dir) {
        Ok(entries) => vec![Row::field(
            "Toggles recorded",
            entries.flatten().count().to_string(),
        )],
        Err(_) => Vec::new(),
    }
}

/// Where extensions come from, which decides how they are labelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    User,
    System,
}

fn extensions(host: &Host) -> Vec<Row> {
    let sources = [
        (host.home_path(".config/omarchy/extensions"), Origin::User),
        (
            Some(host.path(format!("{SHARE}/extensions"))),
            Origin::System,
        ),
    ];
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();

    for (dir, origin) in sources {
        let Some(dir) = dir else {
            continue;
        };
        for entry in list_dir(&dir) {
            let name = file_name(&entry);
            let label = match origin {
                Origin::User => name,
                Origin::System => format!("{name} (system)"),
            };
            if seen.insert(label.clone()) {
                rows.push(Row::field(label, extension_detail(&entry)));
            }
        }
    }

    rows
}

fn extension_detail(entry: &Path) -> String {
    if !entry.is_dir() {
        let ext = entry
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();

        return format!("config · .{ext}");
    }

    let manifest = read(entry.join("manifest.json")).or_else(|| read(entry.join("package.json")));
    if let Some(json) = manifest {
        extract_json_string(&json, "description").unwrap_or_else(|| "installed".into())
    } else {
        let files = list_dir(entry).iter().filter(|p| p.is_file()).count();
        format!("{files} file(s)")
    }
}

fn plugins(host: &Host) -> Vec<Row> {
    [
        Some(host.path(format!("{SHARE}/shell/plugins"))),
        host.home_path(".config/omarchy/plugins"),
    ]
    .into_iter()
    .flatten()
    .flat_map(|dir| list_dir(&dir))
    .filter(|entry| entry.is_dir())
    .map(|entry| {
        let files: Vec<String> = list_dir(&entry).iter().map(|p| file_name(p)).collect();

        Row::field(file_name(&entry), files.join(", "))
    })
    .collect()
}

fn hooks(host: &Host) -> Vec<String> {
    [
        host.home_path(".config/omarchy/hooks"),
        Some(host.path(format!("{SHARE}/config/omarchy/hooks"))),
    ]
    .into_iter()
    .flatten()
    .flat_map(|dir| list_dir(&dir))
    .filter(|entry| entry.is_file())
    .map(|entry| file_name(&entry))
    .collect()
}

/// Launcher entries: the ones Omarchy ships plus any the user added, each
/// named once and in order.
fn apps(host: &Host) -> BTreeSet<String> {
    [
        Some(host.path(format!("{SHARE}/applications"))),
        host.home_path(".local/share/applications"),
    ]
    .into_iter()
    .flatten()
    .flat_map(|dir| list_dir(&dir))
    .filter(|entry| entry.extension().is_some_and(|e| e == "desktop"))
    .filter_map(|entry| entry.file_stem().map(|s| s.to_string_lossy().into_owned()))
    .collect()
}

/// Theme and channel for the overview.
pub(crate) fn summary(host: &Host, fixed: &Fixed) -> (String, String) {
    (theme(host), fixed.channel.clone())
}

/// Each line is `SUPER + I  →  open the launcher`; the arrow is a literal
/// U+2192, and the space around it varies with the locale.
fn parse_keybindings(out: &str) -> Vec<Keybinding> {
    out.lines()
        .filter_map(|l| {
            let (keys, action) = l.split_once('→')?;
            let (keys, action) = (keys.trim(), action.trim());
            if keys.is_empty() || action.is_empty() {
                return None;
            }

            Some(Keybinding {
                keys: keys.to_string(),
                action: action.to_string(),
            })
        })
        .collect()
}

/// Pull one string field out of a JSON object.
///
/// This is a scanner, not a JSON parser, so it only has to do one job well.
/// The important part is that it understands escapes: searching for the next
/// unescaped `"` instead means a value like `say \"hi\"` came back as `say \`.
/// A non-string value is reported as absent rather than mis-read. Decoded
/// control characters (`\n`, `\u001b`) are harmless: every row is sanitised
/// before it reaches the terminal.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0;

    while let Some(at) = json[from..].find(&needle) {
        let after = from + at + needle.len();
        from = after;
        // The match must be a key, not a string value that happens to hold
        // the needle: a key is followed by a colon, a value is followed by a
        // comma or a closing brace.
        let Some(rest) = json[after..].trim_start().strip_prefix(':') else {
            continue;
        };
        // Only a string value is interesting here.
        let Some(body) = rest.trim_start().strip_prefix('"') else {
            continue;
        };

        return Some(decode_json_string(body));
    }

    None
}

/// The body of a JSON string up to its closing quote, escapes decoded. A
/// string that runs off the end is returned as far as it got.
fn decode_json_string(body: &str) -> String {
    let mut out = String::new();
    let mut chars = body.chars();

    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                // \uXXXX needs the digits; a bad escape keeps its raw form.
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(decoded) =
                        u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                    {
                        out.push(decoded);
                    } else {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => break,
            },
            other => out.push(other),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;
    use std::time::Duration;

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
            Keybinding {
                keys: "SUPER + I".to_string(),
                action: "Open the launcher".to_string(),
            }
        );
        assert_eq!(pairs[1].keys, "SUPER + SHIFT + A");
    }

    #[test]
    fn parse_keybindings_skips_lines_with_no_arrow() {
        let pairs = parse_keybindings("SUPER + Q  Quit\nSUPER + E  →  Editor\n");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].action, "Editor");
    }

    #[test]
    fn parse_keybindings_skips_a_half_written_line() {
        // An arrow with nothing on one side is a truncated line, not a binding.
        let pairs = parse_keybindings("  →  no keys\nSUPER + X  →  some action\n");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].keys, "SUPER + X");
    }

    #[test]
    fn parse_keybindings_of_junk_is_empty() {
        assert!(parse_keybindings("").is_empty());
        assert!(parse_keybindings("no arrows at all").is_empty());
    }

    #[test]
    fn parse_keybindings_keeps_an_arrow_inside_the_action() {
        let pairs = parse_keybindings("SUPER + X  →  Copy → clipboard\n");
        assert_eq!(pairs[0].action, "Copy → clipboard");
    }

    // ---- pacman ---------------------------------------------------------------

    const DESC: &str = "%NAME%\nomarchy\n\n%VERSION%\n3.1.0-1\n\n%DESC%\nThe desktop\n";

    #[test]
    fn desc_fields_are_read_by_heading() {
        assert_eq!(desc_field(DESC, "NAME"), Some("omarchy"));
        assert_eq!(desc_field(DESC, "VERSION"), Some("3.1.0-1"));
        assert_eq!(desc_field(DESC, "URL"), None);
    }

    #[test]
    fn the_version_comes_from_the_package_not_a_similarly_named_one() {
        let fx = Fixture::new();
        fx.write(
            "var/lib/pacman/local/omarchy-keyring-20250101-1/desc",
            "%NAME%\nomarchy-keyring\n\n%VERSION%\n20250101-1\n",
        );
        fx.write("var/lib/pacman/local/omarchy-3.1.0-1/desc", DESC);
        fx.write("usr/share/omarchy/version", "3.1.0\n");

        assert_eq!(version(&fx.host()), "3.1.0-1");
    }

    #[test]
    fn a_source_checkout_that_disagrees_with_the_package_says_so() {
        let fx = Fixture::new();
        fx.write("var/lib/pacman/local/omarchy-3.1.0-1/desc", DESC);
        fx.write("usr/share/omarchy/version", "3.2.0-dev\n");

        assert_eq!(version(&fx.host()), "3.1.0-1 (source 3.2.0-dev)");
    }

    #[test]
    fn without_a_package_the_source_version_is_used() {
        let fx = Fixture::new();
        fx.write("usr/share/omarchy/version", "3.1.0\n");

        assert_eq!(version(&fx.host()), "3.1.0");
        assert_eq!(version(&Fixture::new().host()), "unknown");
    }

    // ---- theme ----------------------------------------------------------------

    fn themed(name: &str) -> Fixture {
        let fx = Fixture::new();
        fx.write(
            "home/user/.local/state/omarchy/current/theme.name",
            &format!("{name}\n"),
        );
        fx
    }

    #[test]
    fn a_light_theme_is_recognised_by_its_marker_file() {
        let fx = themed("catppuccin-latte");
        fx.write("usr/share/omarchy/themes/catppuccin-latte/light.mode", "");

        assert_eq!(theme(&fx.host()), "catppuccin-latte (light)");
    }

    #[test]
    fn a_theme_without_the_marker_is_dark() {
        let fx = themed("tokyo-night");
        fx.mkdir("usr/share/omarchy/themes/tokyo-night");

        assert_eq!(theme(&fx.host()), "tokyo-night (dark)");
    }

    #[test]
    fn a_user_theme_wins_over_a_shipped_one() {
        let fx = themed("mine");
        fx.mkdir("usr/share/omarchy/themes/mine");
        fx.write("home/user/.config/omarchy/themes/mine/light.mode", "");
        fx.write(
            "home/user/.config/omarchy/themes/mine/backgrounds/1.png",
            "",
        );

        let host = fx.host();
        assert_eq!(
            theme_dir(&host),
            Some(fx.dir().join("home/user/.config/omarchy/themes/mine"))
        );
        assert_eq!(theme(&host), "mine (light)");
        assert_eq!(background(&host), "1.png");
    }

    #[test]
    fn a_theme_name_cannot_walk_out_of_the_themes_directory() {
        let fx = themed("../../../etc");

        assert_eq!(theme_dir(&fx.host()), None);
    }

    #[test]
    fn no_theme_recorded_is_unknown() {
        assert_eq!(theme(&Fixture::new().host()), "unknown");
    }

    // ---- last update ------------------------------------------------------------

    #[test]
    fn last_update_is_an_age_not_a_time_since_1970() {
        let now = SystemTime::now();
        let then = now - Duration::from_secs(3 * 86_400 + 3_600);

        assert_eq!(ago(then, now), "3d 1h 0m ago");
        assert_eq!(ago(now + Duration::from_secs(60), now), "0m 0s ago");
    }

    #[test]
    fn last_update_skips_first_run_markers() {
        let fx = Fixture::new();
        fx.write("home/user/.local/state/omarchy/done/first-run-1", "");

        assert_eq!(last_update(&fx.host()), "no record");

        fx.write("home/user/.local/state/omarchy/done/1751234567.sh", "");
        let update = last_update(&fx.host());
        assert!(update.starts_with("1751234567.sh ("), "{update}");
        assert!(update.ends_with(" ago)"), "{update}");
    }

    // ---- processes ---------------------------------------------------------------

    #[test]
    fn parent_pid_survives_a_comm_with_spaces_and_parens() {
        assert_eq!(parent_pid("42 (my (odd) prog) S 7 42 42 0"), Some(7));
        assert_eq!(parent_pid("42 (bash) S 1"), Some(1));
        assert_eq!(parent_pid("garbage"), None);
    }

    #[test]
    fn the_terminal_is_the_first_ancestor_that_is_not_a_shell() {
        let fx = Fixture::new();
        fx.write("proc/300/stat", "300 (omarchy-sysinf) S 200 0 0");
        fx.write("proc/200/stat", "200 (zsh) S 100 0 0");
        fx.write("proc/200/comm", "zsh\n");
        fx.write("proc/100/stat", "100 (alacritty) S 1 0 0");
        fx.write("proc/100/comm", "alacritty\n");

        assert_eq!(
            terminal_ancestor(&fx.host(), 300).as_deref(),
            Some("alacritty")
        );
    }

    #[test]
    fn shell_state_scans_proc_for_quickshell() {
        let fx = Fixture::new();
        fx.write("proc/1234/comm", "quickshell\n");
        fx.write("proc/self/comm", "quickshell\n");

        assert_eq!(shell_state(&fx.host()), "quickshell (bar, menus)");
        assert_eq!(shell_state(&Fixture::new().host()), "not running");
    }

    // ---- the section against a fixture ------------------------------------------

    #[test]
    fn rows_render_on_an_empty_machine_and_note_missing_keybindings() {
        let fx = Fixture::new();
        let host = fx.host();
        let rows = rows(&host, &Fixed::read(&host));

        assert!(rows.contains(&Row::note("omarchy menu keybindings --print gave nothing")));
        assert!(
            !format!("{rows:?}").contains("Locale"),
            "locale lives in the OS section"
        );
    }

    #[test]
    fn apps_are_unique_and_sorted() {
        let fx = Fixture::new();
        fx.write("usr/share/omarchy/applications/zed.desktop", "");
        fx.write("usr/share/omarchy/applications/alacritty.desktop", "");
        fx.write("home/user/.local/share/applications/zed.desktop", "");
        fx.write("home/user/.local/share/applications/notes.txt", "");

        let apps: Vec<String> = apps(&fx.host()).into_iter().collect();
        assert_eq!(apps, ["alacritty", "zed"]);
    }

    #[test]
    fn a_user_extension_and_a_system_one_are_labelled_apart() {
        let fx = Fixture::new();
        fx.write(
            "home/user/.config/omarchy/extensions/weather/manifest.json",
            r#"{"description": "Weather in the bar"}"#,
        );
        fx.write("usr/share/omarchy/extensions/weather/manifest.json", "{}");

        assert_eq!(
            extensions(&fx.host()),
            [
                Row::field("weather", "Weather in the bar"),
                Row::field("weather (system)", "installed"),
            ]
        );
    }
}
