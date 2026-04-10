#!/bin/bash
# Forward Stop events to the dashboard socket.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

TASK_NAME="${CC_TASK_NAME:-}"

cat | perl -MIO::Socket::UNIX -e '
  my $raw = do { local $/; <STDIN> };
  my ($cwd) = $raw =~ /"cwd"\s*:\s*"([^"]+)"/;
  exit 0 unless $cwd;
  my $tn = $ARGV[1];
  my $extra = length($tn) ? ",\"_task_name\":\"$tn\"" : "";
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s "{\"_hook\":\"Stop\",\"cwd\":\"$cwd\"$extra}";
  shutdown($s, 1);
' "$SOCK" "$TASK_NAME"
