#!/bin/bash
# Notify the dashboard that a tool completed, so it can clear
# stale permission requests (e.g. when approved in the agent's pane).
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

TASK_NAME="${CC_TASK_NAME:-}"

# Extract cwd from Claude's hook JSON and send a resolved notification.
cat | perl -MIO::Socket::UNIX -e '
  my $raw = do { local $/; <STDIN> };
  my ($cwd) = $raw =~ /"cwd"\s*:\s*"([^"]+)"/;
  exit 0 unless $cwd;
  my $tn = $ARGV[1];
  my $extra = length($tn) ? ",\"_task_name\":\"$tn\"" : "";
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s "{\"_resolved\":true,\"cwd\":\"$cwd\"$extra}";
  shutdown($s, 1);
' "$SOCK" "$TASK_NAME"
