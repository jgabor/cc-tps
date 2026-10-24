//! cc-tps: tokens per second across all local Claude Code sessions, as one
//! status line segment.

mod format;
mod parse;
mod rates;
mod scan;

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clap::Parser;

use format::Format;
use rates::{Kind, Rate};

/// Responses can start this long before the window and still end inside it.
const SLACK_MS: i64 = 15 * 60 * 1000;

#[derive(Parser, Debug, Hash)]
#[command(version, about)]
struct Args {
    /// Window to average over, such as 300, 90s, 5m or 1h.
    #[arg(long, default_value = "5m", value_parser = parse_secs)]
    window: u64,

    /// Rates to show, in order.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "tp,resp,active"
    )]
    rates: Vec<Rate>,

    /// Token kinds to show, in order. Labeled shows rates per kind (default
    /// out); compact shows window totals and the cache hit share (default
    /// in,out,cache).
    #[arg(long, value_enum, value_delimiter = ',')]
    tokens: Option<Vec<Kind>>,

    /// Reuse the last result for this long; 0 turns the cache off.
    #[arg(long, default_value = "2s", value_parser = parse_secs)]
    cache_ttl: u64,

    /// Transcript root. Defaults to $CLAUDE_CONFIG_DIR/projects or ~/.claude/projects.
    #[arg(long)]
    projects_dir: Option<PathBuf>,

    /// Output layout.
    #[arg(long, value_enum, default_value = "labeled")]
    format: Format,

    /// Print without ANSI dim styling.
    #[arg(long)]
    no_color: bool,

    /// Pretend the current time is this RFC 3339 timestamp (for tests).
    #[arg(long, hide = true, value_parser = parse_now)]
    now: Option<i64>,
}

fn parse_secs(s: &str) -> Result<u64, String> {
    let (n, mult) = match s.char_indices().last() {
        Some((i, 's')) => (&s[..i], 1),
        Some((i, 'm')) => (&s[..i], 60),
        Some((i, 'h')) => (&s[..i], 3600),
        _ => (s, 1),
    };
    n.parse::<u64>()
        .map(|n| n * mult)
        .map_err(|_| format!("invalid duration {s:?}; use a number with an optional s, m or h"))
}

fn parse_now(s: &str) -> Result<i64, String> {
    parse::parse_ts(s).ok_or_else(|| format!("invalid RFC 3339 timestamp {s:?}"))
}

fn main() {
    let args = Args::parse();
    if args.window == 0 {
        eprintln!("cc-tps: --window must be greater than 0");
        std::process::exit(2);
    }
    // The status line must never break, so failures print nothing on stdout.
    match run(&args) {
        Ok(line) => println!("{line}"),
        Err(e) => eprintln!("cc-tps: {e}"),
    }
}

fn run(args: &Args) -> Result<String, Box<dyn Error>> {
    let root = match &args.projects_dir {
        Some(p) => p.clone(),
        None => default_root()?,
    };
    let cache = (args.cache_ttl > 0 && args.now.is_none()).then(|| cache_path(args, &root));
    if let Some(line) = cache.as_deref().and_then(|p| read_cache(p, args.cache_ttl)) {
        return Ok(line);
    }

    let now = args
        .now
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond());
    let window_ms = args.window as i64 * 1000;
    let from = now - window_ms;
    let cutoff = from - SLACK_MS;
    let since = SystemTime::UNIX_EPOCH + Duration::from_millis(cutoff.max(0) as u64);

    let mut all = HashMap::new();
    for path in scan::discover(&root, since)? {
        // A transcript can be rotated or deleted between listing and reading.
        let Ok(data) = scan::read_tail(&path, cutoff) else {
            continue;
        };
        parse::merge(&mut all, parse::parse_file(&data));
    }
    let resps: Vec<_> = all
        .into_values()
        .filter(|r| r.end > from && r.end <= now)
        .collect();

    let rates_for = |k: Kind| -> Vec<_> {
        args.rates
            .iter()
            .map(|&r| (r, rates::rate(&resps, r, k, window_ms)))
            .collect()
    };
    let mut line = match args.format {
        Format::Labeled => {
            let kinds = args.tokens.as_deref().unwrap_or(&[Kind::Out]);
            let groups: Vec<_> = kinds.iter().map(|&k| (k, rates_for(k))).collect();
            format::labeled(args.window, &groups)
        }
        Format::Compact => {
            let kinds = args
                .tokens
                .as_deref()
                .unwrap_or(&[Kind::In, Kind::Out, Kind::Cache]);
            format::compact(&rates_for(Kind::Out), kinds, &rates::totals(&resps))
        }
    };
    if !args.no_color {
        line = format!("\x1b[2m{line}\x1b[0m");
    }
    if let Some(p) = &cache {
        write_cache(p, &line);
    }
    Ok(line)
}

fn default_root() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("projects"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".claude/projects"))
}

fn cache_path(args: &Args, root: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    (args, root).hash(&mut h);
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("cc-tps").join(format!("{:016x}", h.finish()))
}

fn read_cache(path: &Path, ttl: u64) -> Option<String> {
    let age = fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()?;
    if age >= Duration::from_secs(ttl) {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Writes via a temp file and rename, so concurrent readers never see a
/// partial line. Cache failures are ignored; the next run just recomputes.
fn write_cache(path: &Path, line: &str) {
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::create_dir_all(dir)
        .and_then(|_| fs::write(&tmp, line))
        .and_then(|_| fs::rename(&tmp, path));
}
