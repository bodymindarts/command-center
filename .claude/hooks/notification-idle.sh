#!/bin/bash
# Notify the dashboard that the agent is idle (waiting for input),
# so the task list can show an idle indicator.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

# Extract cwd from Claude's Notification hook JSON and send an idle notification.
cat | perl -MIO::Socket::UNIX -e '
  my $raw = do { local $/; <STDIN> };
  my ($cwd) = $raw =~ /"cwd"\s*:\s*"([^"]+)"/;
  exit 0 unless $cwd;
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s "{\"_idle\":true,\"cwd\":\"$cwd\"}";
  shutdown($s, 1);
' "$SOCK"
