//! How the engine is performing and scaling, as people read it:
//! the figures' formatting, and the
//! status page's sentences about a slow block and header-first reading.
//! The engine page's "Machine and links" strip and Scanning panel are its
//! app's (`apps/engine/src/machine.ts`).

use engine_protocol::scaling::{HeadersFirstReason, SlowBlock};

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

/// A network's name as a word, for a title or the start of a sentence:
/// "Mainnet". The status page's network cards and the network badge use it.
pub fn network_name(network: &str) -> String {
    let mut name = network.to_string();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}

/// One sentence about a slow block: what is slow, at what rate, how much
/// longer it needs and what would help.
/// The node only when `show_node` (an operator is looking).
pub fn slow_block_message(network: &str, slow: &SlowBlock, show_node: bool) -> String {
    let size = slow
        .wire_bytes
        .map(|wire| format!(" ({})", bytes(wire)))
        .unwrap_or_default();
    let mut message = format!(
        "{}: block {}{size} has taken {} so far",
        network_name(network),
        thousands(slow.height),
        duration(slow.elapsed_secs),
    );
    match (show_node, &slow.node, slow.rate_bytes_per_sec) {
        (true, Some(node), Some(speed)) => {
            message.push_str(&format!(", at {} from {node}", rate(speed)));
        }
        (_, _, Some(speed)) => message.push_str(&format!(", at {}", rate(speed))),
        _ => {}
    }
    message.push('.');
    if let Some(remaining) = slow.remaining_secs.filter(|secs| *secs > 0) {
        message.push_str(&format!(
            " At this rate it needs about {} more.",
            duration_rough(remaining)
        ));
    }
    message.push_str(" A faster node or a larger scan memory budget would help.");
    message
}

/// Why headers come first and for how long: "for about 50 minutes more: a
/// block request ran out of time or came back too large, so each block's
/// size is checked before it is fetched".
fn headers_first_why(on: &engine_protocol::scaling::HeadersFirst) -> String {
    format!(
        "for about {} more: {}, so each block's size is checked before it is fetched",
        duration_rough(on.remaining_secs),
        match on.reason {
            HeadersFirstReason::FailedRequest => {
                "a block request ran out of time or came back too large"
            }
            HeadersFirstReason::LargeBlock => {
                "a recent block came near the size that is scanned in pages"
            }
        }
    )
}

/// The status page's sentence while a network's blocks are read
/// header-first.
pub fn headers_first_sentence(on: &engine_protocol::scaling::HeadersFirst) -> String {
    format!("Reading block headers first {}.", headers_first_why(on))
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
