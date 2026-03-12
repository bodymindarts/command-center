#!/bin/bash
# Forward UserPromptSubmit events to the dashboard socket.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

cat | perl -MIO::Socket::UNIX -e '
  my $raw = do { local $/; <STDIN> };
  my ($cwd) = $raw =~ /"cwd"\s*:\s*"([^"]+)"/;
  exit 0 unless $cwd;
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s "{\"_hook\":\"UserPromptSubmit\",\"cwd\":\"$cwd\"}";
  shutdown($s, 1);
' "$SOCK"
