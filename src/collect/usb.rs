//! USB devices and root hubs from `/sys/bus/usb`, and wireless links.

use std::path::{Path, PathBuf};

use super::fs::{driver_name, file_name, list_dir, read, read_u64};
use super::units::{capitalise, human_bytes, round_u64};
use super::{Host, Row};

/// One device on the bus, as sysfs describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UsbDevice {
    /// The sysfs name is the port path, e.g. `1-5.2`. `devnum` is the device
    /// number on the bus, which reads like a different port.
    port: String,
    description: String,
}

pub(crate) fn rows(host: &Host) -> Vec<Row> {
    let entries = host.list_dir("/sys/bus/usb/devices");
    let mut rows = Vec::new();

    let mut devices: Vec<UsbDevice> = entries.iter().filter_map(|p| device(p)).collect();
    if !devices.is_empty() {
        rows.push(Row::header("USB devices"));
        // Sort by bus then port number. Sorting the text put 1-10 before 1-2.
        devices.sort_by_key(|d| port_key(&d.port));
        rows.extend(
            devices
                .into_iter()
                .map(|d| Row::field(d.port, d.description)),
        );
    }

    let hubs = root_hubs(&entries);
    if !hubs.is_empty() {
        rows.push(Row::header("Root hubs"));
        rows.extend(hubs);
    }

    if rows.is_empty() {
        rows.push(Row::note("no usb devices"));
    }

    rows
}

fn device(path: &Path) -> Option<UsbDevice> {
    let port = file_name(path);
    // `usb1`/`usb2` are the root hubs, reported separately; interfaces
    // (`1-1:1.0`) are not devices.
    if port.contains(':') || port.starts_with("usb") {
        return None;
    }

    // Hubs often carry no product string, and their class code is not
    // reliable; `maxchild` is only exported for hubs.
    let product = match read(path.join("product")) {
        Some(product) => match read(path.join("manufacturer")) {
            Some(manufacturer) => format!("{manufacturer} {product}"),
            None => product,
        },
        None => match read_u64(path.join("maxchild")) {
            Some(0) => "USB hub · nothing plugged in".to_string(),
            Some(ports) => format!("USB hub · {ports} port(s)"),
            None => return None,
        },
    };
    let speed = read(path.join("speed"))
        .and_then(|s| s.parse::<u32>().ok())
        .map_or_else(|| "speed unknown".to_string(), |s| format!("{s} Mb/s"));
    let driver = driver_name(path.join("driver")).unwrap_or_else(|| "-".into());

    Some(UsbDevice {
        port,
        description: format!("{product}  ·  {speed}  {driver}"),
    })
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

/// Every root hub, with the fastest speed it advertises and how many
/// interfaces hang off its bus.
fn root_hubs(entries: &[PathBuf]) -> Vec<Row> {
    let names: Vec<String> = entries.iter().map(|p| file_name(p)).collect();

    entries
        .iter()
        .zip(&names)
        .filter_map(|(hub, name)| {
            let bus = name.strip_prefix("usb")?;
            let speed = read(hub.join("speed"))
                .and_then(|s| s.parse::<u32>().ok())
                .map_or_else(
                    || "speed unknown".to_string(),
                    |s| format!("up to {s} Mb/s"),
                );
            // `1-0:1.0` is the root hub's own interface, not a device hanging
            // off it.
            let interfaces = names
                .iter()
                .filter(|n| bus_of(n) == bus && n.contains(':') && !n.contains("-0:"))
                .count();

            Some(Row::field(
                name.as_str(),
                format!("{speed}, {interfaces} interface(s)"),
            ))
        })
        .collect()
}

/// The wireless section, or `None` on a machine with no radio.
///
/// `iw` knows far more about the link than the sparse sysfs wireless
/// directory, so it is asked first; sysfs is the fallback.
pub(crate) fn wireless(host: &Host) -> Option<Vec<Row>> {
    let mut rows = Vec::new();

    for iface in host.list_dir("/sys/class/net") {
        if !iface.join("wireless").exists() && !iface.join("phy80211").exists() {
            continue;
        }
        let name = file_name(&iface);

        rows.push(Row::header(name.as_str()));
        rows.push(Row::field(
            "Driver",
            driver_name(iface.join("device/driver")).unwrap_or_else(|| "-".into()),
        ));
        rows.push(Row::identifier(
            "MAC",
            read(iface.join("address")).unwrap_or_else(|| "-".into()),
        ));
        if let Some(state) = read(iface.join("operstate")) {
            rows.push(Row::field("State", state));
        }

        match host
            .run("iw", &["dev", &name, "link"])
            .and_then(|out| parse_iw_link(&out))
        {
            Some(fields) => rows.extend(fields.into_iter().map(LinkField::into_row)),
            None => rows.extend(sysfs_link_rows(&iface)),
        }

        for counter in ["rx_bytes", "tx_bytes"] {
            if let Some(v) = read_u64(iface.join("statistics").join(counter)) {
                rows.push(Row::field(format!("  {counter}"), human_bytes(v)));
            }
        }
    }

    (!rows.is_empty()).then_some(rows)
}

fn sysfs_link_rows(iface: &Path) -> Vec<Row> {
    let Some(dir) = list_dir(iface.join("wireless")).into_iter().next() else {
        return Vec::new();
    };

    [("SSID", "ssid"), ("Band", "band"), ("Channel", "channel")]
        .into_iter()
        .filter_map(|(label, file)| {
            let value = read(dir.join(file)).filter(|v| v != "0")?;

            Some(if file == "ssid" {
                Row::identifier(label, value)
            } else {
                Row::field(label, value)
            })
        })
        .collect()
}

/// One labelled line of `iw dev <iface> link`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkField {
    label: String,
    value: String,
}

impl LinkField {
    /// The network name and the access point's address say where the
    /// machine is; the plain report treats them as identifiers.
    fn into_row(self) -> Row {
        if self.label == "SSID" || self.label == "Access point" {
            Row::identifier(self.label, self.value)
        } else {
            Row::field(self.label, self.value)
        }
    }
}

fn parse_iw_link(text: &str) -> Option<Vec<LinkField>> {
    if !text.contains("Connected to") {
        return None;
    }

    let fields: Vec<LinkField> = text.lines().filter_map(parse_iw_line).collect();

    (!fields.is_empty()).then_some(fields)
}

fn parse_iw_line(line: &str) -> Option<LinkField> {
    let line = line.trim();
    // "Connected to aa:bb:cc:dd:ee:ff (on wlan0)" must be split on the
    // known prefix, not on the first colon: splitting there put "aa" into
    // the key and the rest of the MAC into the value, so the BSSID was
    // mangled and the label came out as "Connected to aa".
    if let Some(rest) = line.strip_prefix("Connected to ") {
        // The BSSID is followed by the interface name in parentheses.
        let bssid = rest.split_whitespace().next()?;

        return Some(LinkField {
            label: "Access point".into(),
            value: bssid.into(),
        });
    }

    let (key, value) = line.split_once(':')?;
    let (key, value) = (key.trim(), value.trim());
    if key.is_empty() || value.is_empty() {
        return None;
    }

    // An unparseable frequency is shown as-is: using `?` here once returned
    // from the whole parse and threw away every other field.
    let value = match (key, value.parse::<f64>()) {
        ("freq", Ok(mhz)) => {
            // `iw` reports MHz; a value below 100 is a GHz figure from
            // another tool and is scaled up. The old check was "over 1000
            // means GHz", which reported 5180 MHz as "5.18 GHz".
            let mhz = if mhz < 100.0 { mhz * 1000.0 } else { mhz };
            format!("{mhz:.0} MHz (ch {})", mhz_to_channel(mhz))
        }
        _ => value.to_string(),
    };

    Some(LinkField {
        label: capitalise(key),
        value,
    })
}

/// The 802.11 channel number for a frequency in MHz, or 0 for none.
///
/// The three bands are separate numberings, not one line: 2.4 GHz starts at
/// channel 1 on 2412 MHz, 5 GHz is `5000 + 5 * channel` for channels 36 to 177,
/// and 6 GHz restarts at channel 1 on 5955 MHz. Frequencies that belong to no
/// band (the gap around 5900 MHz) report 0 rather than a made-up channel.
fn mhz_to_channel(mhz: f64) -> u64 {
    // 2.4 GHz: channel 13 is 2472 MHz and channel 14 jumps 12 MHz to 2484,
    // so it does not sit on the 5 MHz grid.
    if (2412.0..=2484.0).contains(&mhz) {
        if mhz >= 2484.0 {
            14
        } else {
            round_u64((mhz - 2412.0) / 5.0) + 1
        }
    } else if (5180.0..=5885.0).contains(&mhz) {
        round_u64((mhz - 5000.0) / 5.0)
    } else if (5955.0..=7115.0).contains(&mhz) {
        round_u64((mhz - 5955.0) / 5.0) + 1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::Sensitivity;
    use crate::collect::fixture::Fixture;

    fn get<'a>(fields: &'a [LinkField], label: &str) -> Option<&'a str> {
        fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.value.as_str())
    }

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
        assert_eq!(mhz_to_channel(f64::NAN), 0);
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
        let fields = parse_iw_link(IW_LINK).expect("parsed");
        assert_eq!(get(&fields, "Access point"), Some("aa:bb:cc:dd:ee:ff"));
    }

    #[test]
    fn iw_link_labels_the_remaining_fields() {
        let fields = parse_iw_link(IW_LINK).expect("parsed");
        assert_eq!(
            get(&fields, "SSID"),
            Some("MyNet"),
            "the label keeps its own capitalisation"
        );
        assert_eq!(get(&fields, "Freq"), Some("5180 MHz (ch 36)"));
        assert_eq!(get(&fields, "Signal"), Some("-42 dBm"));
        assert_eq!(get(&fields, "Rx bitrate"), Some("1444002 BPS"));
    }

    #[test]
    fn iw_link_keeps_every_field_when_one_of_them_is_unparseable() {
        // The bug: `value.parse().ok()?` inside the loop returned from the
        // whole function, so one bad freq lost the entire link report.
        let text = "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\n\
                    SSID: MyNet\n\
                    freq: not-a-number\n\
                    signal: -42 dBm\n";
        let fields = parse_iw_link(text).expect("still parsed");
        assert_eq!(
            get(&fields, "SSID"),
            Some("MyNet"),
            "other fields must survive"
        );
        assert_eq!(get(&fields, "Signal"), Some("-42 dBm"));
        assert_eq!(
            get(&fields, "Freq"),
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
            let fields = parse_iw_link(&format!(
                "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\nfreq: {freq}\n"
            ))
            .expect("parsed");
            assert_eq!(get(&fields, "Freq"), Some(want));
        }
    }

    #[test]
    fn iw_link_scales_a_ghz_figure_up_to_mhz() {
        let fields = parse_iw_link("Connected to aa:bb:cc:dd:ee:ff (on wlan0)\nfreq: 5.18\n")
            .expect("parsed");
        assert_eq!(get(&fields, "Freq"), Some("5180 MHz (ch 36)"));
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
        let fields = parse_iw_link(text).expect("parsed");
        assert_eq!(fields.len(), 2, "{fields:?}");
        assert_eq!(get(&fields, "SSID"), Some("ok"));
    }

    #[test]
    fn the_network_name_and_access_point_are_identifiers() {
        let fields = parse_iw_link(IW_LINK).expect("parsed");
        let identifiers: Vec<String> = fields
            .into_iter()
            .map(LinkField::into_row)
            .filter_map(|r| match r {
                Row::Field {
                    label,
                    sensitivity: Sensitivity::Identifier,
                    ..
                } => Some(label.to_string()),
                _ => None,
            })
            .collect();

        assert_eq!(identifiers, ["Access point", "SSID"]);
    }

    // ---- rows against a fixture --------------------------------------------

    #[test]
    fn devices_hubs_and_interfaces_are_listed() {
        let fx = Fixture::new();
        let usb = "sys/bus/usb/devices";
        fx.write(&format!("{usb}/1-10/product"), "Keyboard\n");
        fx.write(&format!("{usb}/1-10/manufacturer"), "Keychron\n");
        fx.write(&format!("{usb}/1-10/speed"), "12\n");
        fx.write(&format!("{usb}/1-2/maxchild"), "4\n");
        fx.write(&format!("{usb}/usb1/speed"), "480\n");
        fx.mkdir(&format!("{usb}/1-10:1.0"));
        fx.mkdir(&format!("{usb}/1-0:1.0"));
        let rows = rows(&fx.host());

        assert_eq!(
            rows,
            [
                Row::header("USB devices"),
                Row::field("1-2", "USB hub · 4 port(s)  ·  speed unknown  -"),
                Row::field("1-10", "Keychron Keyboard  ·  12 Mb/s  -"),
                Row::header("Root hubs"),
                Row::field("usb1", "up to 480 Mb/s, 1 interface(s)"),
            ]
        );
    }

    #[test]
    fn a_machine_without_usb_says_so() {
        assert_eq!(rows(&Fixture::new().host()), [Row::note("no usb devices")]);
    }

    #[test]
    fn wireless_is_none_without_a_radio_and_uses_sysfs_without_iw() {
        let fx = Fixture::new();
        fx.mkdir("sys/class/net/eth0");
        assert_eq!(wireless(&fx.host()), None);

        fx.write("sys/class/net/wlan0/address", "aa:bb:cc:dd:ee:ff\n");
        fx.write("sys/class/net/wlan0/operstate", "up\n");
        fx.write("sys/class/net/wlan0/wireless/link/ssid", "Home\n");
        fx.write("sys/class/net/wlan0/statistics/rx_bytes", "2048\n");
        let rows = wireless(&fx.host()).expect("a radio");

        assert!(rows.contains(&Row::identifier("MAC", "aa:bb:cc:dd:ee:ff")));
        assert!(rows.contains(&Row::identifier("SSID", "Home")));
        assert!(rows.contains(&Row::field("  rx_bytes", "2.0 KiB")));
    }
}
