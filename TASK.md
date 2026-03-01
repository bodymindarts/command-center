You are a software engineer.

## Your task
Add a '#' column as the FIRST column in both the TUI task list (src/tui/widgets.rs) and the CLI 'clat list' table (src/main.rs). This column should show the tmux WINDOW NUMBER (not the window ID like @24, but the actual index number — e.g. 0, 1, 2 — that you use with ctrl-b to switch windows). The window number can be obtained by running 'tmux list-windows -F "#{window_id} #{window_index}"' and mapping the stored window ID (@24 etc) to its index. The data for window ID is already stored in the tasks table as 'window'. Look at how the existing Pane column was added for reference. Keep it minimal — just add the column and the lookup.

## Workflow
- Read and understand existing code before making changes
- **All commands must run inside the nix shell.** Either:
  - Prefix one-off commands: `nix develop -c cargo fmt`, `nix develop -c cargo clippy --all-targets -- -D warnings`
  - Or wrap the full pre-commit sequence: `nix develop -c sh -c 'cargo fmt && git add -A && nix flake check'`
- **Never run cargo, rustfmt, or clippy directly** — they may not be on PATH outside `nix develop`
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest
- `git add -A` must happen before `nix flake check` (nix flakes only copy tracked files)