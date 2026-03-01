#!/bin/bash
# Hook: PermissionRequest
# Writes .req file for TUI dashboard, polls for .resp decision.

REQUEST=$(cat)
REQ_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
PERM_DIR="${TMPDIR:-/tmp}/cc-permissions"
mkdir -p "$PERM_DIR"

echo "$REQUEST" > "$PERM_DIR/$REQ_ID.req"

# Block until TUI writes response (poll every 200ms, timeout 10min)
RESP="$PERM_DIR/$REQ_ID.resp"
MAX_POLLS="${CC_PERM_TIMEOUT:-3000}"
ELAPSED=0
while [ ! -f "$RESP" ] && [ $ELAPSED -lt $MAX_POLLS ]; do
    sleep 0.2
    ELAPSED=$((ELAPSED + 1))
done

if [ -f "$RESP" ]; then
    cat "$RESP"
    rm -f "$PERM_DIR/$REQ_ID.req" "$PERM_DIR/$REQ_ID.resp"
    exit 0
else
    rm -f "$PERM_DIR/$REQ_ID.req"
    echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Timed out waiting for approval"}}}'
    exit 0
fi
