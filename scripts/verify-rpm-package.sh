#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf '%s\n' "usage: $0 LIBINPUT_RS_RPM" >&2
}

if [[ $# -ne 1 ]]; then
    usage
    exit 2
fi

package=$1
for command in awk cc cpio eu-elflint pkg-config readelf readlink rg rpm rpm2cpio; do
    command -v "$command" >/dev/null
done
[[ -f "$package" ]] || {
    printf '%s\n' "missing RPM: $package" >&2
    exit 2
}
[[ "$(rpm -qp --qf '%{NAME}' "$package")" == "libinput-rs" ]] || {
    printf '%s\n' "not a libinput-rs RPM: $package" >&2
    exit 2
}

tmp_root=${TMPDIR:-/tmp}
stage=$(mktemp -d "$tmp_root/libinput-rs-rpm.XXXXXX")
cleanup() {
    case "$stage" in
        "$tmp_root"/libinput-rs-rpm.*)
            rm -rf -- "$stage"
            ;;
        *)
            printf '%s\n' "refusing to remove unexpected staging directory: $stage" >&2
            return 1
            ;;
    esac
}
trap cleanup EXIT

(
    cd "$stage"
    rpm2cpio "$package" | cpio -idm --quiet
)

libdir=$(rpm --eval '%{_libdir}')
includedir=$(rpm --eval '%{_includedir}')
runtime_library="$stage$libdir/libinput.so.10.13.0"
runtime_link="$stage$libdir/libinput.so.10"
development_link="$stage$libdir/libinput.so"
header="$stage$includedir/libinput.h"
pc_file="$stage$libdir/pkgconfig/libinput.pc"
udev_dir="$stage/usr/lib/udev"
udev_rules_dir="$udev_dir/rules.d"

[[ -f "$runtime_library" ]]
[[ -L "$runtime_link" && "$(readlink "$runtime_link")" == "libinput.so.10.13.0" ]]
[[ -L "$development_link" && "$(readlink "$development_link")" == "libinput.so.10" ]]
[[ -f "$header" && -f "$pc_file" ]]
[[ -x "$stage/usr/bin/libinput" ]]
[[ -x "$stage/usr/bin/libinput-rs-chwd" ]]
[[ -L "$stage/usr/bin/libinput-rs" && "$(readlink "$stage/usr/bin/libinput-rs")" == "libinput" ]]
[[ ! -e "$stage/usr/lib/systemd/system/libinput-rs.service" ]]
[[ ! -e "$stage/usr/lib/systemd/system-preset/90-libinput-rs.preset" ]]
[[ -f "$stage/usr/lib/systemd/system/libinput-rs-elan-resume.service" ]]
[[ -f "$stage/usr/lib/systemd/system-preset/91-libinput-rs-elan.preset" ]]
rg -F 'ExecStop=/usr/bin/libinput elan-recover --all --affected-only --quiet' \
    "$stage/usr/lib/systemd/system/libinput-rs-elan-resume.service"
[[ ! -e "$stage/etc/libinput-rs/config.json" ]]
[[ -L "$stage/usr/libexec/libinput/libinput-debug-events" ]]
[[ -L "$stage/usr/libexec/libinput/libinput-list-devices" ]]
[[ "$(readlink "$stage/usr/libexec/libinput/libinput-debug-events")" == "../../bin/libinput" ]]
[[ "$(readlink "$stage/usr/libexec/libinput/libinput-list-devices")" == "../../bin/libinput" ]]
for helper in libinput-device-group libinput-fuzz-extract libinput-fuzz-to-zero; do
    [[ -x "$udev_dir/$helper" ]]
done
[[ -f "$udev_rules_dir/80-libinput-device-groups.rules" ]]
[[ -f "$udev_rules_dir/90-libinput-fuzz-override.rules" ]]
[[ -f "$udev_rules_dir/90-libinput-rs-elantech-crc.rules" ]]
rg -F 'ATTR{firmware_id}=="PNP: LEN0408 PNP0f13"' \
    "$udev_rules_dir/90-libinput-rs-elantech-crc.rules"
[[ -f "$stage/usr/share/libinput/10-generic-keyboard.quirks" ]]
[[ -f "$stage/usr/share/libinput/30-vendor-elantech.quirks" ]]

