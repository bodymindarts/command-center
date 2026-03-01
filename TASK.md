You are a software engineer.

## Your task
Simplify the Ctrl+P permission handling in the TUI dashboard.

Current behavior (broken): Ctrl+P focuses the task awaiting permissions but enters some special input mode that expects the user to press 1/2 keys directly.

Desired behavior: Ctrl+P should simply focus the task pane that is awaiting permissions and put the cursor in the normal chat input. No special permission mode, no special key handling. The user just types '1' or '2' (or whatever) as a regular chat message and sends it with Enter.

Investigation:
1. Look at `src/tui/app.rs` (and other tui modules) for the Ctrl+P handler — there was a recent refactor in commit 3b0cae7 ('replace permission mode with Ctrl+P focus-based approach'). Read that code to understand what it currently does.
2. Remove any special permission input handling — Ctrl+P should just: (a) find the task that's awaiting permissions, (b) focus/select that task's pane, (c) put focus in the chat input. That's it. Normal chat mode from there.
3. Run `cargo fmt`, `git add -A`, `nix flake check` — all checks must pass before committing.
4. Commit with: `fix(tui): simplify Ctrl+P to just focus task pane in normal chat mode`

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest