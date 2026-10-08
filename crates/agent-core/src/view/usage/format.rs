pub fn token_count(value: u64) -> String {
    match value {
        value if value >= 1_000_000_000 => format!("{:.1}B", value as f64 / 1_000_000_000.0),
        value if value >= 1_000_000 => format!("{:.1}M", value as f64 / 1_000_000.0),
        value if value >= 1_000 => format!("{:.1}K", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

pub fn usd(value: f64) -> String {
    if !value.is_finite() {
        return "Unknown".into();
    }
    "$".to_owned() + &format!("{value:.2}")
}

pub fn percent(value: f64) -> String {
    format!("{:.0}%", value.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_large_counts_and_unknown_prices() {
        assert_eq!(token_count(12_400), "12.4K");
        assert_eq!(usd(f64::NAN), "Unknown");
        assert_eq!(percent(125.0), "100%");
    }
}
