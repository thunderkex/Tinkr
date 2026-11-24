<p align="center">
  <img src="icon.png" alt="Tinkr" width="80" height="80" />
</p>

# Tinkr Companion

Thin status bar and diagnostics companion for the local [Tinkr](https://github.com/thunderkex/Tinkr) binary — works in VS Code, Cursor, Windsurf, VSCodium, Trae, Kiro, and Antigravity IDE.

- **Zero compression logic in the extension** — talks strictly to your local `tinkr` binary.
- **Local-only & private** — no network calls, no telemetry.

## Features

- Live status bar indicator showing overall token reduction (`Tinkr: -61% (9,365 tok)`).
- Hover card with progress bar, exact vs. estimated event counts, per-layer (`Read-Less`, `Send-Less`, `Say-Less`), per-provider, and top-tool breakdowns.
- Instant refresh via file watcher on Tinkr's local data directory.
- Commands:
  - `Tinkr: Doctor` — runs `tinkr doctor --fix` in the Tinkr output channel.
  - `Tinkr: Overlap Status` — runs `tinkr overlap` to inspect coexistence with other optimizers.
  - `Tinkr: Refresh Savings` — refreshes status bar metrics on demand.

## Requirements

Install the local `tinkr` binary first:

- **Windows (Winget):** `winget install thunderkex.Tinkr`
- **macOS / Linux (Homebrew):** `brew install thunderkex/tap/tinkr`
- **Prebuilt binaries:** [GitHub Releases](https://github.com/thunderkex/Tinkr/releases)

Then run `tinkr` once in your terminal to set up your detected AI tools.

## License

Apache-2.0
