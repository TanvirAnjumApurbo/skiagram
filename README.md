# tokscope

> **Status: MVP** · name is a placeholder (will be renamed before publishing)

A local-first, single-binary **CLI + TUI that profiles where your AI coding agent's tokens
actually went** — and why your context window is full. Think *flamegraph for agent token
spend*, not "daily usage table".

Reads the session files your agent already writes — **read-only, fully offline, nothing ever
leaves your machine**.

```
$ tokscope            # spend summary (auto-detects your agent)
$ tokscope tui        # interactive session browser
$ tokscope context    # what's filling your context window, and why
$ tokscope summary --json --since 2026-06-01
```

## Why another usage tool?

Plain usage tracking is a solved problem. tokscope exists because the *numbers themselves*
are usually wrong, and because nobody tells you where the context window went:

1. **Correct accounting.** On-disk logs are unreliable: agents write one JSONL line per
   content block, and every line repeats the request's token usage. A single API request
   shows up as **2–10 lines sharing one `requestId`** (real data we measured: 642 lines for
   262 requests). Naive summation multiplies your spend by that factor. tokscope
   deduplicates per request, prices cache **reads**, **5-minute cache writes**, and
   **1-hour cache writes** at their actual different rates, and flags requests where
   extended-thinking tokens look excluded from `output_tokens` instead of silently
   undercounting. Unknown fields are reported as *unknown* — never as zero, and unknown
   models are never guessed a price.

2. **Context-bloat attribution** *(v0.2, in progress)*. Which MCP server, tool-definition
   set, or plugin is eating your window before you type a word.

3. **Sub-agent attribution.** Spawned sub-agents (`Task`/`Agent` tool) write their own
   transcripts; most tools drop or misattribute that spend. tokscope folds it back into the
   parent session and shows the sub-agent share.

4. **Drill-down UX.** A navigable TUI today; a literal flamegraph SVG export on the
   roadmap (v0.3).

## Install

Prebuilt binaries are planned. For now (any platform with Rust ≥ 1.85):

```
cargo install --path crates/tokscope
```

Single static binary, no runtime, no daemon, no network.

## Usage

```
tokscope [summary] [--agent <id>] [--since YYYY-MM-DD]   # plain tables (default)
tokscope summary --json                                   # machine-readable
tokscope context [--json]                                 # context-window breakdown
tokscope tui                                              # interactive browser
```

- `--agent` — `claude-code` (implemented), `codex`, `cursor`, `gemini`, `copilot`
  (stubs that fail loudly). Default: auto-detect.
- `--since` — only count usage on/after this UTC date.
- `TOKSCOPE_LOG=debug` — see which lines were skipped leniently and why.
- `CLAUDE_CONFIG_DIR` — override the Claude Code data root (default `~/.claude`).

Sample output (from the test fixtures):

```
TOTALS (deduplicated)
 Requests   Input   Output   Cache read   Cache write   Total tokens   Est. cost
        6   5,600      980       11,000           500         18,080     $0.0319

requestId dedup: collapsed 2 duplicate line(s) into 6 request(s) — naive per-line
summing would report 30,850 tokens (+71% overcount avoided)
thinking-token reconciliation: 1 request(s) look UNDERCOUNTED — totals are a lower bound
sub-agent share: 1,300 tokens across 1 request(s) ($0.0087) — attributed to parent sessions
```

## How the accounting works

- **Dedup rule:** assistant lines are grouped by `requestId`; usage counters take the
  field-wise MAX (lines either repeat identical usage or grow monotonically while
  streaming). Lines without a `requestId` are never merged.
- **Cache pricing:** cache-read ≈ 0.1× input, 5m cache-write ≈ 1.25× input, 1h
  cache-write ≈ 2× input — priced separately from an embedded snapshot of public prices
  (refreshable pricing behind an opt-in flag is on the roadmap; the default build makes
  zero network calls).
- **Absence ≠ zero:** a missing usage field makes that total a stated *lower bound*, and
  models missing from the pricing snapshot are listed as unpriced rather than guessed.
- **Sub-agents:** transcripts under `<session>/subagents/` are parsed and folded into the
  parent session's row.
- **Estimates, not invoices.** Costs come from public pricing and are labeled as estimates.

## Context: what's filling your window?

```
tokscope context             # plain-table context-window breakdown
tokscope context --json      # machine-readable
```

Two kinds of number, kept strictly apart:

- **MEASURED** (real, billed tokens, from cache accounting): the **startup overhead** —
  system prompt + tool definitions + memory/CLAUDE.md + your first turn — that's already
  sitting in the window on a fresh session before you type anything, and the **peak/final
  window fill** across your sessions.
- **ESTIMATED** (`~`-prefixed, from on-disk transcript sizes ÷ 4 chars/token, *never*
  billed): the relative composition **by source** (user prompts, assistant text, thinking,
  tool calls/results, attachments), **by MCP server** (which server's tool calls + results
  eat the most window), and the **heaviest individual items** — the context "fat tail"
  where one giant tool result can dominate.

Plus an exact **inventory**: which MCP servers are in play, how many tools were *deferred*
(available but not loaded — so they don't bloat the window, a common misconception), how
many skills were listed, and how many times the context filled up and got compacted.

## Supported agents

| Agent | Status |
|---|---|
| Claude Code | ✅ discover + parse + dedup + sub-agent folding |
| Codex CLI | 🔜 stub (`detect` only) |
| Cursor | 🔜 stub |
| Gemini CLI | 🔜 stub |
| Copilot CLI | 🔜 stub |

## Privacy

Session files contain your prompts and source code. tokscope opens them **read-only**,
processes everything in-memory on your machine, and has **no network code in the default
build**. No telemetry, ever. Test fixtures in this repo are fully synthetic.

## Contributing — adding an agent

Each agent is one implementation of the `Adapter` trait in
`crates/tokscope-core/src/adapters/`:

```rust
pub trait Adapter {
    fn id(&self) -> &'static str;                               // "claude-code"
    fn detect(&self) -> bool;                                   // files present?
    fn discover(&self) -> anyhow::Result<Vec<SessionRef>>;      // find session files
    fn parse(&self, r: &SessionRef) -> anyhow::Result<Session>; // file -> normalized model
}
```

1. Implement the trait (parsers must be lenient: skip unknown lines, never panic).
2. Register it in `adapters::all()`.
3. Add **redacted/synthetic** fixtures under `fixtures/<agent>/` — no real prompts, paths,
   or secrets — plus an `insta` snapshot test and a CLI integration test. Required.
4. `cargo fmt && cargo clippy -- -D warnings && cargo test` must pass.

## Development

```
cargo build
cargo test                        # unit + snapshot + integration
cargo run -p tokscope -- summary
cargo run -p tokscope -- tui
```

See `CLAUDE.md` for the architecture, data-format notes, and the correctness rules every
change must honor.

## License

MIT — see [LICENSE](LICENSE).
