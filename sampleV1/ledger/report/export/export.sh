#!/usr/bin/env bash
# CSV Exporter — the same walk of the same log, written for a spreadsheet.
#
#   export.sh            comma-separated values on stdout
#
# Header row, one row per movement, then a total row.  Notes are quoted and any
# quotation mark inside a note is doubled, which is what spreadsheets expect.
set -u

ledger="${LEDGER_FILE:-}"
if [ -z "$ledger" ]; then
  echo "export: LEDGER_FILE is not set" >&2
  exit 2
fi

echo 'seq,amount_cents,memo'
if [ -f "$ledger" ]; then
  awk -F'|' '
    NF {
      sum += $2
      memo = $3
      gsub(/"/, "\"\"", memo)
      printf "%s,%s,\"%s\"\n", $1, $2, memo
    }
    END { printf ",%s,\"TOTAL\"\n", sum + 0 }
  ' "$ledger"
else
  echo ',0,"TOTAL"'
fi
exit 0
