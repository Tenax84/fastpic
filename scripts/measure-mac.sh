#!/bin/bash
# Indulási idő és memória mérése macOS-en, ismételt futással, mediánnal.
#   scripts/measure-mac.sh <kép> [futások száma] [bináris]
# A rendszerhez adott bash 3.2, ezért nincs asszociatív tömb.
set -u
cd "$(dirname "$0")/.."
IMG="${1:?hasznalat: measure-mac.sh <kep> [futasok] [binaris]}"
RUNS="${2:-5}"
BIN="${3:-target/release/fastpic}"
OUT=$(mktemp -t fastpic-bench)
ALL=$(mktemp -t fastpic-bench-all)
: > "$ALL"

for i in $(seq 1 "$RUNS"); do
  rm -f "$OUT"
  scripts/mac-run-guard.sh 8 "$BIN" --bench "$OUT" "$IMG" >/dev/null 2>&1
  # Két futás között hagyjuk elcsendesedni a rendszert (WindowServer, lemez-gyorsítótár).
  sleep 1
  if [ ! -s "$OUT" ]; then echo "a $i. futás nem adott eredményt" >&2; continue; fi
  grep -E '^(first_image_ms|preview_ms|working_set_mb|private_mb)=' "$OUT" >> "$ALL"
  MARKS=$(grep '^mark_' "$OUT")
done

echo "kép: $IMG   futások: $RUNS   bináris: $BIN"
for k in preview_ms first_image_ms working_set_mb private_mb; do
  vals=$(grep "^$k=" "$ALL" | cut -d= -f2 | sort -n | tr '\n' ' ')
  med=$(echo "$vals" | tr ' ' '\n' | grep -v '^$' | awk '{a[NR]=$1} END{print (NR%2)?a[(NR+1)/2]:(a[NR/2]+a[NR/2+1])/2}')
  printf '%-16s medián %8s   (%s)\n' "$k" "$med" "$(echo "$vals" | xargs)"
done
echo "--- utolsó futás fázisai (ms) ---"
echo "${MARKS:-}"
rm -f "$OUT" "$ALL"
