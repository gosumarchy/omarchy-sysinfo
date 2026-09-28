//! Turning raw counters into the strings a person reads.
//!
//! Every collector pulls numbers out of `/proc` and `/sys` in whatever unit the
//! kernel happened to choose — bytes, micro-watt-hours, millidegrees, jiffies.
//! The helpers here are the one place that knows how those map onto "1.5 GiB"
//! and "12.3 W", so the arithmetic is not repeated in twelve collectors.

/// A counter as a float, for display and ratios only.
///
/// Above 2^53 the result is rounded, which no reader of "1.5 TiB" will ever
/// notice; the one place precision matters, parsing, never goes through here.
#[expect(
    clippy::cast_precision_loss,
    reason = "display-only conversion; see the doc comment"
)]
pub(crate) fn approx_f64(value: u64) -> f64 {
    value as f64
}

/// `part / whole`, or zero when there is no whole to divide by.
pub(crate) fn fraction(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        approx_f64(part) / approx_f64(whole)
    }
}

/// A non-negative float rounded to the nearest whole number, saturating at
/// the ends of `u64` (NaN counts as zero).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into u64's range first"
)]
pub(crate) fn round_u64(value: f64) -> u64 {
    if value.is_nan() || value <= 0.0 {
        0
    } else if value >= approx_f64(u64::MAX) {
        u64::MAX
    } else {
        value.round() as u64
    }
}

/// Bytes as a binary-prefixed size, e.g. `1.5 KiB`.
///
/// The unit table stops at TiB, so a value beyond that keeps counting up in TiB
/// rather than wrapping or looping forever.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = approx_f64(bytes);
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    format!("{value:.1} {}", UNITS[unit])
}

/// A clock speed, switching to GHz at 1000 MHz.
pub(crate) fn human_mhz(mhz: u64) -> String {
    if mhz >= 1000 {
        format!("{:.2} GHz", approx_f64(mhz) / 1000.0)
    } else {
        format!("{mhz} MHz")
    }
}

/// Battery energy, which the kernel reports in micro-watt-hours rather than bytes.
pub(crate) fn human_energy(uwh: u64) -> String {
    let wh = approx_f64(uwh) / 1_000_000.0;

    if wh >= 1000.0 {
        format!("{:.2} kWh", wh / 1000.0)
    } else {
        format!("{wh:.2} Wh")
    }
}

/// Power draw in watts, promoting to kilowatts past 1000 W.
pub(crate) fn human_watts(uw: u64) -> String {
    let w = approx_f64(uw) / 1_000_000.0;

    if w >= 1000.0 {
        format!("{:.2} kW", w / 1000.0)
    } else {
        format!("{w:.1} W")
    }
}

/// A duration as its three largest units, starting from the largest that is
/// non-zero: `2d 3h 4m`, `3h 4m 5s`, `4m 5s`.
pub(crate) fn human_secs(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;

    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else {
        format!("{m}m {s}s")
    }
}

/// Seconds since the epoch as a UTC timestamp, `2026-09-28 14:05 UTC`.
///
/// The local zone would need a tzdata parser; UTC is unambiguous and the
/// report shows the zone name next to it anyway.
pub(crate) fn utc_timestamp(epoch: u64) -> String {
    let days = epoch / 86_400;
    let secs = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3_600,
        (secs % 3_600) / 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
///
/// Howard Hinnant's `civil_from_days`, restricted to non-negative day counts
/// since nothing here predates the epoch.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);

    (year, month, day)
}

/// `wifi` → `Wifi`: the first character uppercased, the rest untouched.
pub(crate) fn capitalise(s: &str) -> String {
    let mut chars = s.chars();

    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Stand in for a value the machine did not report, so a row keeps its shape and
/// the reader can see that something was looked for and not found.
pub(crate) fn dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these tests pin exact, exactly representable results"
)]
mod tests {
    use super::*;

    // ---- human_bytes -----------------------------------------------------

