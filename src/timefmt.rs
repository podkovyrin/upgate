pub(crate) fn human_age(total_secs: u64) -> String {
    if total_secs < 60 {
        return format!("{total_secs}s");
    }

    if total_secs < 60 * 60 {
        return format!("{}m", total_secs / 60);
    }

    if total_secs < 24 * 60 * 60 {
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{minutes}m")
        };
    }

    let days = total_secs / (24 * 60 * 60);
    let hours = (total_secs % (24 * 60 * 60)) / 3600;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{hours}h")
    }
}

#[cfg(test)]
mod tests {
    use super::human_age;

    #[test]
    fn formats_short_and_long_durations() {
        assert_eq!(human_age(59), "59s");
        assert_eq!(human_age(61), "1m");
        assert_eq!(human_age(3600), "1h");
        assert_eq!(human_age(3660), "1h1m");
        assert_eq!(human_age(24 * 3600), "1d");
        assert_eq!(human_age(25 * 3600), "1d1h");
    }
}
