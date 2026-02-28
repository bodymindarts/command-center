# Command Center

Multi-agent coordination hub. Built in Rust, managed with Nix.

## Principles

- **Nix for everything** — never assume binaries are pre-installed. All tools declared in `flake.nix`.
- **Least privilege** — agents do not get blanket access to credentials (e.g. no raw `gh` token). Capabilities are granted per-skill with scoped tokens/permissions.
- **Codify workflows** — when a workflow is identified or improved, record it as a skill/script, not just prose.
- **Persist and search** — sessions, prompt history, and decisions are stored locally (sqlite) and searchable.

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
