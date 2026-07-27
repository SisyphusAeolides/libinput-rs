#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf '%s\n' "usage: $0 RUNTIME_RPM DEVEL_RPM" >&2
}

if [[ $# -ne 2 ]]; then
    usage
    exit 2
fi

runtime_rpm=$1
devel_rpm=$2
for command in awk cc cpio eu-elflint pkg-config readelf readlink rg rpm rpm2cpio; do
    command -v "$command" >/dev/null
done
for package in "$runtime_rpm" "$devel_rpm"; do
    [[ -f "$package" ]] || {
        printf '%s\n' "missing RPM: $package" >&2
        exit 2
    }
done

[[ "$(rpm -qp --qf '%{NAME}' "$runtime_rpm")" == "libinput-rs" ]] || {
    printf '%s\n' "not a libinput-rs runtime RPM: $runtime_rpm" >&2
    exit 2
}
[[ "$(rpm -qp --qf '%{NAME}' "$devel_rpm")" == "libinput-rs-devel" ]] || {
    printf '%s\n' "not a libinput-rs-devel RPM: $devel_rpm" >&2
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
    rpm2cpio "$runtime_rpm" | cpio -idm --quiet
    rpm2cpio "$devel_rpm" | cpio -idm --quiet
)

libdir=$(rpm --eval '%{_libdir}')
includedir=$(rpm --eval '%{_includedir}')
runtime_dir="$stage$libdir/libinput-rs"
runtime_library="$runtime_dir/libinput.so.10.13.0"
runtime_link="$runtime_dir/libinput.so.10"
devel_link="$runtime_dir/libinput.so"
header="$stage$includedir/libinput-rs/libinput.h"
pc_file="$stage$libdir/pkgconfig/libinput-rs.pc"

[[ -f "$runtime_library" ]]
[[ -L "$runtime_link" && "$(readlink "$runtime_link")" == "libinput.so.10.13.0" ]]
[[ -L "$devel_link" && "$(readlink "$devel_link")" == "libinput.so.10" ]]
[[ -f "$header" && -f "$pc_file" ]]

readelf -dW "$runtime_library" | rg 'SONAME.*\[libinput\.so\.10\]'
! readelf -dW "$runtime_library" | rg '\((RPATH|RUNPATH)\)'
readelf -lW "$runtime_library" | rg 'GNU_RELRO'
readelf -dW "$runtime_library" | rg 'BIND_NOW|FLAGS_1.*NOW'
readelf -nW "$runtime_library" | rg 'Build ID:'
! readelf -lW "$runtime_library" | rg 'GNU_STACK.*RWE'
eu-elflint --gnu-ld "$runtime_library"

rpm -qp --provides "$runtime_rpm" | rg '^libinput-rs = '
! rpm -qp --provides "$runtime_rpm" | rg '^libinput\.so\.10\('
! rpm -qp --provides "$runtime_rpm" | rg '^libinput([[:space:]]|$)'
! rpm -qp --obsoletes "$runtime_rpm" | rg '^libinput([[:space:]]|$)'
! rpm -qp --conflicts "$runtime_rpm" | rg '^libinput([[:space:]]|$)'
for package in "$runtime_rpm" "$devel_rpm"; do
	! rpm -qpl "$package" | rg -Fx "$includedir/libinput.h"
	! rpm -qpl "$package" | rg -Fx "$libdir/pkgconfig/libinput.pc"
	! rpm -qpl "$package" | rg -Fx '/usr/share/pkgconfig/libinput.pc'
	! rpm -qp --provides "$package" | rg '^pkgconfig\(libinput\)([[:space:]]|$)'
	! rpm -qpl "$package" | awk -v libdir="$libdir" \
		'$0 ~ ("^" libdir "/libinput\\.so(\\..*)?$") { found = 1 } END { exit !found }'
done

pkg_config=(
    env
    PKG_CONFIG_PATH=
    "PKG_CONFIG_LIBDIR=$stage$libdir/pkgconfig:$libdir/pkgconfig"
    "PKG_CONFIG_SYSROOT_DIR=$stage"
    pkg-config
)
read -r -a cflags <<<"$("${pkg_config[@]}" --cflags libinput-rs)"
read -r -a libs <<<"$("${pkg_config[@]}" --libs libinput-rs)"
consumer="$stage/libinput-rs-smoke"
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
cc -Wl,-z,defs -o "$consumer" "$script_dir/../packaging/libinput-rs-smoke.c" \
    "${cflags[@]}" "${libs[@]}"
readelf -dW "$consumer" | rg 'Shared library: \[libinput\.so\.10\]'

env -i PATH="$PATH" LD_LIBRARY_PATH="$runtime_dir" "$consumer"
loader_trace=$(env -i PATH="$PATH" LD_TRACE_LOADED_OBJECTS=1 \
    LD_LIBRARY_PATH="$runtime_dir" "$consumer")
loaded_library=$(printf '%s\n' "$loader_trace" | awk '$1 == "libinput.so.10" && $2 == "=>" { print $3; exit }')
[[ -n "$loaded_library" ]]
[[ "$(readlink -f "$loaded_library")" == "$(readlink -f "$runtime_link")" ]]
