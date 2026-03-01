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

    # .claude dir with hooks (spawn copies this into worktrees)
    mkdir -p "$PROJECT_DIR/.claude/hooks"
    cat > "$PROJECT_DIR/.claude/settings.local.json" << 'JSON'
{"hooks":{"PreToolUse":[{"matcher":".*","hooks":[{"type":"command","command":"clat permission gate"}]}]}}
JSON

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

@test "spawn copies .claude hooks into worktree" {
    cd "$PROJECT_DIR"
    clat spawn sym-test --skill noop

    local worktree
    worktree=$(find_worktree sym-test)
    # .claude is a real directory (not symlink) with hooks settings copied
    [ -d "$worktree/.claude" ]
    ! [ -L "$worktree/.claude" ]
    [ -f "$worktree/.claude/settings.local.json" ]
    grep -q '"hooks"' "$worktree/.claude/settings.local.json"
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

# --- permission roundtrip through TUI (socket-based) ---

@test "permission roundtrip through TUI" {
    cd "$PROJECT_DIR"

    # Canonicalize TMPDIR to match what clat's socket_path() does
    export TMPDIR
    TMPDIR="$(cd "$TEST_DIR" && pwd -P)"

    # Remember the initial pane (where we'll run the dashboard)
    local dash_pane
    dash_pane=$(tmux display-message -t test -p '#{pane_id}')

    clat spawn perm-rt --skill noop

    local worktree
    worktree=$(find_worktree perm-rt)
    [ -n "$worktree" ]

    # Switch back to original window/pane and start dashboard via script
    # (avoids tmux send-keys length limits with long nix PATH)
    local clat_bin
    clat_bin=$(command -v clat)
    cat > "$TEST_DIR/run-dash.sh" <<DASHSCRIPT
#!/bin/bash
cd '$PROJECT_DIR'
export TMPDIR='$TMPDIR'
export PATH='$PATH'
exec '$clat_bin' dash 2>'$TEST_DIR/dash-stderr'
DASHSCRIPT
    chmod +x "$TEST_DIR/run-dash.sh"
    tmux select-window -t test:0
    tmux send-keys -t "$dash_pane" "'$TEST_DIR/run-dash.sh'" Enter

    # Wait for socket to appear (check both canonical and original path)
    local sock="$TMPDIR/cc-permissions.sock"
    local found_sock=false
    for i in $(seq 1 40); do
        [ -S "$sock" ] && { found_sock=true; break; }
        sleep 0.5
    done

    if [ "$found_sock" != true ]; then
        echo "Socket not found at: $sock" >&2
        echo "Dashboard stderr:" >&2
        cat "$TEST_DIR/dash-stderr" 2>/dev/null >&2 || true
        echo "Dashboard pane:" >&2
        tmux capture-pane -t "$dash_pane" -p >&2 || true
        echo "Files in TMPDIR:" >&2
        ls -la "$TMPDIR" >&2 || true
    fi
    [ "$found_sock" = true ]

    # Pipe request JSON to gate in background — it connects to socket
    local req_json
    req_json=$(cat <<REQJSON
{"tool_name":"Bash","tool_input":{"command":"echo hi"},"cwd":"$worktree"}
REQJSON
    )
    printf '%s' "$req_json" | TMPDIR="$TMPDIR" clat permission gate > "$TEST_DIR/gate-stdout" &
    local gate_pid=$!

    # Poll until TUI shows the permission prompt
    local found=false
    for i in $(seq 1 20); do
        local capture
        capture=$(tmux capture-pane -t "$dash_pane" -p)
        if echo "$capture" | grep -q "wants to use Bash"; then
            found=true
            break
        fi
        sleep 0.5
    done
    [ "$found" = true ]

    # Approve
    tmux send-keys -t "$dash_pane" y
    sleep 1

    # Gate process should have received response
    wait "$gate_pid"
    grep -q '"allow"' "$TEST_DIR/gate-stdout"

    # Quit TUI
    tmux send-keys -t "$dash_pane" Escape
    sleep 0.3
    tmux send-keys -t "$dash_pane" q
}

# --- send message to agent via CLI ---

@test "send message to agent via clat send" {
    # Replace mock claude with cat (echoes stdin to pane)
    printf '#!/bin/bash\nexec cat\n' > "$TEST_DIR/bin/claude"
    chmod +x "$TEST_DIR/bin/claude"

    cd "$PROJECT_DIR"
    clat spawn msg-test --skill noop

    local short_id
    short_id=$(task_field "substr(id, 1, 8)" msg-test)

    local tmux_pane
    tmux_pane=$(task_field tmux_pane msg-test)
    [ -n "$tmux_pane" ]

    run clat send "$short_id" "hello from dashboard"
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Sent message to msg-test"* ]]

    # Give cat time to echo
    sleep 0.5

    # Capture the agent's pane and verify message appears
    local capture
    capture=$(tmux capture-pane -t "$tmux_pane" -p)
    echo "$capture"
    echo "$capture" | grep -q "hello from dashboard"
}

# --- close lifecycle ---

@test "close marks task as closed and kills tmux window" {
    cd "$PROJECT_DIR"
    clat spawn close-test --skill noop

    local short_id
    short_id=$(task_field "substr(id, 1, 8)" close-test)

    local window_id
    window_id=$(task_field tmux_window close-test)

    # Window exists before close
    tmux list-windows -F '#{window_id}' | grep -q "$window_id"

    run clat close "$short_id"
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Closed task close-test"* ]]

    [ "$(task_field status close-test)" = "closed" ]

    # Window should be gone
    ! tmux list-windows -F '#{window_id}' | grep -q "$window_id"
}

@test "close rejects already completed task" {
    cd "$PROJECT_DIR"
    clat spawn close-done --skill noop

    local task_id
    task_id=$(task_field id close-done)
    clat complete "$task_id" 0

    local short_id
    short_id=$(task_field "substr(id, 1, 8)" close-done)

    run clat close "$short_id"
    [ "$status" -ne 0 ]
    [[ "$output" == *"not 'running'"* ]]
}

@test "list shows active by default, --all shows history" {
    cd "$PROJECT_DIR"
    clat spawn active-one --skill noop
    clat spawn active-two --skill noop

    local task_id
    task_id=$(task_field id active-two)
    clat complete "$task_id" 0

    # Default list should only show active (running) tasks
    run clat list
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"active-one"* ]]
    [[ "$output" != *"active-two"* ]]

    # --all shows everything
    run clat list --all
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"active-one"* ]]
    [[ "$output" == *"active-two"* ]]
}

# --- agent chat history / log ---

@test "spawn records initial prompt in log" {
    cd "$PROJECT_DIR"
    clat spawn log-init --skill noop

    local short_id
    short_id=$(task_field "substr(id, 1, 8)" log-init)

    run clat log "$short_id"
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"PROMPT:"* ]]
    [[ "$output" == *"Task complete"* ]]
}

@test "send records message and log shows it" {
    # Replace mock claude with cat (echoes stdin to pane)
    printf '#!/bin/bash\nexec cat\n' > "$TEST_DIR/bin/claude"
    chmod +x "$TEST_DIR/bin/claude"

    cd "$PROJECT_DIR"
    clat spawn log-send --skill noop

    local short_id
    short_id=$(task_field "substr(id, 1, 8)" log-send)

    clat send "$short_id" "first message"
    clat send "$short_id" "second message"

    run clat log "$short_id"
    echo "$output"
    [ "$status" -eq 0 ]
    [[ "$output" == *"PROMPT:"* ]]
    [[ "$output" == *"YOU:"* ]]
    [[ "$output" == *"first message"* ]]
    [[ "$output" == *"second message"* ]]
}
