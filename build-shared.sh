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

compile_flags=()
if [[ -n ${CFLAGS:-} ]]; then
  read -r -a compile_flags <<<"${CFLAGS}"
fi

compiler=${CC:-cc}
objcopy_tool=${OBJCOPY:-objcopy}
archive=target/release/libinput.a
compat_archive=target/release/libinput-compat.a
compat_object=target/release/keyboard_compat.o
compat_test=$(mktemp target/release/keyboard-compat-test.XXXXXX)
trap 'rm -f "$compat_test"' EXIT

"$compiler" \
  "${compile_flags[@]}" \
  -std=c11 -Wall -Wextra -Werror \
  -DLIBINPUT_RS_KEYBOARD_COMPAT_TEST \
  src/keyboard_compat.c -pthread -o "$compat_test"
"$compat_test"

"$compiler" \
  "${compile_flags[@]}" \
  -std=c11 -fPIC -Wall -Wextra -Werror \
  -c src/keyboard_compat.c -o "$compat_object"

rm -f "$compat_archive"
"$objcopy_tool" \
  --redefine-sym libinput_get_event=libinput_rs_get_event \
  --redefine-sym libinput_event_destroy=libinput_rs_event_destroy \
  --redefine-sym libinput_unref=libinput_rs_unref \
  --redefine-sym libinput_event_keyboard_get_seat_key_count=libinput_rs_event_keyboard_get_seat_key_count \
  --redefine-sym libinput_event_pointer_get_seat_button_count=libinput_rs_event_pointer_get_seat_button_count \
  --redefine-sym libinput_device_led_update=libinput_rs_device_led_update \
  "$archive" "$compat_archive"

native_libraries=(-ludev)
if [[ ${CARGO_FEATURES:-} == *libwacom* ]]; then
  native_libraries+=(-lwacom)
fi

"$compiler" -shared \
  "${link_flags[@]}" \
  "$compat_object" \
  -Wl,--whole-archive "$compat_archive" -Wl,--no-whole-archive \
  -Wl,--version-script=libinput.map \
  -Wl,-soname,libinput.so.10 \
  -ldl -lm -lpthread -lrt "${native_libraries[@]}" \
  -o target/release/libinput.so
