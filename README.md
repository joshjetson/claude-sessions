<p align="center">
  <img src="assets/banner.svg" alt="claude-sessions — a live terminal dashboard for your Claude Code sessions" width="900"/>
</p>

<p align="center">
  <a href="https://github.com/joshjetson/claude-sessions/actions/workflows/ci.yml"><img src="https://github.com/joshjetson/claude-sessions/actions/workflows/ci.yml/badge.svg" alt="CI"/></a>
  <a href="https://crates.io/crates/claude-sessions"><img src="https://img.shields.io/crates/v/claude-sessions.svg?color=d97f4a&label=crates.io" alt="crates.io"/></a>
</p>

<p align="center">
  <strong>A live terminal dashboard for your Claude Code sessions</strong><br/>
  Watch every session on your machine — status, context usage, and the conversation as it happens.
  Attach, prompt, and kill sessions without leaving the dashboard. Wire in a task board,
  QA pipelines, and deploy tracking when you want the full workflow.
</p>

```text
┌ Sessions ────────────────────────┐┌ Conversation ───────────────┐
│ ▼ 📁 dev/portal   3 sessions     ││ Claude:                     │
│     ● 8f3a  working  140K (70%)  ││ Here's the plan…            │
│     ● 514e  idle      51K (26%)  ││                             │
│ ▼ 📁 dev/api      1 session      ││ > tests passing, opening    │
│     ● c2d1  blocked   88K (44%)  ││   the merge request next    │
└──────────────────────────────────┘└─────────────────────────────┘
```

---

## Index

