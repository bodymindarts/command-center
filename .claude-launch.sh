#!/bin/sh
unset CLAUDECODE
exec /nix/store/0bw198999qh3q8ywxgp8zlbskabwiggi-claude-code-2.1.63/bin/claude "$(cat .claude-prompt.txt)" --system-prompt "$(cat .claude-system-prompt.txt)"