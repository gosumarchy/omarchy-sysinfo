use super::{
    fs::{read, read_u64},
    units::dash,
    Row,
};
use std::path::Path;

pub fn rows() -> Vec<Row> {
    let devices = super::fs::list_dir("/sys/bus/usb/devices");
    let mut rows = Vec::new();
    let mut devices_seen = Vec::new();
    let ports = root_hubs();

    for dev in devices {
        // The sysfs name is the port path, e.g. `1-5`. `devnum` is the device
        // number on the bus, which reads like a different port.
        let port = dev
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // `usb1`/`usb2` are the root hubs, reported separately; interfaces
        // (`1-1:1.0`) are not devices.
        if port.contains(':') || port.starts_with("usb") {
            continue;
        }
        // Hubs often carry no product string, and their class code is not
        // reliable; `maxchild` is only exported for hubs.
        let product = match read(dev.join("product")) {
            Some(product) => {
                let manufacturer = dash(read(dev.join("manufacturer")));
                format!("{manufacturer} {product}")
            }
            None if dev.join("maxchild").exists() => {
                let in_use = read_u64(dev.join("maxchild")).unwrap_or(0);
                if in_use > 0 {
                    format!("USB hub · {in_use} port(s) in use")
                } else {
                    "USB hub · nothing plugged in".to_string()
                }
            }
            None => continue,
        };
        let speed = read(dev.join("speed"))
            .and_then(|s| s.parse::<u32>().ok())
            .map(|s| format!("{s} Mb/s"))
            .unwrap_or_else(|| "speed unknown".to_string());
        let driver = super::fs::driver_name(dev.join("driver")).unwrap_or_else(|| "-".into());
        devices_seen.push((port, format!("{product}  ·  {speed}  {driver}")));
    }

    if !devices_seen.is_empty() {
        rows.push(Row::Header("USB devices".into()));
        // Sort by bus then port number. Sorting the text put 1-10 before 1-2.
        devices_seen.sort_by_key(|(port, _)| port_key(port));
        for (port, detail) in devices_seen {
            rows.push(Row::field(port, detail));
        }
    }

    if !ports.is_empty() {
        rows.push(Row::Header("Root hubs".into()));
        for (num, name) in ports {
            rows.push(Row::field(num, name));
        }
    }

    if rows.is_empty() {
        rows.push(Row::note("no usb devices"));
    }
    rows
}

/// `1-0:1.0` belongs to bus 1; the root hub of that bus is `usb1`.
fn bus_of(name: &str) -> &str {
    name.split('-').next().unwrap_or(name)
}

/// Sort key for a port path like `1-5.2`: bus number, then the numeric parts
/// of the path, each compared as a number. Unparseable names sort last.
fn port_key(port: &str) -> Vec<u64> {
    port.split(['-', '.'])
        .map(|p| p.parse::<u64>().unwrap_or(u64::MAX))
        .collect()
}

/// Every root hub we can see, with the fastest speed it advertises. The
/// `speed` file only exists on the hub device itself, not on its interfaces.
fn root_hubs() -> Vec<(String, String)> {
    let dir = Path::new("/sys/bus/usb/devices");
    let mut out = Vec::new();
    for hub in super::fs::list_dir(dir) {
        let Some(name) = hub.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("usb") {
            continue;
        }
        let bus = name.trim_start_matches("usb");
        let speed = read(hub.join("speed"))
            .and_then(|s| s.parse::<u32>().ok())
            .map(|s| format!("up to {s} Mb/s"))
            .unwrap_or_else(|| "speed unknown".to_string());
        let interfaces = super::fs::list_dir(dir)
            .iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    // `1-0:1.0` is the root hub's own interface, not a device
                    // hanging off it.
                    .map(|n| bus_of(n) == bus && n.contains(':') && !n.contains("-0:"))
                    .unwrap_or(false)
            })
            .count();
        out.push((
            name.to_string(),
            format!("{speed}, {interfaces} interface(s)"),
        ));
    }
    out
}

