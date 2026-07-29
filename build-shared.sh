#!/usr/bin/env bash
set -euo pipefail

feature_args=()
if [[ -n ${CARGO_FEATURES:-} ]]; then
  read -r -a feature_args <<<"${CARGO_FEATURES}"
fi

cargo build --lib --release --locked --offline "${feature_args[@]}"

link_flags=()
if [[ -n ${RPM_LD_FLAGS:-} ]]; then
  read -r -a link_flags <<<"${RPM_LD_FLAGS}"
fi

native_libraries=(-ludev)
if nm -u target/release/libinput.a 2>/dev/null | rg '_gfortran_' >/dev/null; then
  native_libraries+=(-lgfortran)
fi
if [[ ${CARGO_FEATURES:-} == *libwacom* ]]; then
  native_libraries+=(-lwacom)
fi

cc -shared \
  "${link_flags[@]}" \
  -Wl,-z,defs \
  -Wl,--whole-archive target/release/libinput.a -Wl,--no-whole-archive \
  -Wl,--version-script=libinput.map \
  -Wl,-soname,libinput.so.10 \
  -ldl -lm -lpthread -lrt "${native_libraries[@]}" \
  -o target/release/libinput.so
