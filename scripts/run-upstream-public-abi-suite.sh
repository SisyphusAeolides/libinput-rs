#!/usr/bin/env bash
# Build a disposable upstream libinput 1.31.3 test runner against libinput-rs.
#
# Some upstream test bodies configure gesture holds through an upstream-private
# interface. That interface is compiled into the upstream runner and is not a
# public libinput ABI contract, so it cannot configure an opaque replacement
# library. This script copies the supplied source tree into /tmp, discovers
# exactly those test bodies, marks them not applicable, builds the copied
# suite, and runs the remaining public-ABI corpus with one worker. It never
# writes the supplied source tree or its configured build directory.
#
# Usage:
#   scripts/run-upstream-public-abi-suite.sh UPSTREAM_SOURCE_DIR UPSTREAM_BUILD_DIR [SUITE_ARGUMENTS...]
#
# The build directory supplies the explicit Meson options used to rebuild the
# disposable copy. Set LIBINPUT_RS_LIBRARY to test a library other than the
# default target/release/libinput.so.

set -euo pipefail

expected_upstream_commit=26191d396d74d505541d6311f0b4ae68d791b890
expected_private_config_test_count=196
expected_private_config_test_hash=e29aba062bc3d8811b9978a9b22d4c444ce2f84cb175b835016943de798b7cce

die() {
	printf '%s\n' "run-upstream-public-abi-suite: $*" >&2
	exit 2
}

usage() {
	printf '%s\n' \
		'usage: run-upstream-public-abi-suite.sh UPSTREAM_SOURCE_DIR UPSTREAM_BUILD_DIR [SUITE_ARGUMENTS...]' \
		'' \
		'Build a temporary libinput 1.31.3 public-ABI test suite, mark tests that' \
		'require upstream-private gesture-hold configuration as not applicable,' \
		'and run the remaining suite against libinput-rs.' \
		'' \
		'This suite creates many synthetic input devices. Run it from SSH or a' \
		'text console with no active graphical session, and explicitly set' \
		'LIBINPUT_RS_ALLOW_UINPUT_TESTS=1.'
}

if [[ ${1:-} == '--help' ]]; then
	usage
	exit 0
fi

[[ $# -ge 2 ]] || {
	usage >&2
	exit 2
}

upstream_source=$(realpath "$1")
template_build=$(realpath "$2")
shift 2

suite_jobs=${LIBINPUT_RS_SUITE_JOBS:-1}
suite_arguments=("$@")
[[ $suite_jobs =~ ^[1-9][0-9]*$ ]] || die 'LIBINPUT_RS_SUITE_JOBS must be a positive integer'
for argument in "$@"; do
	case "$argument" in
		--jobs|--jobs=*)
			die 'set LIBINPUT_RS_SUITE_JOBS instead of passing --jobs directly'
			;;
	esac
done

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
candidate=${LIBINPUT_RS_LIBRARY:-"$repo_root/target/release/libinput.so"}
candidate=$(realpath "$candidate")

for command in awk flock git grep ldd loginctl meson mktemp ninja patch pgrep readelf realpath rm rsync sed sha256sum sort tee wc; do
	command -v "$command" >/dev/null || die "required command not found: $command"
done

[[ ${LIBINPUT_RS_ALLOW_UINPUT_TESTS:-} == 1 ]] ||
	die 'refusing to create synthetic input devices; set LIBINPUT_RS_ALLOW_UINPUT_TESTS=1 from a text console or SSH session'

active_graphical_sessions=()
while read -r session_id _; do
	[[ -n ${session_id:-} ]] || continue
	active=$(loginctl show-session "$session_id" -p Active --value 2>/dev/null || true)
	type=$(loginctl show-session "$session_id" -p Type --value 2>/dev/null || true)
	if [[ $active == yes && ( $type == wayland || $type == x11 ) ]]; then
		active_graphical_sessions+=("$session_id:$type")
	fi
done < <(loginctl list-sessions --no-legend 2>/dev/null || true)