/// Wireless chips expose link state through `iw`, which is far more complete
/// than the sparse `sysfs` wireless directory.
pub fn wireless() -> Vec<Row> {
    let mut rows = Vec::new();
    let net = Path::new("/sys/class/net");

    for iface in super::fs::list_dir(net) {
        let Some(name) = iface.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !iface.join("wireless").exists() && !iface.join("phy80211").exists() {
            continue;
        }
        let path = net.join(name);
        rows.push(Row::Header(name.to_string()));

        let driver =
            super::fs::driver_name(path.join("device/driver")).unwrap_or_else(|| "-".into());
        let mac = read(path.join("address")).unwrap_or_else(|| "-".into());
        rows.push(Row::field("Driver", driver));
        rows.push(Row::field("MAC", mac));
        if let Some(state) = read(path.join("operstate")) {
            rows.push(Row::field("State", state));
        }

        match iw_link(name) {
            Some(pairs) => {
                for (label, value) in pairs {
                    rows.push(Row::field(label, value));
                }
            }
            None => {
                for (label, field) in [("SSID", "ssid"), ("Band", "band"), ("Channel", "channel")] {
                    if let Some(v) = wireless_field(&path, field) {
                        rows.push(Row::field(label, v));
                    }
                }
            }
        }

        for rx in ["rx_bytes", "tx_bytes"] {
            if let Some(v) = read_u64(path.join("statistics").join(rx)) {
                rows.push(Row::field(format!("  {rx}"), super::units::human_bytes(v)));
            }
        }
    }
    rows
}

/// `iw dev <iface> link`, parsed into label/value pairs.
fn iw_link(iface: &str) -> Option<Vec<(String, String)>> {
    let out = std::process::Command::new("iw")
        .args(["dev", iface, "link"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_iw_link(&text)
}

fn parse_iw_link(text: &str) -> Option<Vec<(String, String)>> {
    if !text.contains("Connected to") {
        return None;
    }
    let mut pairs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // "Connected to aa:bb:cc:dd:ee:ff (on wlan0)" must be split on the
        // known prefix, not on the first colon: splitting there put "aa" into
        // the key and the rest of the MAC into the value, so the BSSID was
        // mangled and the label came out as "Connected to aa".
        let (key, value) = match line.strip_prefix("Connected to ") {
            Some(rest) => ("Connected to", rest.trim()),
            None => match line.split_once(':') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            },
        };
        if key.is_empty() || value.is_empty() {
            continue;
        }
        // The BSSID is followed by the interface name in parentheses.
        let value = if key == "Connected to" {
            value.split_whitespace().next().unwrap_or(value)
        } else {
            value
        };
        // Note: an unparseable field must not abort the whole parse. Using
        // `?` here returned from `iw_link` and threw away every other field
        // because of one bad line.
        let value = match key {
            "freq" => match value.parse::<f64>() {
                Ok(mhz) => {
                    // `iw` reports MHz; a value below 100 is a GHz figure from
                    // another tool and is scaled up. The old check was "over
                    // 1000 means GHz", which sent every 2.4, 5 and 6 GHz
                    // channel down the GHz branch and reported 5180 MHz as
                    // "5.18 GHz".
                    let mhz = if mhz < 100.0 { mhz * 1000.0 } else { mhz };
                    format!("{mhz:.0} MHz (ch {})", mhz_to_channel(mhz))
                }
                Err(_) => value.to_string(),
            },
            _ => value.to_string(),
        };
        let label = match key {
            "Connected to" => "Access point".to_string(),
            other => {
                let mut c = other.chars();
                match c.next() {
                    Some(f) => format!("{}{}", f.to_uppercase(), c.as_str()),
                    None => continue,
                }
            }
        };
        pairs.push((label, value));
    }
    (!pairs.is_empty()).then_some(pairs)
}

/// The 802.11 channel number for a frequency in MHz.
///
/// The three bands are separate numberings, not one line: 2.4 GHz starts at
/// channel 1 on 2412 MHz, 5 GHz is `5000 + 5 * channel` for channels 36 to 177,
/// and 6 GHz restarts at channel 1 on 5955 MHz. A single fit across the band
/// boundaries reported nonsense, and frequencies that belong to no band (the
/// gap around 5900 MHz) now report 0 instead of a made-up channel.
fn mhz_to_channel(mhz: f64) -> u32 {
    // 2.4 GHz: channel 1 is 2412 MHz, channel 13 is 2472 MHz and channel 14
    // jumps 12 MHz to 2484, so it does not sit on the 5 MHz grid.
    if (2412.0..=2484.0).contains(&mhz) {
        if mhz >= 2484.0 {
            return 14;
        }

        return (((mhz - 2412.0) / 5.0).round() as u32) + 1;
    }
    // 5 GHz: channels 36 (5180 MHz) to 177 (5885 MHz).
    if (5180.0..=5885.0).contains(&mhz) {
        return ((mhz - 5000.0) / 5.0).round() as u32;
    }
    // 6 GHz: channel 1 is 5955 MHz, channel 233 is 7115 MHz.
    if (5955.0..=7115.0).contains(&mhz) {
        return (((mhz - 5955.0) / 5.0).round() as u32) + 1;
    }
    0
}

