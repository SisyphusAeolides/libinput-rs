#!/usr/bin/bash
set -euo pipefail

reference=${1:?usage: check-abi.sh REFERENCE_LIBRARY CANDIDATE_LIBRARY [--exact]}
candidate=${2:?usage: check-abi.sh REFERENCE_LIBRARY CANDIDATE_LIBRARY [--exact]}
mode=${3:-}

test -r "$reference"
test -r "$candidate"

workdir=$(mktemp -d /tmp/libinput-rs-abi.XXXXXX)
trap 'rm -rf -- "$workdir"' EXIT

extract_symbols() {
    objdump -T "$1" |
        awk '$NF ~ /^libinput_/ { print $(NF - 1), $NF }' |
        sort -u
}

extract_nodes() {
    readelf --version-info "$1" |
        sed -n 's/.*Name: \(LIBINPUT_[^ ]*\).*/\1/p' |
        sort -Vu
}

extract_symbols "$reference" >"$workdir/reference.symbols"
extract_symbols "$candidate" >"$workdir/candidate.symbols"
if [[ "$mode" == "--exact" ]]; then
    diff -u "$workdir/reference.symbols" "$workdir/candidate.symbols"
else
    comm -23 "$workdir/reference.symbols" "$workdir/candidate.symbols" \
        >"$workdir/missing.symbols"
    if [[ -s "$workdir/missing.symbols" ]]; then
        echo "candidate is missing required ABI symbols:" >&2
        cat "$workdir/missing.symbols" >&2
        exit 1
    fi
fi

extract_nodes "$reference" >"$workdir/reference.nodes"
extract_nodes "$candidate" >"$workdir/candidate.nodes"
if [[ "$mode" == "--exact" ]]; then
    diff -u "$workdir/reference.nodes" "$workdir/candidate.nodes"
else
    comm -23 "$workdir/reference.nodes" "$workdir/candidate.nodes" \
        >"$workdir/missing.nodes"
    if [[ -s "$workdir/missing.nodes" ]]; then
        echo "candidate is missing required ABI version nodes:" >&2
        cat "$workdir/missing.nodes" >&2
        exit 1
    fi
fi

reference_soname=$(readelf -d "$reference" | sed -n 's/.*SONAME.*\[\(.*\)\]/\1/p')
candidate_soname=$(readelf -d "$candidate" | sed -n 's/.*SONAME.*\[\(.*\)\]/\1/p')
test "$reference_soname" = "libinput.so.10"
test "$candidate_soname" = "$reference_soname"

echo "ABI compatible: $(wc -l <"$workdir/candidate.symbols") symbols, $(wc -l <"$workdir/candidate.nodes") version nodes, SONAME $candidate_soname"
