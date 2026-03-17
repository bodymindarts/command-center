#!/bin/bash
# Route permissions through the dashboard socket when active.
# Without the socket, output nothing — Claude uses its normal permission flow.
# Uses perl (ships with macOS) to avoid needing clat/cargo in PATH.
SOCK="${CC_PERM_SOCKET:-${TMPDIR:-/tmp}/cc-permissions.sock}"
[ -S "$SOCK" ] || exit 0

INPUT=$(cat)

# Inject session role into the JSON payload if CC_SESSION_ROLE is set.
if [ -n "$CC_SESSION_ROLE" ]; then
    INPUT=$(printf '%s' "$INPUT" | perl -e '
        use JSON::PP;
        my $d = do { local $/; <STDIN> };
        my $j = decode_json($d);
        $j->{"_session_role"} = $ENV{"CC_SESSION_ROLE"};
        print encode_json($j);
    ')
fi

printf '%s' "$INPUT" | perl -MIO::Socket::UNIX -e '
  my $d = do { local $/; <STDIN> };
  my $s = IO::Socket::UNIX->new(Peer => $ARGV[0], Type => SOCK_STREAM) or exit 0;
  print $s $d;
  shutdown($s, 1);
  local $/;
  print <$s>;
' "$SOCK"
