pub(crate) fn human_age(total_secs: u64) -> String {
    let minute = 60;
    let hour = 60 * minute;
    let day = 24 * hour;
    let month = 30 * day;
    let year = 365 * day;

    if total_secs < minute {
        return format!("{total_secs}s");
    }

    if total_secs < hour {
        return format!("{}m", total_secs / minute);
    }

    if total_secs < day {
        let hours = total_secs / hour;
        let minutes = (total_secs % hour) / minute;
        return if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        };
    }

    if total_secs < month {
        let days = total_secs / day;
        let hours = (total_secs % day) / hour;
        return if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        };
    }

    if total_secs < year {
        let months = total_secs / month;
        let days = (total_secs % month) / day;
        return if days > 0 {
            format!("{months}mo {days}d")
        } else {
            format!("{months}mo")
        };
    }

    let years = total_secs / year;
    let months = (total_secs % year) / month;
    if months > 0 {
        format!("{years}y {months}mo")
    } else {
        format!("{years}y")
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
        assert_eq!(human_age(3660), "1h 1m");
        assert_eq!(human_age(24 * 3600), "1d");
        assert_eq!(human_age(25 * 3600), "1d 1h");
        assert_eq!(human_age(40 * 24 * 3600), "1mo 10d");
        assert_eq!(human_age((365 + 30 + 2) * 24 * 3600), "1y 1mo");
        assert_eq!(human_age((2 * 365 + 5) * 24 * 3600), "2y");
    }
}