    #[test]
    fn human_bytes_small_values_stay_in_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1), "1 B");
        assert_eq!(human_bytes(1023), "1023 B");
    }

    #[test]
    fn human_bytes_promotes_exactly_at_1024() {
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1025), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(human_bytes(1024u64.pow(4)), "1.0 TiB");
    }

    #[test]
    fn human_bytes_saturates_at_tebibytes_without_overflowing() {
        // The unit table stops at TiB, so huge values keep counting up in TiB
        // rather than wrapping or panicking in the promotion loop.
        assert_eq!(human_bytes(u64::MAX), "16777216.0 TiB");
        let huge = human_bytes(u64::MAX);
        assert!(huge.ends_with(" TiB"), "expected TiB suffix, got {huge}");
    }

    #[test]
    fn human_bytes_rounds_to_one_decimal() {
        // 1536 bytes is exactly 1.5 KiB.
        assert_eq!(human_bytes(1536), "1.5 KiB");
        // 1610.5 -> rounds to 1.6
        assert_eq!(human_bytes(1644), "1.6 KiB");
    }

    // ---- human_mhz -------------------------------------------------------

    #[test]
    fn human_mhz_switches_at_1000() {
        assert_eq!(human_mhz(0), "0 MHz");
        assert_eq!(human_mhz(999), "999 MHz");
        assert_eq!(human_mhz(1000), "1.00 GHz");
        assert_eq!(human_mhz(3600), "3.60 GHz");
        assert_eq!(human_mhz(800), "800 MHz");
    }

    #[test]
    fn human_mhz_rounds_ghz_to_two_decimals() {
        assert_eq!(human_mhz(1234), "1.23 GHz");
        assert_eq!(human_mhz(3499), "3.50 GHz");
    }

    // ---- human_energy / human_watts -------------------------------------

    #[test]
    fn human_energy_converts_microwatt_hours_to_watt_hours() {
        assert_eq!(human_energy(0), "0.00 Wh");
        // 1 Wh == 1_000_000 uWh
        assert_eq!(human_energy(1_000_000), "1.00 Wh");
        assert_eq!(human_energy(5_000_000), "5.00 Wh");
    }

    #[test]
    fn human_energy_promotes_to_kwh_past_1000_wh() {
        assert_eq!(human_energy(999_999_999), "1000.00 Wh");
        assert_eq!(human_energy(1_000_000_000), "1.00 kWh");
        assert_eq!(human_energy(2_500_000_000), "2.50 kWh");
    }

    #[test]
    fn human_watts_converts_microwatts_to_watts() {
        assert_eq!(human_watts(0), "0.0 W");
        assert_eq!(human_watts(1_000_000), "1.0 W");
        assert_eq!(human_watts(12_340_000), "12.3 W");
    }

    #[test]
    fn human_watts_promotes_to_kw_past_1000_w() {
        // Deliberate precision difference: watts need one decimal to be
        // readable, kilowatts carry two so a low draw still shows resolution.
        assert_eq!(human_watts(999_999_999), "1000.0 W");
        assert_eq!(human_watts(1_000_000_000), "1.00 kW");
        assert_eq!(human_watts(2_500_000_000), "2.50 kW");
    }

    // ---- human_secs ------------------------------------------------------

    #[test]
    fn human_secs_drops_seconds_only_when_days_are_shown() {
        assert_eq!(human_secs(0), "0m 0s");
        assert_eq!(human_secs(59), "0m 59s");
        assert_eq!(human_secs(60), "1m 0s");
        assert_eq!(human_secs(3_599), "59m 59s");
    }

    #[test]
    fn human_secs_adds_seconds_when_hours_are_shown() {
        assert_eq!(human_secs(3_600), "1h 0m 0s");
        assert_eq!(human_secs(3_661), "1h 1m 1s");
        assert_eq!(human_secs(86_399), "23h 59m 59s");
    }

    #[test]
    fn human_secs_adds_hours_when_days_are_shown() {
        assert_eq!(human_secs(86_400), "1d 0h 0m");
        assert_eq!(human_secs(90_061), "1d 1h 1m");
        assert_eq!(human_secs(86_400 * 400), "400d 0h 0m");
    }

    // ---- dash ------------------------------------------------------------

    #[test]
    fn dash_replaces_missing_values() {
        assert_eq!(dash(Some("x".into())), "x");
        assert_eq!(dash(None), "-");
    }

    // ---- fraction / round_u64 ---------------------------------------------

    #[test]
    fn fraction_of_nothing_is_zero_not_nan() {
        assert_eq!(fraction(5, 0), 0.0);
        assert_eq!(fraction(1, 4), 0.25);
    }

    #[test]
    fn round_u64_saturates_and_pins_nan() {
        assert_eq!(round_u64(2.5), 3);
        assert_eq!(round_u64(-1.0), 0);
        assert_eq!(round_u64(f64::NAN), 0);
        assert_eq!(round_u64(f64::INFINITY), u64::MAX);
    }

    // ---- utc_timestamp ---------------------------------------------------

    #[test]
    fn utc_timestamp_formats_known_instants() {
        assert_eq!(utc_timestamp(0), "1970-01-01 00:00 UTC");
        // 2000-02-29 is the leap day a naive year/4 rule gets right and a
        // century rule gets wrong.
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(utc_timestamp(1_790_604_300), "2026-09-28 14:05 UTC");
    }

    // ---- capitalise --------------------------------------------------------

    #[test]
    fn capitalise_uppercases_only_the_first_character() {
        assert_eq!(capitalise("wifi"), "Wifi");
        assert_eq!(capitalise("systemd-timesyncd"), "Systemd-timesyncd");
        assert_eq!(capitalise("rx bitrate"), "Rx bitrate");
        assert_eq!(capitalise("Bluetooth"), "Bluetooth");
    }

    #[test]
    fn capitalise_copes_with_empty_and_non_ascii() {
        assert_eq!(capitalise(""), "");
        assert_eq!(capitalise("über"), "Über");
    }
}