readelf -dW "$runtime_library" | rg 'SONAME.*\[libinput\.so\.10\]'
! readelf -dW "$runtime_library" | rg '\((RPATH|RUNPATH)\)'
readelf -lW "$runtime_library" | rg 'GNU_RELRO'
readelf -dW "$runtime_library" | rg 'BIND_NOW|FLAGS_1.*NOW'
readelf -nW "$runtime_library" | rg 'Build ID:'
! readelf -lW "$runtime_library" | rg 'GNU_STACK.*RWE'
eu-elflint --gnu-ld "$runtime_library"

rpm -qp --provides "$package" | rg '^libinput-rs = '
rpm -qp --provides "$package" | rg '^libinput = 1\.31\.3$'
rpm -qp --provides "$package" | rg '^libinput-devel = 1\.31\.3$'
rpm -qp --provides "$package" | rg '^pkgconfig\(libinput\) = 1\.31\.3$'
rpm -qp --provides "$package" | rg '^libinput\.so\.10\('
rpm -qp --obsoletes "$package" | rg '^libinput < 1\.32\.0$'
rpm -qp --obsoletes "$package" | rg '^libinput-devel < 1\.32\.0$'
for installed_path in \
    "$libdir/libinput.so.10.13.0" \
    "$libdir/libinput.so.10" \
    "$libdir/libinput.so" \
    "$includedir/libinput.h" \
    "$libdir/pkgconfig/libinput.pc" \
    /usr/bin/libinput \
    /usr/bin/libinput-rs \
    /usr/bin/libinput-rs-chwd \
    /usr/libexec/libinput/libinput-debug-events \
    /usr/libexec/libinput/libinput-list-devices \
    /usr/lib/udev/libinput-device-group \
    /usr/lib/udev/libinput-fuzz-extract \
    /usr/lib/udev/libinput-fuzz-to-zero \
    /usr/lib/udev/rules.d/80-libinput-device-groups.rules \
    /usr/lib/udev/rules.d/90-libinput-fuzz-override.rules \
    /usr/lib/udev/rules.d/90-libinput-rs-elantech-crc.rules \
    /usr/lib/systemd/system/libinput-rs-elan-resume.service \
    /usr/lib/systemd/system-preset/91-libinput-rs-elan.preset \
    /usr/share/libinput/10-generic-keyboard.quirks \
    /usr/share/libinput/30-vendor-elantech.quirks; do
    rpm -qpl "$package" | rg -Fx "$installed_path"
done

"$stage/usr/bin/libinput" --version | rg '^libinput 1\.31\.3 \(libinput-rs\)$'
"$stage/usr/bin/libinput" --help | rg 'list-devices'
"$stage/usr/bin/libinput" elan-recover --help | rg '^Usage: libinput elan-recover'
"$stage/usr/bin/libinput-rs-chwd" --list-profiles | rg '^thinkpad-p53-elan'
"$stage/usr/libexec/libinput/libinput-debug-events" --help | rg '^Usage: libinput debug-events'
"$stage/usr/libexec/libinput/libinput-list-devices" --help | rg '^Usage: libinput list-devices'

pkg_config=(
    env
    PKG_CONFIG_PATH=
    "PKG_CONFIG_LIBDIR=$stage$libdir/pkgconfig:$libdir/pkgconfig:/usr/share/pkgconfig"
    "PKG_CONFIG_SYSROOT_DIR=$stage"
    pkg-config
)
[[ "$("${pkg_config[@]}" --variable=pcfiledir libinput)" == "$stage$libdir/pkgconfig" ]]
read -r -a cflags <<<"$("${pkg_config[@]}" --cflags libinput)"
read -r -a libs <<<"$("${pkg_config[@]}" --libs libinput)"
consumer="$stage/libinput-rs-smoke"
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
cc -Wl,-z,defs -o "$consumer" "$script_dir/../packaging/libinput-rs-smoke.c" \
    "${cflags[@]}" "${libs[@]}"
readelf -dW "$consumer" | rg 'Shared library: \[libinput\.so\.10\]'

env -i PATH="$PATH" LD_LIBRARY_PATH="$stage$libdir" "$consumer"
loader_trace=$(env -i PATH="$PATH" LD_TRACE_LOADED_OBJECTS=1 \
    LD_LIBRARY_PATH="$stage$libdir" "$consumer")
loaded_library=$(printf '%s\n' "$loader_trace" | awk '$1 == "libinput.so.10" && $2 == "=>" { print $3; exit }')
[[ -n "$loaded_library" ]]
[[ "$(readlink -f "$loaded_library")" == "$(readlink -f "$runtime_link")" ]]
