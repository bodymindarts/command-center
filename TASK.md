You are a software engineer.

## Your task
Change the Tab key behavior in the TUI dashboard so it cycles through agents while staying in the chat input (Focus::ChatInput), instead of switching to the task list.

The cycle order: ExO chat -> task 1 detail -> task 2 detail -> ... -> back to ExO chat.

Specifics:

1. When in ChatInput with show_detail=false (ExO chat), Tab should go to the first task's detail view (show_detail=true, select that task, stay in ChatInput).
2. When in ChatInput with show_detail=true, Tab should go to the next task. If at the last task, wrap back to ExO chat (show_detail=false, stay in ChatInput).
3. Each context (ExO and each task) should preserve its own typed input buffer. If the user is typing in ExO and hits Tab, when they Tab back the partial message is still there. Each task also keeps its own buffer. Use a HashMap<String, String> or similar keyed by task ID (and a special key like 'exo' for ExO). On context switch, save current buffer and restore the target's buffer.
4. Replace the existing Tab handler in ChatInput that switches to Focus::TaskList with this cycling behavior.

Look at src/tui/mod.rs for the key handlers and src/tui/app.rs for the App state.

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