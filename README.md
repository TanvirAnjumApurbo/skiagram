<a id="readme-top"></a>

<div align="center">

<img src="readme_banner.png" alt="skiagram: the flamegraph for your AI agent's token spend" width="100%">

<h3>🔥 The flamegraph for your AI agent's token spend 🔥</h3>

<p>
Profile <strong>where your AI coding agent's tokens actually went</strong>, and <em>why your context window is full</em>.<br>
Local-first. Single static binary. <strong>Nothing ever leaves your machine.</strong>
</p>

<p>
<a href="https://github.com/TanvirAnjumApurbo/skiagram/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/TanvirAnjumApurbo/skiagram/ci.yml?branch=main&style=flat-square&logo=githubactions&logoColor=white&label=CI"></a>
<a href="https://crates.io/crates/skiagram"><img alt="crates.io" src="https://img.shields.io/crates/v/skiagram?style=flat-square&logo=rust&logoColor=white"></a>
<a href="https://crates.io/crates/skiagram"><img alt="Downloads" src="https://img.shields.io/crates/d/skiagram?style=flat-square&label=downloads"></a>
<a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue?style=flat-square"></a>
<a href="https://www.rust-lang.org"><img alt="Rust 1.85+" src="https://img.shields.io/badge/Rust-1.85%2B-CE412B?style=flat-square&logo=rust&logoColor=white"></a>
<img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-555?style=flat-square">
<img alt="Network: zero by default" src="https://img.shields.io/badge/network-zero%20by%20default-3fb950?style=flat-square">
<a href="https://github.com/TanvirAnjumApurbo/skiagram/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/TanvirAnjumApurbo/skiagram?style=flat-square&logo=github&label=stars"></a>
</p>

<p>
<a href="#features"><strong>Features</strong></a> ·
<a href="#install"><strong>Install</strong></a> ·
<a href="#usage"><strong>Usage</strong></a> ·
<a href="#how-it-works"><strong>How it works</strong></a> ·
<a href="#agents"><strong>Agents</strong></a> ·
<a href="#contributing"><strong>Contributing</strong></a>
</p>

</div>

---

## 🤔 What is this?

Your AI coding agent (Claude Code, Codex, Gemini CLI, Copilot CLI) quietly writes a session log
of everything it does. **skiagram** reads those logs (read-only, fully offline) and tells you the
two things they never show you directly:

1. **Where your tokens actually went**, broken down by project, session, model, and token type,
   with the numbers *deduplicated and priced correctly*.
2. **Why your context window is full**: which MCP server, tool-definition set, or giant tool
   result is eating the window before you type a word.

Think *flamegraph for agent token spend*, not "daily usage table".

```bash
skiagram            # spend summary (auto-detects your agent)
skiagram tui        # interactive drill-down browser
skiagram context    # what's filling your context window, and why
skiagram flame      # export a flamegraph SVG of where the tokens went
```

---

<a id="features"></a>

## ✨ Features

Plain per-day usage tracking is a solved problem. skiagram exists because **the numbers themselves
are usually wrong**, and because nobody tells you **where the context window went**.

### 🎯 Correct accounting (getting the number right is the whole point)

On-disk logs are unreliable. Agents write **one JSONL line per content block**, and every line
repeats the request's token usage, so a single API request shows up as **2 to 10 lines sharing one
`requestId`** (real data we measured: 642 lines for 262 requests). Naive summation multiplies your
spend by that factor. skiagram:

- **Deduplicates per request** before summing anything (the core accounting step).
- **Prices cache reads, 5-minute cache writes, and 1-hour cache writes separately.** They differ by
  up to ~10×, and lumping them quietly inflates or deflates your bill.
- **Attributes extended-thinking tokens** as a *measured* share of output (they're already inside
  `output_tokens`, verified on 2,268 real requests, so we never invent a phantom undercount).
- **Treats absence as unknown, not zero.** A missing usage field becomes a stated *lower bound*; a
  model missing from the price snapshot is listed as *unpriced*, never guessed.

### 🧱 Context-bloat attribution

`skiagram context` breaks down what fills your window **by source** (system prompt, tool/MCP
definitions, history, attachments), **by MCP server**, and surfaces the **heaviest individual
items**, the one giant tool result that quietly dominates. It separates *measured, billed* tokens
from *estimated* composition and never blurs the two.

