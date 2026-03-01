#!/usr/bin/env bats
# Tests the bash hook <-> Rust TUI IPC glue:
# hook writes .req, blocks for .resp, outputs JSON, cleans up.

HOOK="$HOOK_DIR/permission-gate.sh"

setup() {
    TEST_DIR="$(mktemp -d)"
    PERM_DIR="$TEST_DIR/cc-permissions"
    mkdir -p "$PERM_DIR"
}

teardown() {
    rm -rf "$TEST_DIR"
}

wait_for_req() {
    local attempts=0
    REQ_FILE=""
    while [ $attempts -lt 30 ]; do
        REQ_FILE=$(find "$PERM_DIR" -name "*.req" 2>/dev/null | head -1)
        [ -n "$REQ_FILE" ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    return 1
}

@test "hook creates .req, returns allow response, cleans up" {
    local input='{"tool":{"name":"Bash","input":{"command":"cargo test"}},"cwd":"/home/user/project"}'

    echo "$input" | TMPDIR="$TEST_DIR" CC_PERM_TIMEOUT=25 bash "$HOOK" > "$TEST_DIR/stdout" &
    local pid=$!

    wait_for_req

    # .req content matches input
    [ "$(jq -r '.tool.name' "$REQ_FILE")" = "Bash" ]
    [ "$(jq -r '.tool.input.command' "$REQ_FILE")" = "cargo test" ]
    [ "$(jq -r '.cwd' "$REQ_FILE")" = "/home/user/project" ]

    # Simulate TUI writing allow response
    local req_id
    req_id=$(basename "$REQ_FILE" .req)
    echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}' \
        > "$PERM_DIR/$req_id.resp"

    wait "$pid"

    # Hook forwarded the response to stdout
    [ "$(jq -r '.hookSpecificOutput.decision.behavior' "$TEST_DIR/stdout")" = "allow" ]

    # Hook cleaned up both files
    [ ! -f "$PERM_DIR/$req_id.req" ]
    [ ! -f "$PERM_DIR/$req_id.resp" ]
}

@test "hook forwards deny response" {
    local input='{"tool":{"name":"Write","input":{"file_path":"/etc/passwd"}},"cwd":"/dangerous"}'

    echo "$input" | TMPDIR="$TEST_DIR" CC_PERM_TIMEOUT=25 bash "$HOOK" > "$TEST_DIR/stdout" &
    local pid=$!

    wait_for_req

    local req_id
    req_id=$(basename "$REQ_FILE" .req)
    echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny"}}}' \
        > "$PERM_DIR/$req_id.resp"

    wait "$pid"

    [ "$(jq -r '.hookSpecificOutput.decision.behavior' "$TEST_DIR/stdout")" = "deny" ]
}

@test "hook denies on timeout and cleans up .req" {
    local input='{"tool":{"name":"Bash","input":{"command":"echo timeout"}},"cwd":"/tmp"}'

    # 2 polls * 200ms = ~0.4s timeout
    run env TMPDIR="$TEST_DIR" CC_PERM_TIMEOUT=2 bash "$HOOK" <<< "$input"

    [ "$status" -eq 0 ]
    [ "$(echo "$output" | jq -r '.hookSpecificOutput.decision.behavior')" = "deny" ]
    [ "$(echo "$output" | jq -r '.hookSpecificOutput.decision.message')" = "Timed out waiting for approval" ]

    # .req was cleaned up even on timeout
    [ "$(find "$PERM_DIR" -name '*.req' 2>/dev/null | wc -l)" -eq 0 ]
}
