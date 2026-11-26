<p align="center">
  <img src="assets/logo.svg" alt="Tinkr" width="96" height="96" />
</p>

# Tinkr

> **Tinkr optimizes your agent's context so you don't have to.**

A single static Rust binary (`tinkr` + `tinkr-proxy`) that automatically cuts NET LLM token usage across AI coding agents and IDEs. Install once, confirm once, never configure.

- **Local-only & private** — no telemetry, no TLS interception, auth headers untouched, secrets redacted in local recall storage (`0600` / user-only Windows ACL).
- **Fail-open** — on any error, timeout, or uncertainty, passes original output through unchanged.
- **Fully reversible** — backs up every edited file (`*.tinkr.bak`) and restores byte-for-byte with `tinkr undo`.

---

## Install

| OS | Command |
|---|---|
| **Windows (Winget)** | `winget install thunderkex.Tinkr` *(or `scripts/install.ps1`)* |
| **macOS / Linux (Homebrew)** | `brew install thunderkex/tap/tinkr` *(or `scripts/install.sh`)* |

Prebuilt binaries (`x64` / `arm64` for Windows, macOS, Linux) and `SHA256SUMS` are published on [GitHub Releases](https://github.com/thunderkex/Tinkr/releases).

## Quick Start

Run `tinkr` once:

```sh
tinkr
```

Tinkr scans your machine for installed AI tools, shows **one** summary of changes, and asks **one** question:

```
Apply changes? [y]es (auto-detected) / [a]ll (every supported tool) / [n]o:
```

- **`y`** — configure only the tools detected on your machine *(or `tinkr setup --yes`)*
- **`a`** — pre-configure all 24 supported tools *(or `tinkr setup --all`)*
- **`n`** — cancel without touching anything

## Commands

| Command | What it does |
|---|---|
| `tinkr` | First run = setup; afterwards = status |
| `tinkr gain [--why <cmd>] [--json]` | NET token savings by layer, command, agent, and provider, or explain tuning for `<cmd>` |
| `tinkr doctor [--fix] [--network]` | Check OS, shell, coverage table, auto-disabled profiles, and repair config drift |
| `tinkr recall <id \| --purge>` | Print the original uncompressed output, or purge local recall items |
| `tinkr undo [--purge]` | Restore every edited config & env var byte-for-byte (`--purge` also deletes Tinkr data) |
| `tinkr pause` / `tinkr resume` | Pause or resume globally (add `--here` for the current repo only) |
| `tinkr overlap` | Detect competing token optimizers (RTK, Headroom, Caveman, unknown) and manage coexistence |

---

## Already using RTK, Headroom or Caveman?

Tinkr checks (read-only, local-only) whether another token optimizer is already active on your machine before touching any overlapping config.

- **Default is always keep yours:** in non-interactive mode, CI, or `--yes`, Tinkr leaves your existing tool active and automatically yields only its own overlapping layer.
- **Nothing is ever uninstalled or modified in another tool's directories.**
- **Your choice per tool or per agent:** run `tinkr overlap` to keep your current tool (`keep`), switch reversibly with a diff preview (`tinkr`, undoable via `tinkr overlap restore <tool>` or `tinkr undo`), run both safely where non-overlapping (`both`), or measure side-by-side in shadow mode without altering agent output (`compare`).
- **What those tools do that Tinkr does not:** RTK's markerless `rtk err` saves slightly more on stateless build-error loops and ships `rtk gain --daily` / `rtk discover`; Headroom ships inline Python/TypeScript `compress(messages)` SDKs and cross-agent memory/failure-learning; Caveman offers an interactive `/caveman` voice toggle, `/caveman-compress` for `CLAUDE.md`, and Claude plugin marketplace distribution.

See [docs/COEXISTENCE.md](docs/COEXISTENCE.md) for full details on what is read, what is never touched, and how coexistence works.

---

## How It Works (3 Automatic Layers)

1. **Read less** — Declarative TOML CLI filters (`git`, `cargo`, `pytest`, `jest`/`vitest`, `tsc`, `eslint`, `docker`, `kubectl`, `gh`, `aws`, `npm`/`pnpm`/`pip`, `go test`, `rspec`, `Get-ChildItem`/`Select-String`/`Get-Content`), repeated file-read deduplication (hash stubs & unified diffs), and recall-rate self-tuning. Originals stay locally for 7 days (`tinkr recall <id>` or MCP `tinkr_retrieve`).
2. **Send less** — Loopback proxy (`tinkr-proxy`) deduplicates repeated file reads in uncached turns and compresses large new JSON, log, CSV, and HTML tool results while keeping first, last, critical, and structural/level outliers — without touching prompt-cache prefixes or `cache_control` blocks.
3. **Say less** — Appends a short (114-token, ≤ 300-token budget) terse-output rule at the end of the system prompt or rules file. Switches to full sentences for security warnings, destructive actions, ambiguity, or confused users, skips per-request billing (e.g. Copilot), and drops automatically if overhead isn't repaid within 5 turns.

---

## Proof (Measured `o200k_base` NET Savings)

| Workload | Before | After | NET Saved |
|---|---:|---:|---:|
| `cargo test` (400 tests, 1 deep failure) | 6,362 tok | 222 tok | **−96.5%** (+6,140 tok) |
| `git log -p` (200 commits, schema change) | 43,358 tok | 8,463 tok | **−80.5%** (+34,895 tok) |
| 2,000-line file read 5× with 2 edits between | 210,229 tok | 42,478 tok | **−79.7%** (+167,527 tok) |
| `kubectl logs` (3,000 repeats, 1 `OOMKilled`) | 132,536 tok | 173 tok | **−99.9%** (+132,363 tok) |
| 1.2 MB JSON array (1 `"status":"error"` outlier) | 295,971 tok | 222 tok | **−99.9%** (+295,749 tok) |
| PowerShell `Get-ChildItem -Recurse` (`node_modules` + `src`) | 12,594 tok | 76 tok | **−99.4%** (+12,518 tok) |

Every compressed output includes a recall marker (e.g. `[tinkr:4762fa | 6,362->222 tok | recall]`), and `NET` deducts all injected rules, recall markers, and recall round-trips.

---

## Supported Tools & Providers (Conformance-Tested)

- **24 Tools / IDEs:** Claude Code, Codex CLI, Gemini CLI, Cursor, VS Code, VS Code Insiders, VSCodium, Windsurf, Trae, Kiro, OpenCode, GitHub Copilot CLI, Zed, Claude Desktop, JetBrains IDEs, Visual Studio, Xcode, Neovim, Emacs, Sublime Text, Eclipse, Aider, Goose, Warp.
- **Providers:** Anthropic Messages, OpenAI Chat & Responses, Google Gemini / Vertex AI, Azure OpenAI, GitHub Copilot, OpenAI-compatible (OpenRouter, Ollama, vLLM, DeepSeek, Groq, etc.), and AWS Bedrock pass-through.
- **Optional thin UI extensions:** `extensions/vscode` (VS Code family) and `extensions/jetbrains` — status bar + `Tinkr: Doctor` only, zero compression logic.

---

## Where We Lose (Honest Limits)

- **Small outputs (< 200 tokens) or < 15% savings:** passed through unchanged (`0` NET tokens saved, zero risk of marker overhead turning NET negative).
- **Stateless build-error loops vs markerless filters:** `rtk err` saves ~78 more tokens across 3 `pnpm build` retries (`11,148` vs `11,070` NET tokens) because Tinkr attaches reversible recall markers to each compressed turn.
- **Unprofitable profiles & short sessions:** profiles with `NET <= 0` over 20+ samples are auto-disabled (`tinkr gain`), and say-less is dropped if its rule overhead isn't repaid within 5 turns.
- **Cloud-routed IDE features & signed-request APIs (AWS Bedrock SigV4):** cannot be proxied locally; savings come from local hooks, MCP, and rules only.
- **Codex CLI hook trust & unparseable configs:** Codex asks you to approve new hooks once via `/hooks` (never bypassed); corrupt configs are skipped and reported without guessing.

---

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
