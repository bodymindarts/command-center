#!/bin/bash
# Forward UserPromptSubmit events to the dashboard socket.
# Detects [Watch: prefix in prompt text and sets _watch flag accordingly.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

cat | perl -MIO::Socket::UNIX -e '
  my $raw = do { local $/; <STDIN> };
  my ($cwd) = $raw =~ /"cwd"\s*:\s*"([^"]+)"/;
  exit 0 unless $cwd;
  my ($prompt) = $raw =~ /"prompt"\s*:\s*"((?:[^"\\]|\\.)*)"/;
  my $watch = ($prompt && $prompt =~ /^\[Watch:/) ? "true" : "false";
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s "{\"_hook\":\"UserPromptSubmit\",\"cwd\":\"$cwd\",\"_watch\":$watch}";
  shutdown($s, 1);
' "$SOCK"
