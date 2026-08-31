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

    if ($line.StartsWith(";")) { $label = $line; $out.Add($raw); continue }
    if ($line -eq "")          { $out.Add($raw); continue }

    $fields = $line.Split("|")
    if ($fields.Length -eq 3 -and $fields[1] -eq "link") {
        # Whole-word GPU anywhere in the label. Everything else — including no label at all — is a
        # CPU node, which is what these machines were before there was anything else to be.
        if ($label -match '(?i)(^|[^a-z0-9])gpu([^a-z0-9]|$)') { $purpose = "gpu" } else { $purpose = "cpu" }
        $out.Add("$line|$purpose")
        $changed++
        $label = ""
        continue
    }

    $label = ""
    $out.Add($raw)
}

# The token file is the whole of the API's authentication, so it is replaced by rename rather than
# rewritten in place: a truncated write here locks every caller out, node and console alike.
$temp = "$Tokens.migrating"
[System.IO.File]::WriteAllLines($temp, $out)
Move-Item -Path $temp -Destination $Tokens -Force

Write-Output "marked $changed link token(s) with a purpose"
