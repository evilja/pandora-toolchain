#!/usr/bin/env sh
# pandora-migration: 1788177600
#
# Give every Pandora Mini token the purpose field the scheduler now reads.
#
# A link token line grew a fourth field, `<token>|link|<node>|<cpu|gpu>`, and a line without one
# reads as CPU. Before the field existed the same information was written by hand into the token's
# `;` label, and no label has ever carried both words — so the label is an unambiguous source to
# derive the field from once, here, instead of teaching the token parser to keep reading free text
# forever.
#
# Idempotent: a line that already has four fields is left exactly as it is.

set -e

TOKENS="DB/config/global/environment/api.pandora"

if [ ! -f "$TOKENS" ]; then
    echo "no token file at $TOKENS; nothing to migrate"
    exit 0
fi

TMP="$TOKENS.migrating.$$"

awk '
BEGIN { changed = 0 }
{
    raw = $0
    line = raw
    sub(/^[ \t]+/, "", line)
    sub(/[ \t]+$/, "", line)

    if (line ~ /^;/) { label = line; print raw; next }
    if (line == "")  { print raw; next }

    n = split(line, f, "|")
    if (n == 3 && f[2] == "link") {
        # Whole-word GPU anywhere in the label. Everything else — including no label at all — is a
        # CPU node, which is what these machines were before there was anything else to be.
        if (tolower(label) ~ /(^|[^a-z0-9])gpu([^a-z0-9]|$)/) purpose = "gpu"
        else purpose = "cpu"
        print line "|" purpose
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

echo "marked $CHANGED link token(s) with a purpose"
