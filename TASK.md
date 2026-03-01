You are a software engineer.

## Your task
Two keybinding fixes in the TUI dashboard (src/tui/mod.rs and possibly src/tui/widgets.rs):

1. **Ctrl+P** — Currently it focuses the task list and highlights the correct task row. It should ALSO open that task's chat detail view, as if the user pressed Enter after selecting it. So Ctrl+P should: focus the task pane, select the correct task row, AND set show_detail = true / focus = Focus::ChatInput so the user lands in the agent's chat ready to type.

2. **Ctrl+E** — Should ALWAYS bring the user back to the ExO chat. That means: set show_detail = false and focus = Focus::ChatInput. This should work from ANY focus state (TaskList, ChatInput with detail open, etc.).

Look at how Focus, show_detail, and the Ctrl+P handler currently work to understand the flow. The key handlers are in the main event loop in src/tui/mod.rs.

Run 'cargo fmt', 'git add -A', then 'nix flake check' before committing.

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