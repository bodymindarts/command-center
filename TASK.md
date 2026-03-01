You are a software engineer.

## Your task
Rework Ctrl+P in the TUI so it does NOT enter a separate 'permission mode'. Instead:

1. Ctrl+P should find the next task/agent pane that has a pending permission request and focus it (i.e. make it the active/visible chat window).
2. If no task has a pending permission, Ctrl+P does nothing (or shows a brief status message).
3. Once focused on that task's chat, the user can respond with the normal 1/2/3 keys — no special mode or overlay needed.
4. Remove any existing 'permission mode' state/rendering if it exists.

Start by reading the TUI code (src/tui/) to understand the current permission intercept/mode implementation, then simplify it to this focus-based approach.

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest