#!/bin/sh
# Refreshes the data files ChronusGateway-RS reloads automatically (docs/DEPLOYMENT.md):
#   $CHRONUS_DATA_DIR/tle.txt       one element set from CelesTrak   (station.tle_file)
#   $CHRONUS_DATA_DIR/finals.daily  IERS Earth orientation, finals2000A format (freshness.eop_file)
#
# A download replaces a file only if it looks right, so an error page never reaches the gateway.
# One attempt per run, no retries (CelesTrak's published expectations); schedule it every few hours
# (deploy/chronus-fetch-data.timer), never more often than every two hours.
#
# Environment: CHRONUS_DATA_DIR (default /var/lib/chronus-gateway), CHRONUS_CATNR (NORAD catalog
# number, default 25544 = ISS), TLE_URL / EOP_URL to override the sources.
set -eu

DIR=${CHRONUS_DATA_DIR:-/var/lib/chronus-gateway}
CATNR=${CHRONUS_CATNR:-25544}
TLE_URL=${TLE_URL:-"https://celestrak.org/NORAD/elements/gp.php?CATNR=${CATNR}&FORMAT=tle"}
EOP_URL=${EOP_URL:-"https://datacenter.iers.org/data/latestVersion/finals.daily.iau2000.txt"}
AGENT="chronus-gateway-fetch/1 (+https://github.com/IsomorphicAlgo/chronus-gateway)"
status=0

# A TLE: a line starting "1 NNNNNC " and one starting "2 NNNNN " (the gateway checks the rest).
# shellcheck disable=SC2329 # called indirectly, as fetch's CHECK argument
looks_like_tle() {
    grep -Eq '^1 [0-9 ]{5}[A-Z ] ' "$1" && grep -Eq '^2 [0-9 ]{5} ' "$1"
}

# finals2000A: YYMMDD, blank, MJD with ".00", blank, I/P flag.
# shellcheck disable=SC2329 # called indirectly, as fetch's CHECK argument
looks_like_finals() {
    grep -Eq '^[0-9 ]{6} [0-9 ]{5}\.00 [IP ]' "$1"
}

# fetch URL DEST CHECK
fetch() {
    url=$1 dest=$2 check=$3
    tmp=$(mktemp "$dest.XXXXXX")
    set -- --fail --silent --show-error --location --max-time 60 --user-agent "$AGENT" --output "$tmp"
    # -z: only download when the server copy is newer than ours (If-Modified-Since).
    if [ -f "$dest" ]; then set -- "$@" -z "$dest"; fi
    if curl "$@" "$url"; then
        if [ ! -s "$tmp" ]; then
            echo "fetch-data: $dest is up to date"
            rm -f "$tmp"
        elif "$check" "$tmp"; then
            chmod 0640 "$tmp"
            mv -f "$tmp" "$dest"
            echo "fetch-data: updated $dest"
        else
            echo "fetch-data: $url did not return the expected format; keeping $dest" >&2
            rm -f "$tmp"
            status=1
        fi
    else
        echo "fetch-data: download of $url failed; keeping $dest" >&2
        rm -f "$tmp"
        status=1
    fi
}

mkdir -p "$DIR"
fetch "$TLE_URL" "$DIR/tle.txt" looks_like_tle
fetch "$EOP_URL" "$DIR/finals.daily" looks_like_finals
exit $status
