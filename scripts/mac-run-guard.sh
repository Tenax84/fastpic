#!/bin/bash
# Korlátozott GUI-teszt: a fastpicet saját folyamatcsoportban indítja, a megadott
# idő után az egész csoportot és minden kóbor fastpic példányt leállít.
# Így egy hibás "új példány" ág sem tud elszaladni.
#
#   scripts/mac-run-guard.sh <másodperc> <parancs...>
set -u
SECS="${1:?hasznalat: mac-run-guard.sh <masodperc> <parancs...>}"
shift
BIN_RE='(target/(debug|release)|FastPic\\.app/Contents/MacOS)/fastpic$'

sweep() {
  ps -Ao pid=,command= | awk -v re="$BIN_RE" '$2 ~ re {print $1}'
}

# Biztonsági öv: a teszt alatt indított "új példány" csak naplózik, nem indul el.
# FASTPIC_NO_SPAWN=0 -val kívülről kikapcsolható, ha épp az új példányt teszteljük.
export FASTPIC_NO_SPAWN="${FASTPIC_NO_SPAWN-1}"

set -m
"$@" &
PGID=$!
set +m

MAX=0
SEEN=0
for _ in $(seq 1 "$((SECS * 20))"); do
  N=$(sweep | wc -l | tr -d ' ')
  [ "$N" -gt "$MAX" ] && MAX=$N
  [ "$N" -gt 0 ] && SEEN=1
  if [ "$N" -gt 5 ]; then
    echo "!! $N fastpic process fut – azonnali leállítás" >&2
    break
  fi
  # Ha már futott és mind kilépett (pl. --bench), nem várunk tovább az időkorlátig.
  if [ "$SEEN" = 1 ] && [ "$N" = 0 ]; then
    break
  fi
  sleep 0.05
done

kill -- "-$PGID" 2>/dev/null
for _ in 1 2 3 4 5; do
  PIDS=$(sweep)
  [ -z "$PIDS" ] && break
  echo "$PIDS" | xargs kill -9 2>/dev/null
  sleep 0.2
done
echo "[guard] csúcs process-szám: $MAX, maradék: $(sweep | wc -l | tr -d ' ')"
