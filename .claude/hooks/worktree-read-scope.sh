#!/bin/bash
# PreToolUse hook: forward Read/Grep/Glob events to the dashboard
# for worktree scope detection. Pure transport — all logic is Rust-side.
# Reads back response (allow/deny) so Claude Code respects the decision.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

INPUT=$(cat)

# Inject hook discriminator so the Rust side can identify this event.
TAGGED="{\"_hook\":\"WorktreeReadScope\",${INPUT#\{}"

printf '%s' "$TAGGED" | perl -MIO::Socket::UNIX -e '
  my $d = do { local $/; <STDIN> };
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s $d;
  shutdown($s, 1);
  local $/;
  print <$s>;
' "$SOCK"
