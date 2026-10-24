//! Renders rates as one labeled status line segment.

use crate::rates::{Kind, Rate, Totals};

/// Formats a rate: one decimal below 10, whole numbers below 1000, then k and M.
pub fn num(x: f64) -> String {
    if x == 0.0 {
        "0".to_owned()
    } else if x < 9.95 {
        format!("{x:.1}")
    } else if x < 999.5 {
        format!("{x:.0}")
    } else if x < 999_950.0 {
        format!("{:.1}k", x / 1e3)
    } else {
        format!("{:.1}M", x / 1e6)
    }
}

/// Short label for a window length, such as `5m`, `1h` or `90s`.
pub fn window_label(secs: u64) -> String {
    match secs {
        s if s > 0 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s > 0 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Out => "out",
        Kind::In => "in",
        Kind::Cache => "cache",
    }
}

fn rate_label(r: Rate) -> &'static str {
    match r {
        Rate::Tp => "",
        Rate::Resp => "resp ",
        Rate::Active => "active ",
    }
}

/// One token kind with its selected rates; `None` means no time base.
pub type Group = (Kind, Vec<(Rate, Option<f64>)>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Format {
    /// Groups per token kind, with rate labels and `·` dividers.
    Labeled,
    /// Space-separated `key:value` pairs: output-token rates, then token
    /// totals for the window.
    Compact,
}

/// Formats a token count: whole numbers below 1000, then k and M.
pub fn count(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        num(n as f64)
    }
}

/// Labeled layout: `5m out 124/s · resp 92/s · active 131/s | cache 41.0k/s`.
pub fn labeled(window_secs: u64, groups: &[Group]) -> String {
    let groups: Vec<String> = groups
        .iter()
        .map(|(kind, vals)| {
            let vals: Vec<String> = vals
                .iter()
                .map(|(rate, v)| match v {
                    Some(v) => format!("{}{}/s", rate_label(*rate), num(*v)),
                    None => format!("{}–", rate_label(*rate)),
                })
                .collect();
            format!("{} {}", kind_label(*kind), vals.join(" · "))
        })
        .collect();
    format!("{} {}", window_label(window_secs), groups.join(" | "))
}

/// Compact layout: `tps:124/s e2e:92/s act:131/s in:855k out:35.0k cache:99%`.
/// Rates are for output tokens; `kinds` picks and orders the window totals.
pub fn compact(rates: &[(Rate, Option<f64>)], kinds: &[Kind], totals: &Totals) -> String {
    let rates = rates.iter().map(|(rate, v)| {
        let key = match rate {
            Rate::Tp => "tps",
            Rate::Resp => "e2e",
            Rate::Active => "act",
        };
        match v {
            Some(v) => format!("{key}:{}/s", num(*v)),
            None => format!("{key}:–"),
        }
    });
    let totals = kinds.iter().map(|&kind| {
        let v = match kind {
            Kind::In => count(totals.input),
            Kind::Out => count(totals.output),
            // Floor, so 100% means nothing missed the cache.
            Kind::Cache => totals
                .cache_hit
                .map_or_else(|| "–".to_owned(), |p| format!("{}%", p.floor())),
        };
        format!("{}:{v}", kind_label(kind))
    });
    rates.chain(totals).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let cases = [
            (0.0, "0"),
            (0.04, "0.0"),
            (3.24, "3.2"),
            (9.96, "10"),
            (123.4, "123"),
            (999.6, "1.0k"),
            (41_234.0, "41.2k"),
            (999_960.0, "1.0M"),
            (3_400_000.0, "3.4M"),
        ];
        for (x, want) in cases {
            assert_eq!(num(x), want, "{x}");
        }
    }

    #[test]
    fn window_labels() {
        assert_eq!(window_label(300), "5m");
        assert_eq!(window_label(3600), "1h");
        assert_eq!(window_label(90), "90s");
    }

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(52), "52");
        assert_eq!(count(999), "999");
        assert_eq!(count(35_012), "35.0k");
        assert_eq!(count(3_200_000), "3.2M");
    }

    #[test]
    fn labeled_line() {
        let groups = vec![
            (
                Kind::Out,
                vec![
                    (Rate::Tp, Some(124.2)),
                    (Rate::Resp, Some(91.7)),
                    (Rate::Active, None),
                ],
            ),
            (Kind::Cache, vec![(Rate::Tp, Some(41_000.0))]),
        ];
        assert_eq!(
            labeled(300, &groups),
            "5m out 124/s · resp 92/s · active – | cache 41.0k/s"
        );
        let no_tp = vec![(Kind::Out, vec![(Rate::Resp, Some(92.0))])];
        assert_eq!(labeled(300, &no_tp), "5m out resp 92/s");
    }

    #[test]
    fn compact_line() {
        let rates = [
            (Rate::Tp, Some(116.2)),
            (Rate::Resp, Some(101.7)),
            (Rate::Active, None),
        ];
        let totals = Totals {
            input: 855_000,
            output: 35_000,
            cache_hit: Some(99.97),
        };
        let all = [Kind::In, Kind::Out, Kind::Cache];
        assert_eq!(
            compact(&rates, &all, &totals),
            "tps:116/s e2e:102/s act:– in:855.0k out:35.0k cache:99%"
        );
        let idle = Totals {
            input: 0,
            output: 0,
            cache_hit: None,
        };
        assert_eq!(
            compact(
                &[(Rate::Active, Some(3.0))],
                &[Kind::Cache, Kind::In],
                &idle
            ),
            "act:3.0/s cache:– in:0"
        );
    }
}
