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

$ErrorActionPreference = "Stop"

$Tokens = "DB/config/global/environment/api.pandora"

if (-not (Test-Path $Tokens)) {
    Write-Output "no token file at $Tokens; nothing to migrate"
    exit 0
}

$label = ""
$changed = 0
$out = New-Object System.Collections.Generic.List[string]

foreach ($raw in [System.IO.File]::ReadAllLines($Tokens)) {
    $line = $raw.Trim()

    if ($line.StartsWith(";")) {
        # The label is the comment body up to the " (added <unix>)" suffix /gentoken appends,
        # which is exactly what the token parser reads it as.
        $body = $line.TrimStart(';').Trim()
        $idx = $body.IndexOf(" (added ")
        if ($idx -ge 0) { $body = $body.Substring(0, $idx) }
        $label = $body.Trim()
        $out.Add($raw)
        continue
    }
    if ($line -eq "") { $out.Add($raw); continue }

    $fields = $line.Split("|")
    if ($label -ceq "PNwitch" -and $fields[$fields.Length - 1] -ne "witch" -and
        -not ($fields.Length -ge 2 -and $fields[1] -eq "link")) {
        $out.Add("$line|witch")
        $changed++
        $label = ""
        continue
    }

    $label = ""
    $out.Add($raw)
}

# The token file is the whole of the API's authentication, so it is replaced by rename rather than
# rewritten in place: a truncated write here locks every caller out, node and console alike.
$tmp = "$Tokens.migrating.$PID"
[System.IO.File]::WriteAllLines($tmp, $out)
Move-Item -Force -Path $tmp -Destination $Tokens

Write-Output "marked $changed token(s) privileged"