if (( ${#active_graphical_sessions[@]} > 0 )); then
	die "refusing to disrupt active graphical session(s): ${active_graphical_sessions[*]}"
fi

[[ -r "$candidate" ]] || die "candidate library is not readable: $candidate"
[[ -r "$upstream_source/meson.build" ]] || die "not an upstream source directory: $upstream_source"
[[ -r "$upstream_source/src/libinput-private-config.c" ]] || die "missing private hold helper source"
[[ -r "$template_build/build.ninja" ]] || die "not a configured Meson build directory: $template_build"
template_options="$template_build/meson-private/cmd_line.txt"
[[ -r "$template_options" ]] || die "missing Meson command-line options: $template_options"

grep -Fq "version : '1.31.3'" "$upstream_source/meson.build" ||
	die 'this harness is pinned to upstream libinput 1.31.3'
[[ "$(git -C "$upstream_source" rev-parse --show-toplevel)" == "$upstream_source" ]] ||
	die 'the supplied upstream source must be a standalone checkout'
[[ "$(git -C "$upstream_source" rev-parse HEAD)" == "$expected_upstream_commit" ]] ||
	die "the supplied upstream source is not commit $expected_upstream_commit"
git -C "$upstream_source" diff --quiet ||
	die 'the supplied upstream source has uncommitted tracked changes'
git -C "$upstream_source" diff --cached --quiet ||
	die 'the supplied upstream source has staged changes'
meson introspect --projectinfo "$template_build" | grep -Fq '"version": "1.31.3"' ||
	die 'the configured build directory is not upstream libinput 1.31.3'
grep -Fxq 'tests = true' "$template_options" ||
	die 'the configured build directory must enable tests'
readelf -d "$candidate" | grep -Fq 'Library soname: [libinput.so.10]' ||
	die "candidate does not declare SONAME libinput.so.10: $candidate"

# Reapply every explicit Meson option from the known-good upstream build.
# Values are kept as individual array elements so paths containing spaces stay
# intact. Properties are intentionally not replayed: this harness is native
# only, and cross-build properties would not produce a runnable test suite.
meson_options=()
in_options=false
line=''
while IFS= read -r line || [[ -n "$line" ]]; do
	case "$line" in
		'[options]')
			in_options=true
			;;
		'[properties]')
			break
			;;
		''|'#'*)
			;;
		*)
			[[ "$in_options" == true ]] || continue
			[[ "$line" == *' = '* ]] || die "unrecognized Meson option: $line"
			key=${line%% = *}
			value=${line#* = }
			[[ "$key" =~ ^[A-Za-z0-9_.-]+$ ]] || die "unsafe Meson option name: $key"
			meson_options+=("-D${key}=${value}")
			;;
	esac
done < "$template_options"

# The litest suite shares udev and quirks state. Cooperating harness runs from
# the same account use a lock, and the process check protects across accounts
# and from older runners that do not use this harness.
exec 9>"/tmp/libinput-rs-upstream-suite-${EUID}.lock"
flock -n 9 || die 'another libinput-rs upstream suite is already running'

suite_is_running() {
	pgrep -f '^/[^[:space:]]*/libinput-test-suite(-public)?([[:space:]]|$)' >/dev/null
}

suite_is_running && die 'an upstream libinput test suite is already running'

workdir=$(mktemp -d /tmp/libinput-rs-public-abi.XXXXXX)
cleanup() {
	status=$?
	case "$workdir" in
		/tmp/libinput-rs-public-abi.*)
			rm -rf -- "$workdir"
			;;
		*)
			printf '%s\n' "refusing to remove unexpected work directory: $workdir" >&2
			status=1
			;;
	esac
	trap - EXIT
	exit "$status"
}
trap cleanup EXIT

source_copy="$workdir/source"
build_copy="$workdir/build"
library_dir="$workdir/lib"
mkdir -p "$source_copy" "$library_dir"

# The upstream checkout contains locally configured build directories. They
# are neither source nor a reusable build tree, so omit them from the copy.
rsync -a --exclude='/.git/' --exclude='/build*/' "$upstream_source/" "$source_copy/"

