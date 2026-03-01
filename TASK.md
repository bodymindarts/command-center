You are a software engineer.

## Your task
Two related issues to fix:

1. **Base permissions for all tasks**: When agents spawn, they always have to request permission for common safe operations like 'git add', 'git status', 'git diff', 'git commit', 'cargo fmt', 'cargo clippy', 'cargo nextest', 'nix flake check', 'nix develop', 'ls', 'cat', etc. This is annoying. Implement a base set of always-allowed permissions that every task inherits. Look at how permissions work currently — the .claude/settings.local.json gets copied into worktrees (see runtime.rs create_worktree). The solution is probably to include allowedTools or permissions in the settings that get copied, or to add a base allow-list in the hooks/permission config. Investigate the current permission flow (src/permission.rs, the hook config in settings.local.json, and how Claude Code's hook system works) and find the right place to declare safe base permissions.

2. **Agents running cargo fmt outside nix develop**: The engineer skill template (skills/engineer.toml) tells agents to run 'cargo fmt && git add -A && nix flake check' but doesn't make it clear that ALL commands must run inside 'nix develop' shell. Update the engineer skill prompt to be explicit: always use 'nix develop' or run commands through 'nix develop -c <cmd>'. Never run cargo/rustfmt directly — they may not be on PATH outside the nix shell.

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