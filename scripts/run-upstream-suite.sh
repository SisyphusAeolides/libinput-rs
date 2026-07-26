#!/usr/bin/bash
set -euo pipefail

builddir=${1:?usage: run-upstream-suite.sh UPSTREAM_BUILD_DIR [SUITE_ARGUMENTS...]}
shift

candidate=${LIBINPUT_RS_LIBRARY:-target/release/libinput.so}
runner=$builddir/libinput-test-suite

test -x "$runner"
test -r "$candidate"
candidate=$(realpath "$candidate")

libdir=$(mktemp -d /tmp/libinput-rs-suite.XXXXXX)
trap 'rm -rf -- "$libdir"' EXIT
ln -s "$candidate" "$libdir/libinput.so.10"

LD_LIBRARY_PATH="$libdir" "$runner" "$@"