private_config_tests="$workdir/private-config-tests"
for source_file in "$source_copy"/test/*.c; do
	awk '
		match($0, /^START_TEST\(([^)]*)\)/, matched) {
			test_name = matched[1]
			next
		}
		/END_TEST/ {
			test_name = ""
			next
		}
		test_name != "" &&
			/litest_(enable|disable)_hold_gestures\(|libinput_device_config_gesture_/ {
				print test_name
			}
	' "$source_file"
done | LC_ALL=C sort -u > "$private_config_tests"

private_config_test_count=$(wc -l < "$private_config_tests")
private_config_test_count=${private_config_test_count//[[:space:]]/}
[[ "$private_config_test_count" == "$expected_private_config_test_count" ]] ||
	die "expected $expected_private_config_test_count private-config test functions, found $private_config_test_count"
private_config_test_hash=$(sha256sum "$private_config_tests" | awk '{print $1}')
[[ "$private_config_test_hash" == "$expected_private_config_test_hash" ]] ||
	die 'private-config test inventory differs from the pinned upstream fixture'

for source_file in "$source_copy"/test/*.c; do
	outside_private_config_calls=$(awk '
		match($0, /^START_TEST\(([^)]*)\)/, matched) {
			test_name = matched[1]
			next
		}
		/END_TEST/ {
			test_name = ""
			next
		}
		test_name == "" &&
			/litest_(enable|disable)_hold_gestures\(|libinput_device_config_gesture_/ {
				print FILENAME ":" FNR
			}
	' "$source_file")
	[[ -z "$outside_private_config_calls" ]] ||
		die "private gesture-hold configuration used outside a test body: $outside_private_config_calls"
done

while IFS= read -r test_name; do
	[[ "$test_name" =~ ^[A-Za-z0-9_]+$ ]] ||
		die "unsafe private-config test name: $test_name"
done < "$private_config_tests"

private_config_header="$source_copy/test/libinput-rs-public-abi-private-config.h"
{
	printf '%s\n' \
		'/* Generated from the pinned upstream test corpus by this harness. */' \
		'#ifndef LIBINPUT_RS_PUBLIC_ABI_PRIVATE_CONFIG_H' \
		'#define LIBINPUT_RS_PUBLIC_ABI_PRIVATE_CONFIG_H' \
		'' \
		'#include <stdbool.h>' \
		'#include <stddef.h>' \
		'#include <string.h>' \
		'' \
		'static inline bool' \
		'libinput_rs_public_abi_requires_private_config(const char *name)' \
		'{' \
		'static const char *const excluded_tests[] = {'
	awk '{ printf "\t\t\"%s\",\n", $0 }' "$private_config_tests"
	printf '%s\n' \
		'};' \
		'' \
		'for (size_t i = 0; i < sizeof(excluded_tests) / sizeof(excluded_tests[0]); i++) {' \
		'if (strcmp(name, excluded_tests[i]) == 0)' \
		'return true;' \
		'}' \
		'' \
		'return false;' \
		'}' \
		'' \
		'#endif'
} > "$private_config_header"

# The here-document keeps C indentation literal. Prefix tab-indented context
# lines with the unified-diff context marker while streaming the fixed patch.
sed -e 's/^\t/ &/' -e 's/\\\\$/\\/' <<'PATCH' | patch -d "$source_copy" -p1 --batch
--- a/test/litest.h
+++ b/test/litest.h
@@ -38,13 +38,16 @@
 #include "libinput-private-config.h"
 #include "libinput-util.h"
 #include "litest-runner.h"
+#include "libinput-rs-public-abi-private-config.h"
 #include "quirks.h"
 
 DEFINE_DESTROY_CLEANUP_FUNC(libevdev_uinput);
 
 #define START_TEST(func_)  \\
    static enum litest_runner_result func_(const struct litest_runner_test_env *test_env) { \\
-	int _i _unused_ = test_env->rangeval;
+	int _i _unused_ = test_env->rangeval; \\
+	if (libinput_rs_public_abi_requires_private_config(#func_)) \\
+		return LITEST_NOT_APPLICABLE;
 
 #define END_TEST \\
 	return LITEST_PASS; \\
PATCH

[[ -r "$private_config_header" ]] || die 'private-config exclusion header was not generated'
printf '%s\n' \
	"marking $private_config_test_count upstream test functions that require private gesture-hold configuration as not applicable (inventory $private_config_test_hash)" \
	>&2

CC=cc CXX=c++ meson setup "$build_copy" "$source_copy" "${meson_options[@]}"
ninja -C "$build_copy" \
	libinput-test-suite \
	libinput-fuzz-extract \
	libinput-fuzz-to-zero
runner="$build_copy/libinput-test-suite"
[[ -x "$runner" ]] || die "upstream test runner was not built: $runner"

# Meson emits a legacy $ORIGIN DT_RPATH for the runner. That path takes
# precedence over LD_LIBRARY_PATH, so point its build-tree SONAME link at the
# candidate and verify resolution before executing any test.
rm -f -- "$build_copy/libinput.so.10"
ln -s "$candidate" "$build_copy/libinput.so.10"
resolved_library=$(ldd "$runner" | awk '$1 == "libinput.so.10" { print $3 }')
[[ -n "$resolved_library" ]] || die 'the upstream runner does not resolve libinput.so.10'
resolved_library=$(realpath "$resolved_library")
[[ "$resolved_library" == "$candidate" ]] ||
	die "the upstream runner resolved $resolved_library instead of $candidate"

ln -s "$candidate" "$library_dir/libinput.so.10"
suite_is_running && die 'an upstream libinput test suite started while this runner was building'

printf '%s\n' \
	"running public-ABI upstream suite with $suite_jobs worker(s) against $candidate" \
	>&2
suite_quirks_dir=${LIBINPUT_RS_PUBLIC_ABI_QUIRKS_DIR:-$source_copy/quirks}
suite_libinput_quirks_dir=${LIBINPUT_QUIRKS_DIR:-$suite_quirks_dir}
suite_libinput_quirks_override="${suite_libinput_quirks_dir}"
privilege_prefix=()
if (( EUID != 0 )); then
	command -v sudo >/dev/null || die 'sudo is required when the suite is not run as root'
	privilege_prefix=(sudo -n)
fi
raw_report="$workdir/upstream-suite.yaml"
set +e
"${privilege_prefix[@]}" env \
		LD_LIBRARY_PATH="$library_dir" \
		"LIBINPUT_QUIRKS_DIR=$suite_libinput_quirks_override" \
		"$runner" --jobs "$suite_jobs" "${suite_arguments[@]}" 2>&1 | tee "$raw_report"
runner_status=${PIPESTATUS[0]}
set -e
(( runner_status == 0 )) || exit "$runner_status"

summary_value() {
	local key=$1
	awk -v key="$key" '
		{ gsub(/\r/, "") }
		$1 == "summary:" { in_summary = 1; next }
		in_summary && $1 == (key ":") { value = $2 }
		END { print value }
	' "$raw_report"
}

completed=$(summary_value completed)
passed=$(summary_value pass)
not_applicable=$(summary_value na)
failed=$(summary_value fail)
skipped=$(summary_value skip)
result=$(summary_value status)
[[ $completed =~ ^[0-9]+$ && $passed =~ ^[0-9]+$ && $not_applicable =~ ^[0-9]+$ ]] ||
	die 'upstream suite did not emit a complete numeric summary'
[[ $failed == 0 && $skipped =~ ^[0-9]+$ && $result == PASS ]] ||
	die "upstream suite summary is not passing: fail=$failed skip=$skipped status=$result"
(( passed > 0 )) || die 'upstream suite reported no passing tests'

# An unfiltered parity run must cover the exact pinned corpus. This prevents a
# shortened or accidentally filtered run from creating release evidence.
if (( ${#suite_arguments[@]} == 0 )); then
	[[ $completed == 23245 ]] ||
		die "pinned upstream corpus size changed: expected 23245, completed $completed"
fi

if [[ -n ${LIBINPUT_RS_PARITY_REPORT:-} ]]; then
	report_parent=$(dirname -- "$LIBINPUT_RS_PARITY_REPORT")
	mkdir -p -- "$report_parent"
	candidate_sha256=$(sha256sum "$candidate" | awk '{print $1}')
	cat > "$LIBINPUT_RS_PARITY_REPORT" <<EOF
target_version = "1.31.3"
target_commit = "$expected_upstream_commit"
candidate_sha256 = "$candidate_sha256"
completed = $completed
pass = $passed
not_applicable = $not_applicable
fail = $failed
skip = $skipped
result = "$result"
EOF
fi
