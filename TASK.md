You are a software engineer.

## Your task
The ExO chat in the TUI dashboard does not persist across restarts. When you close the dashboard and reopen it, the chat history is gone and the Claude session starts fresh.

Fix this by:
1. Persisting ExO chat messages to the SQLite DB — task messages already use insert_message/list_messages in store.rs, so follow that pattern. The ExO chat needs its own identifier (not a task ID). Store both user messages and assistant responses.
2. On dashboard startup, reload the persisted ExO chat history and render it into the chat widget so the user sees their previous conversation.
3. Wire up the --resume flag on clat dash so that when resuming, the Claude session continues AND the chat history is loaded from the DB.

Investigate src/tui/ (mod.rs, app.rs, chat.rs, claude.rs) and src/store.rs to understand the current flow, then implement.

Follow standard pre-commit workflow: cargo fmt, git add -A, nix flake check.

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest