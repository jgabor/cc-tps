//! Turns transcript lines into responses: one entry per API response, with
//! its token usage and the interval it took.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::value::RawValue;

/// Gap after which a start marker is too old to belong to a response.
const MAX_START_GAP_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub output: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    fn is_zero(&self) -> bool {
        self.output == 0 && self.input == 0 && self.cache_read == 0 && self.cache_write == 0
    }
}

/// One API response. `start` is the request time when it could be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub start: Option<i64>,
    pub end: i64,
    pub tokens: Tokens,
}

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a str>,
    #[serde(borrow)]
    timestamp: Option<&'a str>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    id: Option<&'a str>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    output_tokens: u64,
    input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

/// Parses an RFC 3339 timestamp into Unix milliseconds.
pub fn parse_ts(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

struct Partial {
    first: i64,
    last: i64,
    tokens: Tokens,
}

/// Collects the responses in one file's complete lines, keyed by message id.
/// Unparseable lines are skipped.
pub fn parse_file(data: &[u8]) -> HashMap<String, Response> {
    let mut starts = Vec::new();
    let mut partials: HashMap<String, Partial> = HashMap::new();
    for line in data.split(|&b| b == b'\n') {
        let Ok(rec) = serde_json::from_slice::<Record>(line) else {
            continue;
        };
        let Some(ts) = rec.timestamp.and_then(parse_ts) else {
            continue;
        };
        match rec.kind {
            Some("user" | "api-request") => starts.push(ts),
            Some("assistant") => {
                let Some(msg) = rec
                    .message
                    .and_then(|m| serde_json::from_str::<Message>(m.get()).ok())
                else {
                    continue;
                };
                let (Some(id), Some(u)) = (msg.id, msg.usage) else {
                    continue;
                };
                let tokens = Tokens {
                    output: u.output_tokens,
                    input: u.input_tokens,
                    cache_read: u.cache_read_input_tokens,
                    cache_write: u.cache_creation_input_tokens,
                };
                if tokens.is_zero() {
                    continue;
                }
                let p = partials.entry(id.to_owned()).or_insert(Partial {
                    first: ts,
                    last: ts,
                    tokens,
                });
                p.first = p.first.min(ts);
                p.last = p.last.max(ts);
            }
            _ => {}
        }
    }
    starts.sort_unstable();
    partials
        .into_iter()
        .map(|(id, p)| {
            let resp = Response {
                start: find_start(&starts, p.first),
                end: p.last,
                tokens: p.tokens,
            };
            (id, resp)
        })
        .collect()
}

/// Finds the latest start marker at or before `first`, if it is recent enough.
fn find_start(sorted: &[i64], first: i64) -> Option<i64> {
    let i = sorted.partition_point(|&t| t <= first);
    let t = *sorted.get(i.checked_sub(1)?)?;
    (first - t <= MAX_START_GAP_MS).then_some(t)
}

/// Merges one file's responses into the global map. The same message id can
/// appear in several files (resumed sessions copy history), so it is counted
/// once, with the widest interval seen.
pub fn merge(into: &mut HashMap<String, Response>, from: HashMap<String, Response>) {
    for (id, r) in from {
        into.entry(id)
            .and_modify(|e| {
                e.start = match (e.start, r.start) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                e.end = e.end.max(r.end);
            })
            .or_insert(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asst(id: &str, ts: &str, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"id":"{id}","content":[],"usage":{{"input_tokens":2,"output_tokens":{out},"cache_read_input_tokens":100,"cache_creation_input_tokens":10}}}}}}"#
        )
    }

    fn rec(kind: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"{kind}","timestamp":"{ts}","message":{{"role":"user","content":"hi"}}}}"#
        )
    }

    fn ms(s: &str) -> i64 {
        parse_ts(s).unwrap()
    }

    #[test]
    fn groups_blocks_of_one_message() {
        let data = [
            rec("user", "2026-01-01T00:00:00Z"),
            asst("m1", "2026-01-01T00:00:03Z", 50),
            asst("m1", "2026-01-01T00:00:05Z", 50),
        ]
        .join("\n");
        let got = parse_file(data.as_bytes());
        assert_eq!(got.len(), 1);
        let r = &got["m1"];
        assert_eq!(r.start, Some(ms("2026-01-01T00:00:00Z")));
        assert_eq!(r.end, ms("2026-01-01T00:00:05Z"));
        assert_eq!(
            r.tokens,
            Tokens {
                output: 50,
                input: 2,
                cache_read: 100,
                cache_write: 10,
            }
        );
    }

    #[test]
    fn start_marker_lookup_tolerates_out_of_order_lines() {
        // A later api-request is appended before the response it does not belong to.
        let data = [
            rec("api-request", "2026-01-01T00:00:00Z"),
            rec("api-request", "2026-01-01T00:00:20Z"),
            asst("m1", "2026-01-01T00:00:08Z", 10),
        ]
        .join("\n");
        let got = parse_file(data.as_bytes());
        assert_eq!(got["m1"].start, Some(ms("2026-01-01T00:00:00Z")));
    }

    #[test]
    fn old_or_missing_start_marker_gives_no_start() {
        let data = [
            rec("user", "2026-01-01T00:00:00Z"),
            asst("m1", "2026-01-01T00:10:01Z", 10),
        ]
        .join("\n");
        assert_eq!(parse_file(data.as_bytes())["m1"].start, None);
        let lone = asst("m2", "2026-01-01T00:00:00Z", 10);
        assert_eq!(parse_file(lone.as_bytes())["m2"].start, None);
    }

    #[test]
    fn skips_zero_usage_and_garbage() {
        let data = [
            "not json".to_owned(),
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","message":{"id":"s","usage":{"input_tokens":0,"output_tokens":0}}}"#.to_owned(),
            r#"{"type":"system","timestamp":"2026-01-01T00:00:00Z","message":"a string"}"#.to_owned(),
            r#"{"type":"assistant","timestamp":"bad","message":{"id":"x","usage":{"output_tokens":5}}}"#.to_owned(),
        ]
        .join("\n");
        assert!(parse_file(data.as_bytes()).is_empty());
    }

    #[test]
    fn merge_counts_a_message_once_across_files() {
        let a = parse_file(asst("m1", "2026-01-01T00:00:05Z", 10).as_bytes());
        let b = parse_file(
            [
                rec("user", "2026-01-01T00:00:00Z"),
                asst("m1", "2026-01-01T00:00:06Z", 10),
            ]
            .join("\n")
            .as_bytes(),
        );
        let mut all = HashMap::new();
        merge(&mut all, a);
        merge(&mut all, b);
        assert_eq!(all.len(), 1);
        assert_eq!(all["m1"].start, Some(ms("2026-01-01T00:00:00Z")));
        assert_eq!(all["m1"].end, ms("2026-01-01T00:00:06Z"));
    }
}
