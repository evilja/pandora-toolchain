#!/usr/bin/env sh
# pandora-migration: 1788264000
#
# Give every token that was privileged by its label the privilege field the API now reads.
#
# The operator-only routes used to accept a token whose `;` comment was exactly `PNwitch`. A label
# is a note somebody writes for themselves: renaming it silently revoked the access, and typing it
# by accident silently granted it. Privilege is a field on the token line now — a trailing `|witch`,
# which composes with the existing kind fields — and `PNwitch` in a label means nothing on its own
# any more. This is the one-time translation, done here rather than by teaching the token parser to
# keep reading free text forever.
#
# Link lines are left alone: a Pandora Mini token is a machine's credential, and the parser refuses
# the privilege on one whatever its line says.
#
# Idempotent: a line that already ends in `|witch` is left exactly as it is.

set -e

TOKENS="DB/config/global/environment/api.pandora"

if [ ! -f "$TOKENS" ]; then
    echo "no token file at $TOKENS; nothing to migrate"
    exit 0
fi

TMP="$TOKENS.migrating.$$"

awk '
BEGIN { changed = 0; label = "" }
{
    raw = $0
    line = raw
    sub(/^[ \t]+/, "", line)
    sub(/[ \t]+$/, "", line)

    if (line ~ /^;/) {
        # The label is the comment body up to the " (added <unix>)" suffix /gentoken appends,
        # which is exactly what the token parser reads it as.
        body = line
        sub(/^;[ \t]*/, "", body)
        idx = index(body, " (added ")
        if (idx > 0) body = substr(body, 1, idx - 1)
        sub(/[ \t]+$/, "", body)
        label = body
        print raw
        next
    }
    if (line == "") { print raw; next }

    n = split(line, f, "|")
    if (label == "PNwitch" && f[n] != "witch" && f[2] != "link") {
        print line "|witch"
        changed = changed + 1
        label = ""
        next
    }

    label = ""
    print raw
}
END { print changed > "/dev/stderr" }
' "$TOKENS" > "$TMP" 2> "$TMP.count"

CHANGED=$(cat "$TMP.count")
rm -f "$TMP.count"

# The token file is the whole of the API's authentication, so it is replaced by rename rather than
# rewritten in place: a truncated write here locks every caller out, node and console alike.
chmod 600 "$TMP" 2>/dev/null || true
mv "$TMP" "$TOKENS"

echo "marked $CHANGED token(s) privileged"
