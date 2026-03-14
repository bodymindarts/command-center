#!/usr/bin/env bash
set -euo pipefail

MCP_URL="${1:-http://localhost:9111/mcp}"

mcp() {
  curl -sf "$MCP_URL" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "$1"
}

echo "=== Initialize ==="
mcp '{
  "jsonrpc": "2.0", "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "2025-03-26",
    "capabilities": {},
    "clientInfo": {"name": "test", "version": "0.1"}
  }
}' | jq .

echo ""
echo "=== List Tools ==="
mcp '{
  "jsonrpc": "2.0", "id": 2,
  "method": "tools/list",
  "params": {}
}' | jq .

echo ""
echo "=== Spawn Test Task ==="
mcp '{
  "jsonrpc": "2.0", "id": 3,
  "method": "tools/call",
  "params": {
    "name": "clat_spawn",
    "arguments": {
      "name": "mcp-test",
      "task": "Say hello and exit"
    }
  }
}' | jq .
