#!/usr/bin/env bats
# Tests the socket-based permission IPC:
# clat permission gate reads stdin, connects to socket, prints response.
# When no socket exists and no tmux, auto-denies.

setup() {
    TEST_DIR="$(mktemp -d)"
    # Canonicalize to avoid macOS /var vs /private/var issues
    TEST_DIR="$(cd "$TEST_DIR" && pwd -P)"
    export TMPDIR="$TEST_DIR"
    SOCK="$TEST_DIR/cc-permissions.sock"

    # Prevent popup fallback from triggering tmux display-popup during tests
    unset TMUX
}

teardown() {
    rm -rf "$TEST_DIR"
}

# Helper: start a minimal socket server that accepts one connection,
# reads the request, and writes a fixed response.
start_mock_server() {
    local response="$1"
    (
        python3 -c "
import socket, sys, os
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.bind('$SOCK')
sock.listen(1)
conn, _ = sock.accept()
data = b''
while True:
    chunk = conn.recv(4096)
    if not chunk:
        break
    data += chunk
conn.sendall(sys.stdin.buffer.read())
conn.close()
sock.close()
" <<< "$response"
    ) &
    SERVER_PID=$!
    # Wait for socket to appear
    local attempts=0
    while [ ! -S "$SOCK" ] && [ $attempts -lt 30 ]; do
        sleep 0.1
        attempts=$((attempts + 1))
    done
}

@test "gate: socket approval returns allow response" {
    local input='{"tool":{"name":"Bash","input":{"command":"cargo test"}},"cwd":"/home/user/project"}'
    local allow_resp='{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'

    start_mock_server "$allow_resp"

    run bash -c "echo '$input' | TMPDIR='$TMPDIR' clat permission gate"
    [ "$status" -eq 0 ]
    [ "$(echo "$output" | jq -r '.hookSpecificOutput.decision.behavior')" = "allow" ]

    wait "$SERVER_PID" 2>/dev/null || true
}

@test "gate: socket denial returns deny response" {
    local input='{"tool":{"name":"Write","input":{"file_path":"/tmp/test-fakefile.txt"}},"cwd":"/home/user/project"}'
    local deny_resp='{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny"}}}'

    start_mock_server "$deny_resp"

    run bash -c "echo '$input' | TMPDIR='$TMPDIR' clat permission gate"
    [ "$status" -eq 0 ]
    [ "$(echo "$output" | jq -r '.hookSpecificOutput.decision.behavior')" = "deny" ]

    wait "$SERVER_PID" 2>/dev/null || true
}

@test "gate: no socket + no tmux auto-denies" {
    local input='{"tool":{"name":"Bash","input":{"command":"echo timeout"}},"cwd":"/tmp"}'

    run bash -c "echo '$input' | TMPDIR='$TMPDIR' clat permission gate"
    [ "$status" -eq 0 ]
    [ "$(echo "$output" | jq -r '.hookSpecificOutput.decision.behavior')" = "deny" ]
    [ "$(echo "$output" | jq -r '.hookSpecificOutput.decision.message')" = "No approval UI available" ]
}
