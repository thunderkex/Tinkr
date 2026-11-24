# Coexistence & Overlap Check

Tinkr is designed to coexist safely with other local token-saving and context-compression tools such as **RTK** (`rtk` / `rtk-oss`), **Headroom** (`headroom-ai`), **Caveman** (`@caveman-ai/cli`), and unknown look-alikes.

## Core Guarantees

1. **Default = change nothing.** In non-interactive mode, CI, or `--yes`, Tinkr keeps your existing tool active and automatically yields only its own overlapping layer.
2. **Never uninstalls or modifies foreign binaries or state.** Tinkr never uninstalls another tool, never edits its packages, and never touches its own config or data directories (such as `~/.config/rtk` or `~/.headroom`).
3. **Read-only detection, local only.** Detection inspects only known agent config filenames, safe non-secret environment variables (`*_BASE_URL`, `RTK_*`, `HEADROOM_*`, `CAVEMAN_*`), loopback ports, and optional `--version` / `verify_args` probes with a strict 2-second timeout. It never reads source code, prompts, `.env` files, or credentials, and sends zero telemetry.
4. **Reversible and no edit wars.** Every edit Tinkr makes under Choice `[2]` is backed up byte-for-byte to `*.tinkr.bak`, recorded in the change manifest, and restored by `tinkr overlap restore <tool>` or `tinkr undo`. If another tool re-wires itself repeatedly, Tinkr stops after 2 repairs and asks instead of fighting.

## Commands

```bash
tinkr overlap                                         # show detected tools, overlaps, and choices
tinkr overlap report --json                           # machine-readable JSON for scripts
tinkr overlap choose <tool> keep|tinkr|both|compare   # save your choice (optional: --agent <id>)
tinkr overlap restore <tool>                          # re-enable what Tinkr disabled for <tool>
```

## What Each Choice Does

- **`[1] keep` (default):** Tinkr yields every overlapping layer on agents where your current tool is active, while still helping on uncovered agents and non-overlapping layers.
- **`[2] tinkr`:** Tinkr shows the exact diff preview and disables only the overlapping hook entry in Tinkr-managed agent configs (with a byte-exact backup). Where a tool's own off-switch is safer (such as stopping `headroom proxy` or toggling `/caveman`), Tinkr prints the command for you to run instead.
- **`[3] both`:** Tinkr applies per-layer coexistence rules:
  - *Read-less:* Tinkr skips commands covered by the other tool's profile and passes through any output already carrying another tool's markers (`[full output: rtk recall ...]`, `[headroom:...]`).
  - *Send-less:* Single loopback proxy per agent by default (falls back to `keep` with an explanation).
  - *Say-less:* Strictly one voice rule active per prompt.
- **`[4] compare` (shadow mode):** Tinkr evaluates what it would have emitted without changing agent-visible output, records exact `o200k_base` token counts (`measured`) and critical-line preservation locally, and refuses to declare a winner until at least 10 samples are collected.

## Honest Comparisons

All tool capability and comparison statements live in `crates/tinkr-core/overlap-profiles/*.toml` with an `https://` source URL, an ISO check date, and a required `does_better_than_tinkr` section. Automated tests fail on unsourced claims or missing honesty fields and warn when a profile is older than 90 days. External profile updates require a valid detached `.sig` signature; tampered or malformed profiles are rejected with a `tinkr doctor` warning.
