#!/bin/bash
# PreToolUse hook: forward events to dashboard + auto-approve compound Bash commands.
#
# When a Bash command contains shell operators (&&, ||, |, ;), Claude Code's
# prefix matching (e.g. Bash(cargo fmt:*)) rejects the whole command even if
# every sub-command would individually be allowed. This hook uses shfmt to parse
# the command into an AST, extracts each sub-command, and checks them all against
# the allowed Bash patterns from settings.local.json. If all match, it outputs
# {"decision":"allow"} to auto-approve.
#
# Safety: if shfmt/jq are missing, parsing fails, or any sub-command uses
# unsupported syntax (subshells, loops, etc.), we output nothing and Claude Code
# falls through to its normal permission flow.

INPUT=$(cat)

# --- Forward to dashboard socket (existing behavior) ---
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
if [ -S "$SOCK" ]; then
  printf '%s' "$INPUT" | perl -MIO::Socket::UNIX -e '
    my $raw = do { local $/; <STDIN> };
    my ($cwd) = $raw =~ /"cwd"\s*:\s*"([^"]+)"/;
    exit 0 unless $cwd;
    my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
    print $s "{\"_hook\":\"PreToolUse\",\"cwd\":\"$cwd\"}";
    shutdown($s, 1);
  ' "$SOCK" 2>/dev/null
fi

# --- Compound Bash command auto-approval ---

# Only process Bash tool calls
TOOL_NAME=$(printf '%s' "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
[ "$TOOL_NAME" = "Bash" ] || exit 0

COMMAND=$(printf '%s' "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
[ -n "$COMMAND" ] || exit 0

# Only process compound commands (containing shell operators)
case "$COMMAND" in
  *'&&'*|*'||'*|*'|'*|*';'*) ;;
  *) exit 0 ;;
esac

# Require shfmt and jq on PATH
command -v shfmt >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

# Parse command into JSON AST with shfmt
AST=$(printf '%s\n' "$COMMAND" | shfmt -tojson 2>/dev/null) || exit 0

# Extract individual commands from the AST.
# - BinaryCmd (&&, ||, |): recurse into X and Y branches
# - CallExpr: reconstruct command from leading literal args
# - Anything else (subshells, loops, etc.): "UNSUPPORTED"
JQ_EXTRACT='
def cmd_str:
  if .Args == null or (.Args | length) == 0 then "UNSUPPORTED"
  else
    reduce .Args[] as $arg (
      {stop: false, words: []};
      if .stop then .
      elif [$arg.Parts[] | .Type] | all(. == "Lit") then
        .words += [[$arg.Parts[] | .Value] | join("")]
      else .stop = true
      end
    ) |
    if (.words | length) == 0 then "UNSUPPORTED"
    else .words | join(" ")
    end
  end;
def extract:
  if .Type == "BinaryCmd" then (.X.Cmd | extract), (.Y.Cmd | extract)
  elif .Type == "CallExpr" then cmd_str
  else "UNSUPPORTED"
  end;
if .Type == "File" then [.Stmts[] | .Cmd | extract] | .[]
else "UNSUPPORTED"
end
'

COMMANDS=$(printf '%s' "$AST" | jq -r "$JQ_EXTRACT" 2>/dev/null) || exit 0

# If any command uses unsupported syntax, fall through
case "$COMMANDS" in
  *UNSUPPORTED*) exit 0 ;;
esac

# Must have extracted at least one command
[ -n "$COMMANDS" ] || exit 0

# Load allowed Bash patterns from settings
SETTINGS="${CLAUDE_PROJECT_DIR:-.}/.claude/settings.local.json"
[ -f "$SETTINGS" ] || exit 0

BASH_PATTERNS=$(jq -r '.permissions.allow // [] | .[]' "$SETTINGS" 2>/dev/null \
  | grep '^Bash(')

[ -n "$BASH_PATTERNS" ] || exit 0

# Check each extracted command against the allowed patterns.
# Pattern formats:
#   Bash(prefix:*)  → command must start with "prefix" (followed by space or EOL)
#   Bash(exact)     → command must equal "exact"
while IFS= read -r cmd; do
  [ -z "$cmd" ] && continue
  matched=false
  while IFS= read -r pattern; do
    [ -z "$pattern" ] && continue
    if [[ "$pattern" == *":*)" ]]; then
      # Prefix match
      prefix="${pattern#Bash(}"
      prefix="${prefix%:\*)}"
      if [ "$cmd" = "$prefix" ] || [[ "$cmd" == "$prefix "* ]]; then
        matched=true
        break
      fi
    else
      # Exact match
      exact="${pattern#Bash(}"
      exact="${exact%)}"
      if [ "$cmd" = "$exact" ]; then
        matched=true
        break
      fi
    fi
  done <<< "$BASH_PATTERNS"
  if [ "$matched" = false ]; then
    exit 0
  fi
done <<< "$COMMANDS"

# All sub-commands match allowed patterns — auto-approve
printf '{"decision":"allow","reason":"all sub-commands match allowed patterns"}\n'
