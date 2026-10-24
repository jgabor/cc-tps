//! The three rates, computed over responses that ended inside the window.

use crate::parse::{Response, Tokens};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Rate {
    /// Tokens divided by the window length.
    Tp,
    /// Tokens divided by the summed duration of each response.
    Resp,
    /// Tokens divided by the wall time when any response was in flight.
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Kind {
    /// Output tokens, thinking included.
    Out,
    /// Input tokens: uncached only in labeled, all input in compact.
    In,
    /// Cache read and write tokens in labeled; the cache hit share in compact.
    Cache,
}

impl Kind {
    fn of(self, t: &Tokens) -> u64 {
        match self {
            Kind::Out => t.output,
            Kind::In => t.input,
            Kind::Cache => t.cache_read + t.cache_write,
        }
    }
}

/// Window totals for the compact layout.
#[derive(Debug, PartialEq)]
pub struct Totals {
    /// All input tokens: uncached, cache reads and cache writes.
    pub input: u64,
    pub output: u64,
    /// Percentage of input tokens read from the cache; `None` without input.
    pub cache_hit: Option<f64>,
}

pub fn totals(resps: &[Response]) -> Totals {
    let sum = |f: fn(&Tokens) -> u64| resps.iter().map(|r| f(&r.tokens)).sum::<u64>();
    let read = sum(|t| t.cache_read);
    let input = sum(|t| t.input) + read + sum(|t| t.cache_write);
    Totals {
        input,
        output: sum(|t| t.output),
        cache_hit: (input > 0).then(|| read as f64 * 100.0 / input as f64),
    }
}

fn total(resps: &[Response], kind: Kind) -> u64 {
    resps.iter().map(|r| kind.of(&r.tokens)).sum()
}

/// Tokens per second, or `None` when the rate has no time base.
pub fn rate(resps: &[Response], rate: Rate, kind: Kind, window_ms: i64) -> Option<f64> {
    match rate {
        Rate::Tp => Some(per_sec(total(resps, kind), window_ms)),
        Rate::Resp => {
            let (total, ms) = timed(resps)
                .map(|(s, r)| (kind.of(&r.tokens), r.end - s))
                .fold((0, 0), |(t, d), (a, b)| (t + a, d + b));
            (ms > 0).then(|| per_sec(total, ms))
        }
        Rate::Active => {
            let total: u64 = timed(resps).map(|(_, r)| kind.of(&r.tokens)).sum();
            let ms = union_len(timed(resps).map(|(s, r)| (s, r.end)).collect());
            (ms > 0).then(|| per_sec(total, ms))
        }
    }
}

fn timed(resps: &[Response]) -> impl Iterator<Item = (i64, &Response)> {
    resps.iter().filter_map(|r| Some((r.start?, r)))
}

fn per_sec(tokens: u64, ms: i64) -> f64 {
    tokens as f64 * 1000.0 / ms as f64
}

/// Total length covered by a set of intervals, overlaps counted once.
fn union_len(mut iv: Vec<(i64, i64)>) -> i64 {
    iv.sort_unstable();
    let mut total = 0;
    let mut cur: Option<(i64, i64)> = None;
    for (s, e) in iv {
        cur = match cur {
            Some((cs, ce)) if s <= ce => Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                total += ce - cs;
                Some((s, e))
            }
            None => Some((s, e)),
        };
    }
    total + cur.map_or(0, |(s, e)| e - s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(start: Option<i64>, end: i64, output: u64) -> Response {
        Response {
            start,
            end,
            tokens: Tokens {
                output,
                input: 1,
                cache_read: 990,
                cache_write: 10,
            },
        }
    }

    #[test]
    fn union_merges_overlaps_and_keeps_gaps() {
        assert_eq!(union_len(vec![]), 0);
        assert_eq!(union_len(vec![(0, 10), (5, 20), (30, 40), (40, 45)]), 35);
        assert_eq!(union_len(vec![(30, 40), (0, 10)]), 20);
    }

    #[test]
    fn three_rates() {
        // Two overlapping 10 s responses and one without a start.
        let resps = [
            r(Some(0), 10_000, 1000),
            r(Some(5_000), 15_000, 500),
            r(None, 20_000, 1500),
        ];
        let w = 300_000;
        assert_eq!(rate(&resps, Rate::Tp, Kind::Out, w), Some(10.0));
        assert_eq!(rate(&resps, Rate::Resp, Kind::Out, w), Some(75.0));
        assert_eq!(rate(&resps, Rate::Active, Kind::Out, w), Some(100.0));
        assert_eq!(rate(&resps, Rate::Tp, Kind::Cache, w), Some(10.0));
        assert_eq!(rate(&resps, Rate::Resp, Kind::In, w), Some(0.1));
        // Input: 3 uncached + 2970 read + 30 written.
        assert_eq!(
            totals(&resps),
            Totals {
                input: 3003,
                output: 3000,
                cache_hit: Some(2970.0 * 100.0 / 3003.0)
            }
        );
        assert_eq!(totals(&[]).cache_hit, None);
    }

    #[test]
    fn no_time_base_gives_none() {
        let w = 300_000;
        assert_eq!(rate(&[], Rate::Tp, Kind::Out, w), Some(0.0));
        assert_eq!(rate(&[], Rate::Resp, Kind::Out, w), None);
        assert_eq!(rate(&[], Rate::Active, Kind::Out, w), None);
        let untimed = [r(None, 1, 5)];
        assert_eq!(rate(&untimed, Rate::Resp, Kind::Out, w), None);
    }
}
