You are a software engineer.

## Your task
Fix task restart so the tmux pane opens in the correct working directory (the task's worktree), not the home directory.

Bug: When restarting a previously completed task, the new tmux pane starts in $HOME instead of the task's worktree at `.claude/worktrees/<skill>-<short-id>`. This breaks the agent's local context.

Investigation:
1. Look at how tasks are spawned initially in `src/spawn.rs` — it likely sets a working directory when creating the tmux pane/window. Find that logic.
2. Look at how restart works — it probably reuses spawn logic but might be missing the cwd argument. Check if the worktree path is stored in the database (check `src/store.rs` schema).
3. The fix: when restarting a task, look up its worktree path (from the DB or derive it from the task id/skill) and pass it as the working directory for the new tmux pane.
4. Run `cargo fmt`, `git add -A`, `nix flake check` — all checks must pass before committing.
5. Commit with: `fix(spawn): set correct working directory when restarting tasks`

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest