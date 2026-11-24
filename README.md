<p align="center">
  <img src="assets/logo.svg" alt="Tinkr" width="96" height="96" />
</p>

# Tinkr

> **Tinkr optimizes your agent's context so you don't have to.**

A single static Rust binary (`tinkr` + `tinkr-proxy`) that automatically cuts LLM token usage across AI coding agents and IDEs. Install once, confirm once, never configure.

- **Local-only & private** — no telemetry, no TLS interception, auth headers untouched.
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
| `tinkr gain [--json]` | Token savings by layer, provider, and tool |
| `tinkr doctor [--fix] [--network]` | Check OS, shell, coverage table, and repair config drift |
| `tinkr recall <id>` | Print the exact original uncompressed output |
| `tinkr undo [--purge]` | Restore every edited config & env var byte-for-byte (`--purge` also deletes Tinkr data) |
| `tinkr pause` / `tinkr resume` | Pause or resume globally (add `--here` for the current repo only) |
| `tinkr overlap` | Detect competing token optimizers (RTK, Headroom, Caveman, unknown) and manage coexistence |

---

## Already using RTK, Headroom or Caveman?

Tinkr checks (read-only, local-only) whether another token optimizer is already active on your machine before touching any overlapping config.

- **Default is always keep yours:** in non-interactive mode, CI, or `--yes`, Tinkr leaves your existing tool active and automatically yields only its own overlapping layer.
- **Nothing is ever uninstalled or modified in another tool's directories.**
- **Your choice per tool or per agent:** run `tinkr overlap` to keep your current tool (`keep`), switch reversibly with a diff preview (`tinkr`, undoable via `tinkr overlap restore <tool>` or `tinkr undo`), run both safely where non-overlapping (`both`), or measure side-by-side in shadow mode without altering agent output (`compare`).
- **What those tools do that Tinkr does not:** RTK has a broader battle-tested shell-filter catalog and daily CLI dashboard; Headroom ships inline Python/TypeScript SDKs and cross-agent memory/failure-learning; Caveman offers a `/caveman` voice toggle, `/caveman-compress` for `CLAUDE.md`, and Claude plugin marketplace distribution.

See [docs/COEXISTENCE.md](docs/COEXISTENCE.md) for full details on what is read, what is never touched, and how coexistence works.

---

## How It Works (3 Automatic Layers)

1. **Read less** — Hooks verbose shell output (`git`, `ls`/`dir`/`Get-ChildItem`, `grep`/`rg`, test runners, build tools, `docker`, `kubectl`). Compresses noise while keeping errors, stack traces, and `file:line` references verbatim. Originals stay locally for 7 days (`tinkr recall <id>` or MCP `tinkr_retrieve`).
2. **Send less** — Loopback proxy (`tinkr-proxy`) compresses large new tool results (JSON, logs, code, CSV, HTML) without touching prompt-cache prefixes or `cache_control` blocks.
3. **Say less** — Appends a short (≤ 300-token) terse-output rule at the end of the system prompt or rules file. Automatically uses full sentences for security warnings, destructive actions, or confused users, and skips on per-request billing (e.g. Copilot).

---

## Proof (Measured on Real Runs)

| Workload | Before | After | Saved |
|---|---:|---:|---:|
| `cargo test` (80 tests) | 1,426 tok | 211 tok | **−85.2%** |
| `Get-ChildItem -Recurse` | 2,418 tok | 797 tok | **−67.0%** |
| Conversation transcript via MCP `tinkr_read` | 157,464 tok | 132,194 tok | **−16.0%** (25,270 tok) |
| CI fixture suite (`cargo test/build`, `git log/diff`, `ls`, `rg`) | — | — | **≥ 60% median** (all needles preserved) |

Every compressed output includes a recall marker (e.g. `[tinkr:4762fa | 1,426->211 tok | recall]`) so the agent or user can fetch the exact original anytime.

---

## Supported Tools & Providers (Conformance-Tested)

- **24 Tools / IDEs:** Claude Code, Codex CLI, Gemini CLI, Cursor, VS Code, VS Code Insiders, VSCodium, Windsurf, Trae, Kiro, OpenCode, GitHub Copilot CLI, Zed, Claude Desktop, JetBrains IDEs, Visual Studio, Xcode, Neovim, Emacs, Sublime Text, Eclipse, Aider, Goose, Warp.
- **Providers:** Anthropic Messages, OpenAI Chat & Responses, Google Gemini / Vertex AI, Azure OpenAI, GitHub Copilot, OpenAI-compatible (OpenRouter, Ollama, vLLM, DeepSeek, Groq, etc.), and AWS Bedrock pass-through.
- **Optional thin UI extensions:** `extensions/vscode` (VS Code family) and `extensions/jetbrains` — status bar + `Tinkr: Doctor` only, zero compression logic.

---

## Where We Lose (Honest Limits)

- **Small outputs (< 200 tokens) or < 15% savings:** skipped automatically (0% savings, zero risk).
- **Cloud-routed IDE features** (some Copilot / Cursor / Windsurf server-side flows): cannot be proxied locally; Tinkr applies local hooks, MCP tools, and rules only.
- **Signed-request providers (AWS Bedrock SigV4):** proxy passes through untouched; savings come from hooks, MCP, and rules.
- **Codex CLI hook trust:** Codex asks you to approve new hooks once via `/hooks`; Tinkr never bypasses that.
- **Corrupt / unparseable configs:** skipped and reported — never guessed or overwritten.

---

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
