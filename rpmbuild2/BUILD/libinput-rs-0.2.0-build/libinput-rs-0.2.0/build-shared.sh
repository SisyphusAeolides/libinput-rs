#!/usr/bin/env bash
set -euo pipefail

cargo build --lib --release --locked --offline

link_flags=()
if [[ -n ${RPM_LD_FLAGS:-} ]]; then
  read -r -a link_flags <<<"${RPM_LD_FLAGS}"
fi

cc -shared \
  "${link_flags[@]}" \
  -Wl,--whole-archive target/release/libinput.a -Wl,--no-whole-archive \
  -Wl,--version-script=libinput.map \
  -Wl,-soname,libinput.so.10 \
  -ldl -lm -lpthread -lrt -ludev -lwacom \
  -o target/release/libinput.so
