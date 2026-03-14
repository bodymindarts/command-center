# Command Center

Multi-agent coordination hub. Built in Rust, managed with Nix.

## Principles

- **Nix for everything** — never assume binaries are pre-installed. All tools declared in `flake.nix`.
- **Least privilege** — agents do not get blanket access to credentials (e.g. no raw `gh` token). Capabilities are granted per-skill with scoped tokens/permissions.
- **Codify workflows** — when a workflow is identified or improved, record it as a skill/script, not just prose.
- **Persist and search** — sessions, prompt history, and decisions are stored locally (sqlite) and searchable.
- **Three-tier hierarchy** — User → ExO → PM(s) → Tasks. ExO is a strategic co-pilot that creates projects and briefs PMs. PMs are autonomous executors that spawn and coordinate agents. Agents do the work.

## Architecture (evolving)

```
command-center/
├── flake.nix          # All dependencies declared here
├── src/               # Rust CLI + TUI
├── skills/            # Codified workflow scripts/definitions
├── docs/              # Decision records, workflow docs
└── data/              # Local sqlite db, session logs (gitignored)
```

## Workflow Hierarchy

```
User → ExO → PM(s) → Tasks
```

- **ExO** creates projects and briefs PMs on goals. For simple one-off fixes, spawns tasks directly.
- **PM** autonomously breaks down work, spawns agents (engineer, researcher, reviewer, monitor, reporter, security-auditor), coordinates them, and delivers results.
- **Tasks** run in git worktrees with a specific skill. Task descriptions must be self-contained.

Choose the skill based on the task's nature: `engineer` (commits code), `researcher` (explores, no commits), `reviewer` (audits code), `monitor` (watches conditions), `reporter` (generates reports), `security-auditor` (security review).

## Dev Shell

```sh
nix develop   # or direnv allow
```

## Standard Workflow

Before committing, always:

```sh
cargo fmt                        # fix formatting
git add -A                       # stage everything (nix flake only sees tracked files)
nix flake check                  # runs fmt, clippy (--deny warnings), nextest
```

**Checks must pass before any commit.**

Commit messages must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<optional scope>): <description>
```

Common types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `ci`.

Individual checks during development (inside `nix develop`):

```sh
cargo fmt --check                          # formatting
cargo clippy --all-targets -- -D warnings  # lints
cargo nextest run                          # tests
```

All check tooling is declared in `flake.nix` via crane — never run checks outside the nix shell.
