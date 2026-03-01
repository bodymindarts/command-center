#!/usr/bin/env bats
# Tests the clat spawn → worktree → tmux pipeline glue.
# Requires: clat, tmux, git, sqlite3 (all provided by nix)
#
# Isolation: a tmux wrapper forces ALL tmux commands (including those
# inside clat) through a dedicated -L socket, so the user's real
# tmux session is never touched.

setup() {
    TEST_DIR="$(mktemp -d)"
    TMUX_SOCKET="cc-e2e-$$"

    # Resolve real tmux before we shadow it
    REAL_TMUX="$(command -v tmux)"

    # --- tmux wrapper: forces -L $TMUX_SOCKET on every call ---
    mkdir -p "$TEST_DIR/bin"
    cat > "$TEST_DIR/bin/tmux" <<WRAPPER
#!/bin/bash
exec "$REAL_TMUX" -L "$TMUX_SOCKET" "\$@"
WRAPPER
    chmod +x "$TEST_DIR/bin/tmux"

    # Mock claude binary (spawn_agent resolves it via `which`)
    printf '#!/bin/bash\nexec sleep infinity\n' > "$TEST_DIR/bin/claude"
    chmod +x "$TEST_DIR/bin/claude"

    # Wrapper + mock must come first in PATH
    export PATH="$TEST_DIR/bin:$PATH"

    # Clean slate: kill leftover server from a crashed previous test
    unset TMUX
    tmux kill-server 2>/dev/null || true
    tmux new-session -d -s test -x 200 -y 50

    # clat checks TMUX to verify it's inside tmux — value doesn't matter
    # because the wrapper handles server targeting via -L
    export TMUX="isolated,0,0"

    # --- Minimal project structure (needed by find_project_root) ---
    PROJECT_DIR="$TEST_DIR/project"
    mkdir -p "$PROJECT_DIR/skills" "$PROJECT_DIR/data"

    cat > "$PROJECT_DIR/Cargo.toml" << 'TOML'
[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
TOML

    cat > "$PROJECT_DIR/skills/noop.toml" << 'TOML'
[skill]
name = "noop"
description = "test"
params = []

[agent]
allowed_tools = ["Bash"]

[template]
prompt = "Say 'Task complete.' and nothing else."
TOML

    # Git repo (needed for worktree creation)
    git -C "$PROJECT_DIR" init -q
    git -C "$PROJECT_DIR" -c user.name="Test" -c user.email="test@test.com" add -A
    git -C "$PROJECT_DIR" -c user.name="Test" -c user.email="test@test.com" commit -q -m "init"
}

teardown() {
    tmux kill-server 2>/dev/null || true
    rm -rf "$TEST_DIR"
}

find_worktree() {
    find "$PROJECT_DIR/.claude/worktrees" -maxdepth 1 -name "$1-*" -type d | head -1
}

task_field() {
    sqlite3 "$PROJECT_DIR/data/cc.db" "SELECT $1 FROM tasks WHERE name='$2' LIMIT 1"
}

# --- spawn glue ---

@test "spawn creates worktree and git branch" {
    cd "$PROJECT_DIR"
    run clat spawn wt-test --skill noop
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Spawned task wt-test"* ]]

    local worktree
    worktree=$(find_worktree wt-test)
    [ -n "$worktree" ]

    local branch="task/$(basename "$worktree")"
    git -C "$PROJECT_DIR" branch --list "$branch" | grep -q "task/"
}

@test "spawn writes TASK.md with rendered prompt" {
    cd "$PROJECT_DIR"
    clat spawn prompt-test --skill noop

    local worktree
    worktree=$(find_worktree prompt-test)
    [ -f "$worktree/TASK.md" ]
    grep -q "Task complete" "$worktree/TASK.md"
}

@test "spawn symlinks .claude into worktree" {
    cd "$PROJECT_DIR"
    clat spawn sym-test --skill noop

    local worktree
    worktree=$(find_worktree sym-test)
    [ -L "$worktree/.claude" ]
    # Symlink target is a valid directory (skip literal path compare — macOS
    # resolves /tmp → /private/tmp which makes paths mismatch)
    [ -d "$worktree/.claude" ]
}

@test "spawn creates tmux window with 3 panes" {
    cd "$PROJECT_DIR"
    clat spawn pane-test --skill noop

    tmux list-windows -F '#{window_name}' | grep -q 'cc:pane-test'

    local window_id
    window_id=$(tmux list-windows -F '#{window_id} #{window_name}' | awk '/cc:pane-test/ {print $1}')
    local pane_count
    pane_count=$(tmux list-panes -t "$window_id" | wc -l | tr -d ' ')
    [ "$pane_count" -eq 3 ]
}

# --- list / complete / goto ---

@test "list shows spawned task as running" {
    cd "$PROJECT_DIR"
    clat spawn list-test --skill noop

    run clat list
    [ "$status" -eq 0 ]
    [[ "$output" == *"list-test"* ]]
    [[ "$output" == *"noop"* ]]
    [[ "$output" == *"running"* ]]
}

@test "complete marks task as completed" {
    cd "$PROJECT_DIR"
    clat spawn done-test --skill noop

    local task_id
    task_id=$(task_field id done-test)

    run clat complete "$task_id" 0
    [ "$status" -eq 0 ]

    [ "$(task_field status done-test)" = "completed" ]
}

@test "complete with nonzero exit marks task as failed" {
    cd "$PROJECT_DIR"
    clat spawn fail-test --skill noop

    local task_id
    task_id=$(task_field id fail-test)

    run clat complete "$task_id" 1
    [ "$status" -eq 0 ]

    [ "$(task_field status fail-test)" = "failed" ]
}

@test "goto switches to task window" {
    cd "$PROJECT_DIR"
    clat spawn goto-test --skill noop

    local short_id
    short_id=$(task_field "substr(id, 1, 8)" goto-test)

    run clat goto "$short_id"
    [ "$status" -eq 0 ]

    local active_window
    active_window=$(tmux display-message -t test -p '#{window_name}')
    [ "$active_window" = "cc:goto-test" ]
}
