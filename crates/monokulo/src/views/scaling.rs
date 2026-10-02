//! How the engine is performing and scaling, as people read it
//! (docs/engine_scaling.md section 6): the figures' formatting here, and
//! the admin page's performance panels.

/// `bytes` in decimal units, as sizes on a network are given: "412 MB",
/// "1.8 MB", "13 kB".
pub fn bytes(bytes: u64) -> String {
    let b = bytes as f64;
    let (value, unit) = if b >= 1e9 {
        (b / 1e9, "GB")
    } else if b >= 1e6 {
        (b / 1e6, "MB")
    } else if b >= 1e3 {
        (b / 1e3, "kB")
    } else {
        return format!("{bytes} B");
    };
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

/// A link's rate, from bytes a second: "3.1 Mbit/s", "480 kbit/s".
pub fn rate(bytes_per_sec: u64) -> String {
    let bits = bytes_per_sec as f64 * 8.0;
    if bits >= 1e9 {
        format!("{:.1} Gbit/s", bits / 1e9)
    } else if bits >= 1e6 {
        format!("{:.1} Mbit/s", bits / 1e6)
    } else {
        format!("{:.0} kbit/s", bits / 1e3)
    }
}

/// An elapsed time to the second: "2 m 10 s", "45 s", "1 h 5 m".
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h} h {m} m")
    } else if m > 0 {
        format!("{m} m {s} s")
    } else {
        format!("{s} s")
    }
}

/// A rough length of time: "18 minutes", "40 seconds", "2 hours".
pub fn duration_rough(secs: i64) -> String {
    let secs = secs.max(0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    if secs >= 2 * 3600 {
        plural((secs as f64 / 3600.0).round() as i64, "hour")
    } else if secs >= 90 {
        plural((secs as f64 / 60.0).round() as i64, "minute")
    } else {
        plural(secs, "second")
    }
}

/// `n` with thousands separators: "3,412,001".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_read_as_people_write_them() {
        assert_eq!(bytes(412_000_000), "412 MB");
        assert_eq!(bytes(1_800_000), "1.8 MB");
        assert_eq!(bytes(13_000), "13.0 kB");
        assert_eq!(bytes(900), "900 B");
        assert_eq!(bytes(7_600_000_000), "7.6 GB");
        assert_eq!(rate(387_500), "3.1 Mbit/s");
        assert_eq!(rate(60_000), "480 kbit/s");
        assert_eq!(duration(130), "2 m 10 s");
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(3_900), "1 h 5 m");
        assert_eq!(duration_rough(1_080), "18 minutes");
        assert_eq!(duration_rough(40), "40 seconds");
        assert_eq!(duration_rough(60), "60 seconds");
        assert_eq!(duration_rough(7_200), "2 hours");
        assert_eq!(thousands(3_412_001), "3,412,001");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
    }
}
