#!/bin/bash
# Route permissions through the dashboard socket when active.
# Without the socket, output nothing — Claude uses its normal permission flow.
# Uses perl (ships with macOS) to avoid needing clat/cargo in PATH.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

INPUT=$(cat)

# Inject _task_name hint when CC_TASK_NAME is set, so the dashboard can
# identify the task even if the agent cd'd outside its worktree.
if [ -n "$CC_TASK_NAME" ]; then
  INPUT="{\"_task_name\":\"$CC_TASK_NAME\",${INPUT#\{}"
fi

printf '%s' "$INPUT" | perl -MIO::Socket::UNIX -e '
  my $d = do { local $/; <STDIN> };
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s $d;
  shutdown($s, 1);
  local $/;
  print <$s>;
' "$SOCK"
