# cc-tps

Tokens per second across all Claude Code sessions on this machine, as one
status line segment.

## Examples

Default, labeled: output tokens per second over the last 5 minutes, overall,
per response, and while any response was in flight.

<pre>
$ cc-tps
5m out 31/s · resp 90/s · active 90/s
</pre>

Compact: space-separated `key:value` pairs, which suit a status line built
from short segments. The same rates, then total input and output tokens and
the share of input read from the cache.

<pre>
$ cc-tps --format compact
tps:31/s e2e:90/s act:90/s in:3.0M out:9.2k cache:99%
</pre>

Pick and order rates and token kinds:

<pre>
$ cc-tps --tokens out,cache
5m out 31/s · resp 90/s · active 90/s | cache 10.0k/s · resp 29.5k/s · active 29.5k/s

$ cc-tps --format compact --rates tp --tokens cache
tps:31/s cache:99%

$ cc-tps --window 1m --rates tp,active
1m out 9.3/s · active 72/s
</pre>

With nothing in the window:

<pre>
$ cc-tps --format compact
tps:0/s e2e:– act:– in:0 out:0 cache:–
</pre>

cc-tps reads the transcripts Claude Code writes under `~/.claude/projects`,
including subagent transcripts. It needs no hook, plugin or daemon.

## Install

```bash
cargo install --git https://github.com/jgabor/cc-tps
```

To use cc-tps as your whole status line, point `statusLine` in
`~/.claude/settings.json` at it. `refreshInterval` re-runs it on a timer, so
the figure keeps moving while your own session is idle:

```json
"statusLine": {
  "type": "command",
  "command": "cc-tps --format compact",
  "refreshInterval": 5
}
```

If you already have a status line script, append cc-tps's output to what it
prints, for example:

```bash
printf '%s %s\n' "$your_line" "$(cc-tps --format compact 2>/dev/null)"
```

## What the numbers mean

Every figure covers API responses that ended inside the window (default the
last 5 minutes). A response counts in full even if it started before the
window.

| Rate     | Label    | Meaning                                                    |
| -------- | -------- | ---------------------------------------------------------- |
| `tp`     | none     | Tokens divided by the window length. The overall throughput. |
| `resp`   | `resp`   | Tokens divided by the summed duration of each response.    |
| `active` | `active` | Tokens divided by the wall time when any response was in flight. |

A response's duration runs from the request (the `api-request` or `user`
record just before it) to its last content block. It includes time to first
token, so `resp` is end-to-end speed, not model decode speed. Responses with
no request record in the 10 minutes before them count for `tp` only. With no
responses in the window, `tp` shows `0` and the other rates show `–`.

| Kind    | Labeled (rate)                                          | Compact (window total)                     |
| ------- | ------------------------------------------------------- | ------------------------------------------ |
| `out`   | `output_tokens`, thinking included                      | same                                       |
| `in`    | `input_tokens` (uncached input)                         | all input: uncached, cache reads and writes |
| `cache` | `cache_read_input_tokens + cache_creation_input_tokens` | cache hit share: reads / all input         |

## Options

```text
--window 5m               window: 300, 90s, 5m, 1h
--rates tp,resp,active    rates to show, in order
--tokens KINDS            kinds to show, in order: out, in, cache
--format labeled          layout: labeled or compact
--cache-ttl 2s            reuse the last result this long; 0 turns it off
--projects-dir PATH       default $CLAUDE_CONFIG_DIR/projects or ~/.claude/projects
--no-color                drop the dim styling
```

### Labeled (default)

Rates per token kind, divided by `·`. `--tokens` defaults to `out`, and each
kind gets its own group, joined by ` | `:

```text
$ cc-tps --rates tp --tokens out,cache
5m out 31/s | cache 10.0k/s
```

### Compact

Space-separated `key:value` pairs. The rates always use output tokens (`tps`, `e2e` and `act` are `tp`, `resp` and
`active`). Then come window figures, one per kind in `--tokens`, which
defaults to `in,out,cache`: all input tokens, output tokens, and the share
of input read from the cache. The share is rounded down, so `100%` means no
input missed the cache, and it shows `–` when there was no input. The window
is not shown.

```text
$ cc-tps --format compact
tps:31/s e2e:90/s act:90/s in:3.0M out:9.2k cache:99%
```

In long sessions most input is read from the cache, so the share tends to
stay high. Dips point at an expired cache (after an idle gap longer than the
cache lifetime), a compaction or fresh subagents, and cache writes cost more
than reads.

## Limits

- A response appears only once its content blocks reach the transcript, so a
  long response still streaming is not counted yet. The figure can trail by
  up to one response.
- Advisor tokens are not counted. If your sessions use the advisor tool, a
  response that calls it reports the advisor's tokens only in `usage.iterations`, and cc-tps reads
  the top-level `usage`, which covers the main model alone. Such a response
  also spans the advisor's run, which lowers `resp` and `active`.
- Only local sessions count, not cloud or remote ones.
- The transcript format is internal to Claude Code and undocumented. cc-tps
  was built against Claude Code 2.1.308, and a later release can change the
  format.
- A message id that shows up in several transcripts (resumed sessions copy
  history) counts once.
- On an unexpected error cc-tps prints nothing on stdout and the reason on
  stderr, so the status line keeps working. Run `claude --debug` to see it.

## Cost

Each run lists transcripts changed in the last 20 minutes and reads only
their recent tail. Measured on one Linux machine with about 1,000 transcripts
and 6 active sessions, a run takes about 10 ms. Results are cached in
`$XDG_RUNTIME_DIR/cc-tps/` for `--cache-ttl`, so several sessions refreshing
at once share one scan; a cached run takes under 1 ms.

## License

cc-tps is released under the [MIT License](LICENSE).