### 🌳 Sub-agent attribution

Spawned sub-agents (the `Task` / `Agent` tool) write their **own** transcripts; most tools drop or
misattribute that spend. skiagram folds it back into the parent session and shows the sub-agent share.

### 🔥 Drill-down UX (TUI + flamegraph)

A navigable **TUI** (sessions → turns → context breakdown) and a literal **flamegraph SVG** export,
color-coded by token type with a legend, regroupable with `--group-by`.

### 🔒 Local-first · 📦 single binary

No daemon, no proxy in the request path, no telemetry, **no network calls in the default build**.
One static binary you can drop anywhere.

---

<a id="install"></a>

## 📦 Install

skiagram is a single static binary. Pick whichever channel fits your setup; every command is one line.

**Cargo** (any OS with Rust ≥ 1.85):

```bash
cargo install skiagram
```

**Prebuilt binary via [cargo-binstall](https://github.com/cargo-bins/cargo-binstall)** (no compile):

```bash
cargo binstall skiagram
```

**macOS and Linux (Homebrew):**

```bash
brew install TanvirAnjumApurbo/tap/skiagram
```

**macOS and Linux (shell installer):**

```bash
curl -LsSf https://github.com/TanvirAnjumApurbo/skiagram/releases/latest/download/install.sh | sh
```

**Windows (Scoop):**

```bash
scoop bucket add skiagram https://github.com/TanvirAnjumApurbo/scoop-bucket && scoop install skiagram
```

**Windows (winget):**

```bash
winget install TanvirAnjumApurbo.skiagram
```

**Windows (PowerShell installer):**

```powershell
irm https://github.com/TanvirAnjumApurbo/skiagram/releases/latest/download/install.ps1 | iex
```

**npm** (install globally, or run on demand with `npx`):

```bash
npm install -g skiagram
```

```bash
npx skiagram
```

**From source:**

```bash
git clone https://github.com/TanvirAnjumApurbo/skiagram && cd skiagram && cargo install --path crates/skiagram
```

Prebuilt archives for **macOS · Linux · Windows** are attached to every
[GitHub Release](https://github.com/TanvirAnjumApurbo/skiagram/releases).

---

<a id="usage"></a>

## 🚀 Usage

Run it with no arguments to auto-detect your agent and print a deduplicated spend summary:

```bash
skiagram
```

### Commands

| Command | What it does |
| --- | --- |
| `skiagram summary` | 🧾 Token + cost summary, deduplicated (the default command) |
| `skiagram context` | 🧱 What's filling your context window, by source, by MCP server, fat tail |
| `skiagram anomalies` | 🚨 Fat-tail requests that dominate spend, plus retry storms |
| `skiagram classify` | 🏷️ Spend broken down by task type (debugging / features / refactor / …) |
| `skiagram flame` | 🔥 Export a flamegraph SVG of token spend |
| `skiagram tui` | 🖥️ Interactive drill-down browser (arrow keys / `j` `k`, `q` to quit) |
| `skiagram watch` | 📡 Live-tail: re-render the summary whenever session files change |

### Global flags

| Flag | Effect |
| --- | --- |
| `--agent <id>` | `claude-code`, `codex`, `gemini`, `copilot`, or `cursor`. Default: **auto-detect** |
| `--since YYYY-MM-DD` | Only count usage on/after this UTC date |
| `--refresh-pricing` | Refresh model prices from LiteLLM first (needs the `network` build feature) |

Most commands also accept `--json` for machine-readable output.

### Examples

```bash
skiagram summary --json --since 2026-06-01
```

```bash
skiagram --agent codex context
```

```bash
skiagram flame --metric cost --out spend.svg
```

```bash
skiagram flame --group-by project,model,type
```

```bash
skiagram flame --fold | flamegraph.pl > spend.svg
```

<details>
<summary><strong>Sample <code>summary</code> output</strong> (from the bundled synthetic fixtures)</summary>

```text
TOTALS (deduplicated)
 Requests   Input   Output   Cache read   Cache write   Total tokens   Est. cost
        6   5,600      980       11,000           500         18,080     $0.0319

requestId dedup: collapsed 2 duplicate line(s) into 6 request(s); naive per-line
summing would report 30,850 tokens (+71% overcount avoided)
extended thinking: used in 1 of 6 request(s); already counted inside Output above; visible thinking ~414 est. token(s)
sub-agent share: 1,300 tokens across 1 request(s) ($0.0087), attributed to parent sessions
```

Every number is traceable to `(model, token type, unit price)`, with no magic constants.

</details>

---

## 🔥 Flamegraph

`skiagram flame` turns your spend into a navigable flamegraph SVG. Frame **width = tokens** (or cost,
with `--metric cost`), and the default hierarchy is **project → session → model → token-type**:

```bash
skiagram flame --out spend.svg
```

- **Colored by token type.** `input`, `output`, `cache-read`, `cache-write`, and `thinking` each get
  a fixed swatch with a legend, so you read the graph by color instead of squinting at labels.
- **Readable sessions.** The opaque session UUID is shortened to an 8-char prefix.
- **Regroupable.** `--group-by project,model,type` drops or reorders levels. Regrouping never changes
  the totals; it only changes how the same spend is sliced.
- **Pipe-friendly.** `--fold` prints the raw folded stacks to stdout for any flamegraph tool.

Frame widths agree with `summary` **by construction**: the same request-level dedup feeds both, so
the picture and the table can never disagree.

---

<a id="how-it-works"></a>

## 🧮 How the accounting works

> Getting the number right is a **core feature**, not a footnote. Here is exactly what skiagram does,
> so you can trust (and challenge) every figure.

- **Dedup rule.** Assistant lines are grouped by `requestId`; usage counters take the field-wise
  **MAX** (lines either repeat identical usage or grow monotonically while streaming). Lines without
  a `requestId` are never merged.
- **Cache pricing.** cache-read ~0.1× input, 5-minute cache-write ~1.25× input, 1-hour cache-write
  ~2× input. Each is priced separately from an embedded snapshot of public prices.
- **Thinking tokens.** Claude Code's `output_tokens` *already includes* extended-thinking tokens
  (verified), so we never add an estimate on top. When an agent reports thinking as a *separate*
  count (Codex, Gemini), we keep it disjoint so the sum still balances.
- **Absence is not zero.** A missing usage field makes that total a stated lower bound; unpriced
  models are listed, not guessed.
- **Estimates, not invoices.** Costs come from public pricing and are labeled as estimates.

The embedded price snapshot keeps the default build fully **offline**. `--refresh-pricing` (behind the
opt-in `network` feature) updates it from LiteLLM and caches the result for later offline runs.

---

## 🧱 Context: what's filling your window?

```bash
skiagram context
```

```bash
skiagram context --json
```

Two kinds of number, kept strictly apart:

- **MEASURED** (real, billed tokens): your **startup overhead** (system prompt + tool definitions +
  memory files + first turn) that's already in the window on a fresh session before you type
  anything, plus the **peak/final window fill** across sessions.
- **ESTIMATED** (`~`-prefixed, from transcript sizes, *never billed*): the relative composition **by
  source**, **by MCP server**, and the **heaviest individual items**, the context "fat tail" where one
  giant tool result dominates.

Plus an exact **inventory**: which MCP servers are in play, how many tools were *deferred* (available
but not loaded, so they *don't* bloat the window, a common misconception), how many skills were listed,
and how many times the window filled up and got compacted.

---

<a id="agents"></a>

## 🤖 Supported agents

| Agent | Status | Notes |
| --- | :---: | --- |
| **Claude Code** | ✅ | discover + parse + dedup + sub-agent folding + thinking attribution |
| **Codex CLI** | ✅ | real token reconciliation (cumulative vs per-request delta) |
| **Gemini CLI** | ✅ | real per-message tokens, dedup by message id, disjoint thoughts |
| **Copilot CLI** | ✅ | structural (Copilot logs no per-request billing tokens) |
| **Cursor** | ⬜ | deferred: per-request `tokenCount` is ~99% zeroed; needs bundled `rusqlite` |

Adding a new agent is one trait implementation. See [Contributing](#contributing).

---

## ⚙️ Configuration

**Config file:** `config.toml` in your platform's config dir (resolved via `directories`; override the
whole path with `$SKIAGRAM_CONFIG`). Unknown keys are ignored, so it stays forward-compatible:

```toml
# Skip auto-detect and always read this agent unless --agent is passed.
default_agent = "claude-code"
```

Agent precedence: `--agent` flag › `default_agent` in config › auto-detect.

**Environment variables**

| Variable | Effect |
| --- | --- |
| `SKIAGRAM_LOG=debug` | See which lines were skipped leniently, and why |
| `SKIAGRAM_CONFIG` | Path to an alternate `config.toml` |
| `CLAUDE_CONFIG_DIR` | Override the Claude Code data root (default `~/.claude`) |
| `CODEX_HOME` / `GEMINI_HOME` / `COPILOT_HOME` | Override each agent's data root |

---

## 🔒 Privacy

Session files contain your prompts and source code. skiagram:

- opens them **read-only** and processes everything **in-memory, on your machine**;
- has **no network code in the default build**: no telemetry, no uploads, ever;
- ships only **fully synthetic** test fixtures (no real prompts, paths, or secrets).

It reads files your agents already write; it is **not** a proxy or interceptor in the request path.

---

<a id="contributing"></a>

## 🛠️ Contributing

PRs welcome, especially **new agent adapters**. Each agent is one implementation of the `Adapter`
trait in `crates/skiagram-core/src/adapters/`:

```rust
pub trait Adapter {
    fn id(&self) -> &'static str;                               // "claude-code"
    fn detect(&self) -> bool;                                   // files present?
    fn discover(&self) -> anyhow::Result<Vec<SessionRef>>;      // find session files
    fn parse(&self, r: &SessionRef) -> anyhow::Result<Session>; // file -> normalized model
}
```

1. Implement the trait. Parsers must be **lenient**: skip unknown lines with a `tracing::debug`, never
   panic. A corrupt line must not abort a whole session parse.
2. Register it in `adapters::all()`.
3. Add **redacted/synthetic** fixtures under `fixtures/<agent>/` (no real prompts, paths, or secrets),
   plus an `insta` snapshot test and an `assert_cmd` CLI test. **Required.**
4. `cargo fmt && cargo clippy -- -D warnings && cargo test` must all pass.

The `skiagram-core` crate's module docs explain the architecture, the data-format notes, and the
correctness rules every change must honor.

---

## 🧑‍💻 Development

```bash
cargo build                          # debug build
cargo test                           # unit + snapshot + integration tests
cargo run -p skiagram -- summary     # run the CLI
cargo run -p skiagram -- tui         # run the TUI
cargo clippy -- -D warnings          # lint (CI-enforced)
cargo fmt --check                    # format check (CI-enforced)
```

The workspace is two crates: **`skiagram-core`** (pure domain logic: model, adapters, analysis,
pricing; no terminal I/O, no network) and **`skiagram`** (the CLI + TUI binary that owns all I/O).

---

## 🗺️ Project status

**skiagram `v0.1.0` is the first public release.** It ships:

- Correct, deduplicated token + cost accounting
- Adapters for **Claude Code**, **Codex CLI**, **Gemini CLI**, and **Copilot CLI**
- Context-window bloat attribution, sub-agent attribution, anomaly detection, and task classification
- Flamegraph SVG export, an interactive TUI, and live-tail (`watch`)
- A config file, optional online pricing refresh, and a fully offline default build

**Planned next:** a Cursor adapter (waiting on usable per-request token data), refreshable-pricing UX
polish, and a homebrew-core submission.

---

## 📄 License

[MIT](LICENSE) © skiagram contributors.

---

<div align="center">

Built with 🦀 **Rust** · Local-first · Offline by default

<a href="https://github.com/TanvirAnjumApurbo/skiagram/issues/new">🐛 Report a bug</a> ·
<a href="https://github.com/TanvirAnjumApurbo/skiagram/issues/new">💡 Request a feature</a> ·
<a href="https://github.com/TanvirAnjumApurbo/skiagram/stargazers">⭐ Star the repo</a>

<sub>If skiagram saved you some tokens, a ⭐ helps other people find it. Thank you!</sub>

<br><br>

<a href="#readme-top"><sub>↑ Back to top</sub></a>

</div>
