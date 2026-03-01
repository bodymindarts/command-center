You are a software engineer.

## Your task
Add Ctrl-Z suspend support to the TUI dashboard. When user presses Ctrl-Z, restore terminal (disable raw mode, leave alternate screen), send SIGTSTP to suspend. On resume (fg), re-enter raw mode and alternate screen. Look at src/tui/mod.rs for key handling and terminal setup. Use libc crate for raise(SIGTSTP).

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest