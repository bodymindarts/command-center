#!/bin/bash
# Route permissions through the dashboard socket when active.
# Without the socket, output nothing — Claude uses its normal permission flow.
# Uses perl (ships with macOS) to avoid needing clat/cargo in PATH.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

INPUT=$(cat)
printf '%s' "$INPUT" | perl -MIO::Socket::UNIX -e '
  my $d = do { local $/; <STDIN> };
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s $d;
  shutdown($s, 1);
  local $/;
  print <$s>;
' "$SOCK"
