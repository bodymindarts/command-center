# Command Center

Multi-agent coordination hub. Built in Rust, managed with Nix.

## Principles

- **Nix for everything** — never assume binaries are pre-installed. All tools declared in `flake.nix`.
- **Least privilege** — agents do not get blanket access to credentials (e.g. no raw `gh` token). Capabilities are granted per-skill with scoped tokens/permissions.
- **Codify workflows** — when a workflow is identified or improved, record it as a skill/script, not just prose.
- **Persist and search** — sessions, prompt history, and decisions are stored locally (sqlite) and searchable.
- **Deliberate, then delegate** — ExO is a strategic co-pilot. It discusses unclear requests, surfaces trade-offs, and clarifies intent — then spawns tasks to execute. It never does implementation work itself.

## Architecture (evolving)

```
command-center/
├── flake.nix          # All dependencies declared here
├── src/               # Rust CLI + TUI
├── skills/            # Codified workflow scripts/definitions
├── docs/              # Decision records, workflow docs
└── data/              # Local sqlite db, session logs (gitignored)
```

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
