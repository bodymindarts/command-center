#!/bin/bash
# Only gate permissions when the dashboard socket is active.
# Without the socket, output nothing — Claude uses its normal permission flow.
SOCK="${TMPDIR:-/tmp}/cc-permissions.sock"
[ -S "$SOCK" ] && exec clat permission gate
exit 0
