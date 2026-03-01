You are a software engineer.

## Your task
There's a bug in the TUI dashboard: after closing a task with `clat close`, the task still appears in the task list in the dashboard. The CLI `clat list` correctly hides it, but the TUI doesn't.

Investigate:
1. Look at how the TUI refreshes its task list — check src/tui/mod.rs and src/tui/app.rs for how tasks are fetched and stored
2. Look at how `clat close` marks a task (src/store.rs) — does it set a status or delete the row?
3. Look at how `clat list` filters tasks — it correctly hides closed tasks
4. The TUI likely queries tasks differently from the CLI list command and isn't filtering out closed tasks, OR the TUI caches the task list and doesn't re-query after close
5. Fix the TUI to stop showing closed tasks
6. Run: cargo fmt, git add -A, nix flake check. Fix any issues. Commit with a conventional commit message.

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest