//! Display formatting for the raw values sources return.

use std::time::Duration;

use crate::backend::media::Published;

/// Short counts: 999, 1.2K, 12K, 534K, 1.3M, 23M, 1.1B.
pub fn compact_count(n: u64) -> String {
    let (value, suffix) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n as f64 / 1e3, "K"),
        1_000_000..1_000_000_000 => (n as f64 / 1e6, "M"),
        _ => (n as f64 / 1e9, "B"),
    };
    if value < 10.0 {
        let rounded = (value * 10.0).floor() / 10.0;
        let text = format!("{rounded:.1}");
        format!("{}{suffix}", text.trim_end_matches(".0"))
    } else {
        format!("{}{suffix}", value.floor() as u64)
    }
}

/// 10,967,424
pub fn grouped_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 7:44, 1:02:05
pub fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

pub fn published(published: &Published) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match published {
        Published::Date { year, month, day } => {
            let month = MONTHS.get(usize::from(*month).saturating_sub(1)).copied().unwrap_or("");
            format!("{month} {day}, {year}")
        }
        Published::Text(text) => text.clone(),
    }
}

/// "Normal", "1.5×"
pub fn speed(rate: f64) -> String {
    if (rate - 1.0).abs() < 1e-9 { "Normal".to_string() } else { format!("{rate}×") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_times() {
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_250), "1.2K");
        assert_eq!(compact_count(12_900), "12K");
        assert_eq!(compact_count(534_000), "534K");
        assert_eq!(compact_count(1_000_000), "1M");
        assert_eq!(compact_count(23_507_910), "23M");
        assert_eq!(grouped_count(10_967_424), "10,967,424");
        assert_eq!(grouped_count(999), "999");
        assert_eq!(clock(Duration::from_secs(464)), "7:44");
        assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
        assert_eq!(published(&Published::Date { year: 2019, month: 4, day: 4 }), "Apr 4, 2019");
        assert_eq!(speed(1.0), "Normal");
        assert_eq!(speed(1.5), "1.5×");
    }
}
