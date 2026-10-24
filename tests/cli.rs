//! End-to-end checks: build a projects tree, run the binary, compare output.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NOW: &str = "2026-01-01T12:00:00Z";

struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("cc-tps-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Tree(d)
    }

    fn file(&self, rel: &str, lines: &[String]) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, lines.join("\n") + "\n").unwrap();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn marker(kind: &str, ts: &str) -> String {
    format!(r#"{{"type":"{kind}","timestamp":"{ts}","message":{{"role":"user","content":[]}}}}"#)
}

fn asst(id: &str, ts: &str, out: u64) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","message":{{"id":"{id}","content":[],"usage":{{"input_tokens":2,"output_tokens":{out},"cache_read_input_tokens":900,"cache_creation_input_tokens":100}}}}}}"#
    )
}

fn t(hms: &str) -> String {
    format!("2026-01-01T{hms}Z")
}

/// Two sessions, a subagent, a message duplicated across files, a response
/// that straddles the window start, one without a start marker, one too old
/// and one in the future.
fn busy() -> Tree {
    let tree = Tree::new(&format!("busy-{:?}", std::thread::current().id()));
    tree.file(
        "proj-a/s1.jsonl",
        &[
            asst("old", &t("11:50:00"), 9999),
            marker("api-request", &t("11:54:50")),
            asst("m0", &t("11:55:05"), 150),
            marker("user", &t("11:56:00")),
            asst("m1", &t("11:56:04"), 600),
            asst("m1", &t("11:56:10"), 600),
            marker("api-request", &t("11:57:00")),
            asst("m2", &t("11:57:05"), 310),
            asst("future", &t("12:00:30"), 5000),
        ],
    );
    tree.file(
        "proj-a/s1/subagents/agent-1.jsonl",
        &[
            marker("user", &t("11:56:05")),
            asst("m3", &t("11:56:15"), 450),
        ],
    );
    tree.file(
        "proj-b/s2.jsonl",
        &[
            asst("m1", &t("11:56:04"), 600),
            asst("m1", &t("11:56:10"), 600),
            asst("m4", &t("11:58:00"), 300),
        ],
    );
    tree
}

fn run(root: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cc-tps"))
        .arg("--projects-dir")
        .arg(root)
        .args(["--now", NOW, "--no-color"])
        .args(extra)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn default_shows_three_output_rates() {
    // In the window: m0 150 (15 s), m1 600 (10 s), m2 310 (5 s), m3 450 (10 s)
    // and m4 300 (no start). tp = 1810 / 300 s. resp = 1510 / 40 s.
    // active = 1510 / 35 s, since m1 and m3 overlap.
    let tree = busy();
    assert_eq!(
        stdout(&run(&tree.0, &[])),
        "5m out 6.0/s · resp 38/s · active 43/s\n"
    );
}

#[test]
fn rates_and_tokens_combine_independently() {
    let tree = busy();
    let o = run(&tree.0, &["--rates", "tp", "--tokens", "out,cache"]);
    // Five responses with 1000 cache tokens each over 300 s.
    assert_eq!(stdout(&o), "5m out 6.0/s | cache 17/s\n");
    let o = run(&tree.0, &["--rates", "active,resp", "--tokens", "in"]);
    // Four timed responses with 2 input tokens each.
    assert_eq!(stdout(&o), "5m in active 0.2/s · resp 0.2/s\n");
}

#[test]
fn compact_format_shows_output_rates_then_totals() {
    let tree = busy();
    // Five responses in the window, each with 2 uncached, 900 cache-read and
    // 100 cache-write input tokens: 5010 input, 4500 / 5010 = 89.8% from cache.
    let o = run(&tree.0, &["--format", "compact"]);
    assert_eq!(
        stdout(&o),
        "tps:6.0/s e2e:38/s act:43/s in:5.0k out:1.8k cache:89%\n"
    );
    // Rates stay on output tokens; --tokens picks and orders the totals.
    let o = run(
        &tree.0,
        &[
            "--format",
            "compact",
            "--rates",
            "active,tp",
            "--tokens",
            "cache,out",
        ],
    );
    assert_eq!(stdout(&o), "act:43/s tps:6.0/s cache:89% out:1.8k\n");
}

#[test]
fn window_flag_changes_scope_and_label() {
    let tree = busy();
    // 3 minutes back from noon reaches m2 (310, 5 s) and m4 (300, no start).
    let o = run(&tree.0, &["--window", "180s", "--rates", "tp,resp"]);
    assert_eq!(stdout(&o), "3m out 3.4/s · resp 62/s\n");
}

#[test]
fn idle_tree_shows_zero_and_dashes() {
    let tree = Tree::new("idle");
    tree.file(
        "p/s.jsonl",
        &[
            marker("user", &t("10:00:00")),
            asst("x", &t("10:00:05"), 100),
        ],
    );
    assert_eq!(
        stdout(&run(&tree.0, &[])),
        "5m out 0/s · resp – · active –\n"
    );
    assert_eq!(
        stdout(&run(&tree.0, &["--format", "compact"])),
        "tps:0/s e2e:– act:– in:0 out:0 cache:–\n"
    );
}

#[test]
fn color_is_dim_by_default() {
    let tree = Tree::new("color");
    let o = Command::new(env!("CARGO_BIN_EXE_cc-tps"))
        .arg("--projects-dir")
        .arg(&tree.0)
        .args(["--now", NOW])
        .output()
        .unwrap();
    assert_eq!(stdout(&o), "\x1b[2m5m out 0/s · resp – · active –\x1b[0m\n");
}

#[test]
fn missing_root_prints_nothing_and_succeeds() {
    let o = run(Path::new("/nonexistent/cc-tps"), &[]);
    assert!(o.status.success());
    assert!(o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("cc-tps: "));
}

#[test]
fn bad_flags_fail_loudly() {
    let tree = Tree::new("bad");
    for args in [
        &["--window", "0"][..],
        &["--window", "5x"],
        &["--rates", "nope"],
        &["--format", "nope"],
    ] {
        let o = run(&tree.0, args);
        assert!(!o.status.success(), "{args:?} succeeded");
        assert!(o.stdout.is_empty());
    }
}

#[test]
fn cache_reuses_the_last_line_within_ttl() {
    let tree = Tree::new("cache");
    let runtime = Tree::new("cache-runtime");
    let go = |ttl: &str| {
        let o = Command::new(env!("CARGO_BIN_EXE_cc-tps"))
            .arg("--projects-dir")
            .arg(&tree.0)
            .args(["--no-color", "--rates", "tp", "--cache-ttl", ttl])
            .env("XDG_RUNTIME_DIR", &runtime.0)
            .output()
            .unwrap();
        stdout(&o)
    };
    assert_eq!(go("60"), "5m out 0/s\n");
    let ts = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(10);
    tree.file("p/s.jsonl", &[asst("x", &ts.to_string(), 3000)]);
    assert_eq!(go("60"), "5m out 0/s\n", "cache was not used");
    assert_eq!(go("0"), "5m out 10/s\n", "cache-ttl 0 still cached");
}
