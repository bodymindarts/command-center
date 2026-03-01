You are a software engineer.

## Your task
Add a `clat delete <id>` CLI command that fully deletes a task from the database (not just closes it — removes the row entirely).

Investigation steps:
1. Look at how the dashboard's backspace handler deletes tasks — check `src/tui/app.rs` or similar for the keypress handler. It likely calls something on the store.
2. Look at the existing `clat close` CLI command in `src/cli.rs` as a pattern for adding the new `delete` subcommand.
3. Check `src/store.rs` for any existing delete function. If there isn't one, add a `delete_task` method that does `DELETE FROM tasks WHERE id = ?`.
4. Wire up the new CLI subcommand to call that store method.
5. Run `cargo fmt`, `git add -A`, `nix flake check` — all checks must pass before committing.
6. Commit with message: `feat(cli): add `clat delete` command for hard-deleting tasks`

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest