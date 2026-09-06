#!/usr/bin/env bash
set -Eeuo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
idris2_cmd=${IDRIS2:-idris2}
agda_cmd=${AGDA:-agda}
fortran_cmd=${FC:-gfortran}
build_dir=$(mktemp -d "${TMPDIR:-/tmp}/libinput-formal.XXXXXXXX")
trap 'find "$build_dir" -depth -delete 2>/dev/null || :' EXIT HUP INT TERM

for command in "$idris2_cmd" "$agda_cmd" "$fortran_cmd"; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'formal check: missing compiler: %s\n' "$command" >&2
        exit 1
    }
done

idris_dir="$build_dir/idris2"
agda_dir="$build_dir/agda"
agda_data="$build_dir/agda-data"
agda_config="$build_dir/agda-config"
fortran_modules="$build_dir/fortran-modules"
mkdir -p "$idris_dir" "$agda_dir" "$agda_data" "$agda_config" "$fortran_modules"

cp proofs/idris/*.idr "$idris_dir/"
for source in ButtonLifecycle.idr FailOpen.idr ResourceLifecycle.idr \
    RestrictedDiscovery.idr HwSpec.idr ProfileSelection.idr; do
    (cd "$idris_dir" && "$idris2_cmd" --check "$source")
done

cp proofs/agda/*.agda "$agda_dir/"
Agda_datadir="$agda_data" "$agda_cmd" --setup >/dev/null 2>&1
for source in ButtonLifecycle.agda FailOpen.agda ResourceLifecycle.agda \
    RestrictedDiscovery.agda HwDetect.agda ProfileSelection.agda; do
    Agda_datadir="$agda_data" \
    XDG_DATA_HOME="$agda_data" \
    XDG_CONFIG_HOME="$agda_config" \
        "$agda_cmd" --safe --no-libraries -i "$agda_dir" "$agda_dir/$source"
done

compile_fortran() {
    local source=$1 output=$2
    "$fortran_cmd" -std=f2018 -Wall -Wextra -Werror -fcheck=all \
        -J "$fortran_modules" -I "$fortran_modules" \
        -o "$build_dir/$output" "${root}/proofs/fortran/$source"
    "$build_dir/$output"
}

compile_fortran fail_open.f90 fail-open
compile_fortran resource_lifecycle.f90 resource-lifecycle
compile_fortran restricted_discovery.f90 restricted-discovery
compile_fortran button_lifecycle.f90 button-lifecycle
printf '%s\n' 'formal proofs: Idris2, Agda, and Fortran passed'
