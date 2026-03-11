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

3. **Set environment variables** before launching `clat start`:
   ```sh
   export TELEGRAM_BOT_TOKEN="<your-bot-token>"
   export TELEGRAM_CHAT_ID="<your-chat-id>"
   ```

When both variables are set, the bot activates automatically on `clat start`. If they are absent, the feature is completely dormant.

### Voice Messages (Optional)

The bot can transcribe voice messages and route them to ExO. This requires `ffmpeg` and `whisper-cli` on `PATH`. The whisper GGML model is auto-downloaded on first use to `data/ggml-base.bin` (override with `WHISPER_MODEL`).
