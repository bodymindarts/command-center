# Command Center

Multi-agent coordination hub. Spawn, monitor, and interact with AI agent tasks from the terminal.

## Getting Started

### Prerequisites

- [Nix](https://nixos.org/download/) with flakes enabled

### Setup

```sh
git clone <repo-url>
cd command-center
nix develop    # or: direnv allow
cargo build
```

The binary is called `clat` and is available on `PATH` inside the dev shell via the wrapper at `bin/clat`.

## Quick Usage

```sh
clat start                              # launch the ExO workspace (tmux + TUI dashboard)
clat spawn "fix-bug" -p task="..."      # spawn an engineer task
clat list                               # list active tasks
clat dash                               # open the TUI dashboard
```

### Dashboard

The TUI dashboard is a split-pane interface: chat on the left (65%), task list on the right (35%). Launch it standalone or as part of the ExO workspace:

```sh
clat start                       # ExO workspace (tmux session + dashboard)
clat start --caffeinate          # same, but prevent macOS sleep while running
clat dash                        # standalone dashboard (use this from within a running tmux)
clat dash --caffeinate           # same, but prevent macOS sleep while running
```

The `--caffeinate` flag spawns `caffeinate -s` in the background so long-running agent sessions aren't interrupted by system sleep.

**Global shortcuts** (work everywhere):

| Key | Action |
|-----|--------|
| `Ctrl+C` | Quit |
| `Ctrl+Z` | Suspend (fg to resume) |
| `Ctrl+O` | Switch to ExO chat |
| `Ctrl+R` | Switch to project PM (cycles projects) |
| `Ctrl+P` | Cycle to next task with pending permissions |

**Task list** (right panel focused):

| Key | Action |
|-----|--------|
| `j`/`k` or `↑`/`↓` | Navigate tasks |
| `Enter` | Open task detail |
| `Esc` | Close detail |
| `x` | Close task |
| `r` | Reopen task |
| `Backspace` | Delete task |
| `/` | Search tasks |
| `p` | Show project list |
| `Tab` | Focus chat panel |
| `Ctrl+G` | Jump to task's tmux window |

**Permission prompts** (shown in task detail):

| Key | Action |
|-----|--------|
| `Ctrl+Y` | Approve (one-time) |
| `Ctrl+T` | Trust (always-allow) |
| `Ctrl+N` | Deny |
| `1`–`4` | Answer an AskUser prompt |

**Chat input**:

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Esc` | Cancel streaming |
| `Ctrl+K` | Focus chat history |
| `Ctrl+L` | Focus task list |
| `Tab`/`Shift+Tab` | Navigate between tasks |

### Skills

Tasks run with a skill that determines the agent's role. Pass `-s <skill>` to `clat spawn`:

- `engineer` (default) -- implementation, bug fixes, features
- `researcher` -- research and exploration, no code commits
- `reviewer` -- code review and audits
- `reporter` -- status reports and summaries
- `security-auditor` -- security review

```sh
clat spawn "investigate-auth" -s researcher -p task="..."
clat spawn "review-pr-42" -s reviewer -p task="..."
```

## Telegram Integration (Optional)

The TUI dashboard can optionally forward permission requests and ExO messages to a Telegram chat, letting you approve agent actions and send messages remotely.

### Setup

1. **Create a bot** -- message [@BotFather](https://t.me/BotFather) on Telegram, run `/newbot`, and copy the bot token.

2. **Get your chat ID** -- send any message to your new bot, then fetch:
   ```
   https://api.telegram.org/bot<YOUR_TOKEN>/getUpdates
   ```
   Your chat ID is in `result[0].message.chat.id`.

3. **Set environment variables** — add them to `.env` in the project root (already gitignored):
   ```sh
   TELEGRAM_BOT_TOKEN="<your-bot-token>"
   TELEGRAM_CHAT_ID="<your-chat-id>"
   ```

When both variables are set, the bot activates automatically on `clat start`. If they are absent, the feature is completely dormant.

### Voice Messages

The bot can transcribe Telegram voice messages and route them to ExO. The required tools (`ffmpeg`, `whisper-cli`) are provided by the dev shell. The whisper GGML model is auto-downloaded on first use to `data/ggml-base.bin` (override with `WHISPER_MODEL`).