- [Overview](#overview)
- [Quick Start](#quick-start)
- [Features](#features)
- [Subcommands](#subcommands)
- [Configuration](#configuration)
- [Terminal Drivers](#terminal-drivers)
- [Claude Code Hooks](#claude-code-hooks)
- [Architecture](#architecture)
- [Building from Source](#building-from-source)
- [Porting Status](#porting-status)
- [Contributing](#contributing)
- [License](#license)

---

## Overview

`claude-sessions` discovers every [Claude Code](https://docs.claude.com/en/docs/claude-code)
session running on your machine and renders them as a live dashboard: which project each
session belongs to, whether it is working, idle, or blocked waiting on you, how much of its
context window is spent, and the conversation itself streaming in a side pane.

It is a ground-up Rust rewrite of an internal Node/Ink tool, built on
[ratatui](https://ratatui.rs). The rewrite exists for one reason: the original spent a React
runtime re-rendering a terminal — this one is a single static binary that starts instantly,
idles at ~zero CPU, and installs with one command.

Beyond watching sessions, it can run your delivery workflow end to end:

- **Sessions** — monitor, attach to, prompt, and kill local Claude Code sessions
- **Board** — an Odoo task board: spawn a session against a task, track it to done
- **Pipelines** — QA pipelines with a viewer to watch runs as they happen
- **Deploy** — per-project deploy definitions and one-key shipping
- **Daemon** — a background watcher that fires alerts (sound, notification) the moment
  a session needs you

The Sessions surface works standalone with zero configuration. The Board, Pipelines, and
Deploy surfaces light up when you point the config at your Odoo instance and GitLab host.

## Quick Start

```bash
# From crates.io — no --locked needed; a fresh dependency resolve
# is CI-tested on every push so a bare install always builds
cargo install claude-sessions

# The dashboard
claude-sessions

# The background watcher (alerts when a session needs you)
claude-sessions daemon
```

> Publishing to crates.io happens at 1.0 — until then, [build from source](#building-from-source).

**Requirements:** the `claude` CLI on `PATH`. macOS with [iTerm2](https://iterm2.com) or
[tmux](https://github.com/tmux/tmux) for full terminal control; with the tmux driver the
session features work anywhere tmux runs, including over SSH.

## Features

- **Live session discovery** — scans your project folders and Claude Code's transcript
  directory; new sessions appear the moment they start
- **Context meter** — per-session token usage and % of context window, updated live
- **Conversation pane** — the session's transcript rendered as it streams, with
  syntax-highlighted code blocks
- **Session control** — focus the session's terminal, send it a prompt, or kill it,
  from the dashboard
- **Status at a glance** — working / idle / blocked, surfaced per session and per project
- **Task board** — Odoo-backed board with configurable stage/state filters; start a task
  and the dashboard spawns a branch, a worktree, and a Claude Code session wired to it
- **QA pipelines** — definitions, live runs, and a viewer tab
- **Deploy tracking** — opt projects into a Deploy tab that knows how each one ships
- **Alerts** — sound and notification when a session finishes or blocks, via the daemon
- **Themes** — multiple color themes, honoring your terminal's palette

## Subcommands

One binary, one install. The Node original shipped seven executables; they are now
subcommands:

| Command | What it does |
|---|---|
| `claude-sessions` | The dashboard TUI |
| `claude-sessions daemon` | Background watcher: session state, alerts, notifications |
| `claude-sessions notify` | Hook endpoint — fire a notification for the calling session |
| `claude-sessions done` | Hook endpoint — mark the calling session's work finished |
| `claude-sessions blocked` | Hook endpoint — mark the calling session blocked on input |
| `claude-sessions journal` | Journal viewer |
| `claude-sessions pipeline` | Pipeline viewer |

## Configuration

All configuration lives in `~/.claude-sessions.json` and every key is optional — sensible
defaults apply, and existing config files from the Node version work unchanged.

```jsonc
{
  "groups": [{ "name": "Dev", "path": "~/dev" }],          // folders scanned for projects
  "odoo": { "url": "", "db": "", "user": "", "password": "" },
  "board": { "include": [], "ignore": [], "hideStages": ["Deployed"] },
  "odooProjectDirs": {},                                    // Odoo project → local repos
  "deploy": { "projects": {} },                             // opt-in Deploy tab
  "chat": {}                                                // conversation pane theme/width
}
```

Environment fallbacks: `ODOO_URL`, `ODOO_DB`, `ODOO_USER`, `ODOO_PASSWORD`.

## Terminal Drivers

Terminal control goes through a pluggable driver:

- **iTerm2** — AppleScript-driven; launch, focus, and prompt sessions in real iTerm2 panes
  (macOS will ask permission to control iTerm2 on first launch — allow it)
- **tmux** — works anywhere tmux runs, including over SSH

## Claude Code Hooks

The `notify` / `done` / `blocked` subcommands are designed to be called from
[Claude Code hooks](https://docs.claude.com/en/docs/claude-code/hooks), so sessions report
their own state transitions to the dashboard:

```json
{
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "claude-sessions done" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "claude-sessions notify" }] }]
  }
}
```

## Architecture

```text
                    ┌────────────────────────┐
  transcripts ────▶ │  scanner + parser      │
  (~/.claude)       │  (incremental JSONL)   │
                    └───────────┬────────────┘
                                ▼
  ┌──────────────┐    ┌────────────────────┐    ┌─────────────────┐
  │ daemon        │◀──▶│  state store       │───▶│  ratatui UI     │
  │ alerts/hooks  │    │  (single source    │    │  tabs, dialogs, │
  └──────────────┘    │   of truth)        │    │  themes         │
                       └───────┬────────────┘    └─────────────────┘
                               ▼
                  Odoo (RPC) · GitLab · tmux/iTerm2 drivers
```

Design rules the codebase holds itself to:

- **DRY, no god files** — one responsibility per module; behavior variants are parameters,
  not near-duplicate functions
- **Incremental everything** — transcripts are tailed from a byte offset, never re-parsed
  from the top; lookups are keyed, not scanned
- **The UI never blocks** — I/O lives on worker threads; the render loop only draws state

## Building from Source

Requires Rust 1.85+.

```bash
git clone https://github.com/joshjetson/claude-sessions.git
cd claude-sessions
cargo build --release        # binary at target/release/claude-sessions
cargo test
```

## Porting Status

This is an active rewrite of a battle-tested internal tool. Surfaces land in order:

- [x] CLI scaffold — one binary, subcommand layout
- [ ] Session scanner + transcript parser
- [ ] Dashboard: sessions list + conversation pane + themes
- [ ] Terminal drivers (tmux, iTerm2)
- [ ] Daemon + `notify` / `done` / `blocked` hooks
- [ ] Odoo task board + task pipeline
- [ ] QA pipelines + viewer
- [ ] Deploy tab
- [ ] Journal, daily log, usage
- [ ] 1.0 on crates.io

## Contributing

Issues and PRs welcome. `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` must
pass; CI also builds with a fresh dependency resolve (no lockfile) so `cargo install`
never needs `--locked`.

## License

MIT — see [LICENSE](LICENSE).
