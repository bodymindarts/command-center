#!/bin/bash
# PreToolUse hook: forward Read/Grep/Glob events to the dashboard
# for worktree scope detection. Pure transport — all logic is Rust-side.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

cat | perl -MIO::Socket::UNIX -e '
  my $raw = do { local $/; <STDIN> };
  # Inject hook discriminator so the Rust side can identify this event
  $raw =~ s/^\s*\{/{"_hook":"WorktreeReadScope",/ or exit 0;
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s $raw;
  shutdown($s, 1);
' "$SOCK"