fn wireless_field(base: &Path, field: &str) -> Option<String> {
    let dir = base.join("wireless");
    let first = super::fs::list_dir(&dir).into_iter().next()?;
    read(first.join(field)).filter(|v| v != "0" && !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- bus_of ----------------------------------------------------------

    #[test]
    fn bus_of_takes_the_leading_bus_number() {
        assert_eq!(bus_of("1-0:1.0"), "1");
        assert_eq!(bus_of("2-1.3"), "2");
        assert_eq!(bus_of("1-1:1.0"), "1");
    }

    #[test]
    fn bus_of_copes_with_a_name_that_has_no_dash() {
        assert_eq!(bus_of("usb1"), "usb1");
        assert_eq!(bus_of(""), "");
    }

    // ---- port_key --------------------------------------------------------

    #[test]
    fn port_key_orders_ports_numerically_not_alphabetically() {
        // The bug: sorting "1-10" before "1-2" because "1" < "2".
        let mut ports = vec!["1-10", "1-2", "1-1"];
        ports.sort_by_key(|p| port_key(p));
        assert_eq!(ports, vec!["1-1", "1-2", "1-10"]);
    }

    #[test]
    fn port_key_orders_buses_before_ports() {
        let mut ports = vec!["2-1", "1-9", "10-1", "1-10"];
        ports.sort_by_key(|p| port_key(p));
        assert_eq!(ports, vec!["1-9", "1-10", "2-1", "10-1"]);
    }

    #[test]
    fn port_key_handles_deeply_nested_ports() {
        let mut ports = vec!["1-1.10", "1-1.2", "1-1"];
        ports.sort_by_key(|p| port_key(p));
        assert_eq!(ports, vec!["1-1", "1-1.2", "1-1.10"]);
    }

    #[test]
    fn port_key_sorts_junk_last_instead_of_panicking() {
        let key = port_key("weird");
        assert!(key.contains(&u64::MAX));
        assert!(!port_key("").is_empty());
    }

    // ---- mhz_to_channel --------------------------------------------------

    #[test]
    fn mhz_to_channel_maps_the_2ghz_band() {
        assert_eq!(mhz_to_channel(2412.0), 1);
        assert_eq!(mhz_to_channel(2437.0), 6);
        assert_eq!(mhz_to_channel(2462.0), 11);
        assert_eq!(mhz_to_channel(2484.0), 14, "channel 14 is the last one");
    }

    #[test]
    fn mhz_to_channel_maps_the_5ghz_band() {
        assert_eq!(mhz_to_channel(5180.0), 36);
        assert_eq!(mhz_to_channel(5500.0), 100);
        assert_eq!(mhz_to_channel(5745.0), 149);
        assert_eq!(mhz_to_channel(5845.0), 169);
        assert_eq!(mhz_to_channel(5885.0), 177, "the last 5 GHz channel");
    }

    #[test]
    fn mhz_to_channel_maps_the_6ghz_band_which_restarts_at_one() {
        assert_eq!(mhz_to_channel(5955.0), 1, "6 GHz restarts at channel 1");
        assert_eq!(mhz_to_channel(5965.0), 3);
        assert_eq!(mhz_to_channel(5975.0), 5);
        assert_eq!(mhz_to_channel(7115.0), 233);
    }

    #[test]
    fn mhz_to_channel_returns_zero_for_frequencies_that_are_not_a_channel() {
        // Below the 2.4 GHz band, in the gap between the 2.4 and 5 GHz bands,
        // in the dead zone between the 5 and 6 GHz bands, and outside wifi.
        assert_eq!(mhz_to_channel(0.0), 0);
        assert_eq!(mhz_to_channel(2400.0), 0);
        assert_eq!(mhz_to_channel(5000.0), 0, "not a defined 5 GHz channel");
        assert_eq!(mhz_to_channel(5925.0), 0, "belongs to no band");
        assert_eq!(mhz_to_channel(5900.0), 0);
        assert_eq!(mhz_to_channel(10000.0), 0);
        assert_eq!(mhz_to_channel(-100.0), 0);
    }

    // ---- iw_link ---------------------------------------------------------

    const IW_LINK: &str = "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\n\
    SSID: MyNet\n\
    freq: 5180\n\
    rx bitrate: 1444002 BPS\n\
    signal: -42 dBm\n";

    #[test]
    fn iw_link_reduces_the_bssid_to_the_address() {
        // The bug: splitting the line on its first colon put "aa" in the key
        // and "bb:cc:dd:ee:ff (on wlan0)" in the value.
        let pairs = parse_iw_link(IW_LINK).expect("parsed");
        let ap = pairs
            .iter()
            .find(|(k, _)| k == "Access point")
            .expect("ap row");
        assert_eq!(ap.1, "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn iw_link_labels_the_remaining_fields() {
        let pairs = parse_iw_link(IW_LINK).expect("parsed");
        let get = |k: &str| pairs.iter().find(|(l, _)| l == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("SSID"),
            Some("MyNet"),
            "the label keeps its own capitalisation"
        );
        assert_eq!(get("Freq"), Some("5180 MHz (ch 36)"));
        assert_eq!(get("Signal"), Some("-42 dBm"));
        assert_eq!(get("Rx bitrate"), Some("1444002 BPS"));
    }

    #[test]
    fn iw_link_keeps_every_field_when_one_of_them_is_unparseable() {
        // The bug: `value.parse().ok()?` inside the loop returned from the
        // whole function, so one bad freq lost the entire link report.
        let text = "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\n\
                    SSID: MyNet\n\
                    freq: not-a-number\n\
                    signal: -42 dBm\n";
        let pairs = parse_iw_link(text).expect("still parsed");
        let get = |k: &str| pairs.iter().find(|(l, _)| l == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SSID"), Some("MyNet"), "other fields must survive");
        assert_eq!(get("Signal"), Some("-42 dBm"));
        assert_eq!(
            get("Freq"),
            Some("not-a-number"),
            "the bad value is shown as-is"
        );
    }

    #[test]
    fn iw_link_keeps_the_frequency_in_mhz() {
        // The bug: the old check treated anything over 1000 as GHz, so 5180
        // was reported as "5.18 GHz" and 2412 as "2.41 GHz".
        for (freq, want) in [
            ("2412", "2412 MHz (ch 1)"),
            ("2437", "2437 MHz (ch 6)"),
            ("5180", "5180 MHz (ch 36)"),
            ("5955", "5955 MHz (ch 1)"),
        ] {
            let pairs = parse_iw_link(&format!(
                "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\nfreq: {freq}\n"
            ))
            .expect("parsed");
            let got = pairs.iter().find(|(k, _)| k == "Freq").expect("freq row");
            assert_eq!(&got.1, want);
        }
    }

    #[test]
    fn iw_link_scales_a_ghz_figure_up_to_mhz() {
        let pairs = parse_iw_link("Connected to aa:bb:cc:dd:ee:ff (on wlan0)\nfreq: 5.18\n")
            .expect("parsed");
        let freq = pairs.iter().find(|(k, _)| k == "Freq").expect("freq row");
        assert_eq!(freq.1, "5180 MHz (ch 36)");
    }

    #[test]
    fn iw_link_of_a_disconnected_interface_is_none() {
        // The fallback to the sparse sysfs fields happens on None.
        assert_eq!(parse_iw_link("Not connected.\n"), None);
        assert_eq!(parse_iw_link(""), None);
    }

    #[test]
    fn iw_link_skips_blank_and_colonless_lines() {
        let text = "Connected to aa:bb:cc:dd:ee:ff\n\nno colon here\nSSID:\n: orphan\nSSID: ok\n";
        let pairs = parse_iw_link(text).expect("parsed");
        assert_eq!(pairs.len(), 2, "{pairs:?}");
        assert!(pairs.iter().any(|(k, _)| k == "SSID"), "{pairs:?}");
    }

    // ---- live ------------------------------------------------------------

    #[test]
    fn rows_and_wireless_render_without_panicking() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
        let wifi = wireless();
        let text: String = wifi.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
    }

    #[test]
    fn rows_report_either_devices_or_an_explicit_note() {
        let rows = rows();
        assert!(!rows.is_empty());
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(
            text.contains("no usb devices") || text.contains("USB"),
            "{text}"
        );
    }
}
