#!/bin/bash
# Forward full PreToolUse JSON to dashboard socket and relay response.
# Pure transport — all logic lives in the Rust dashboard.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

INPUT=$(cat)

# Inject _hook discriminator and forward the full payload.
# Read back response (may be empty or contain a decision).
TAGGED="{\"_hook\":\"PreToolUse\",${INPUT#\{}"

printf '%s' "$TAGGED" | perl -MIO::Socket::UNIX -e '
  my $d = do { local $/; <STDIN> };
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s $d;
  shutdown($s, 1);
  local $/;
  print <$s>;
' "$SOCK"
