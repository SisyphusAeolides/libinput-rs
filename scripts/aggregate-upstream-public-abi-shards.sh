#!/usr/bin/env bash

set -euo pipefail

expected_upstream_commit=26191d396d74d505541d6311f0b4ae68d791b890
expected_shards=8
expected_completed=23245
expected_passed=12185
expected_not_applicable=11059
expected_skipped=1
expected_skipped_test_hash=a5357d16e54b88fee77deb453ffb595b268fca5df012981f13f89d7892ae169d

die() {
	printf '%s\n' "aggregate-upstream-public-abi-shards: $*" >&2
	exit 2
}

[[ $# == 2 ]] || die 'usage: aggregate-upstream-public-abi-shards.sh SHARD_DIR OUTPUT_REPORT'
shard_dir=$(realpath "$1")
output_report=$2

value() {
	local report=$1 key=$2
	awk -F ' *= *' -v key="$key" '
		$1 == key {
			gsub(/^"|"$/, "", $2)
			print $2
			exit
		}
	' "$report"
}

shopt -s nullglob
reports=("$shard_dir"/libinput-rs-parity-shard-*.toml)
raw_reports=("$shard_dir"/libinput-rs-upstream-suite-shard-*.yaml)
(( ${#reports[@]} == expected_shards )) ||
	die "expected $expected_shards shard reports, found ${#reports[@]}"
(( ${#raw_reports[@]} == expected_shards )) ||
	die "expected $expected_shards raw reports, found ${#raw_reports[@]}"

declare -A seen=()
candidate_sha256=
completed=0
passed=0
not_applicable=0
failed=0
skipped=0

for report in "${reports[@]}"; do
	[[ $(value "$report" target_version) == 1.31.3 ]] || die "wrong target version in $report"
	[[ $(value "$report" target_commit) == "$expected_upstream_commit" ]] ||
		die "wrong target commit in $report"
	[[ $(value "$report" result) == PASS ]] || die "non-passing shard: $report"
	[[ $(value "$report" shard_total) == "$expected_shards" ]] ||
		die "wrong shard total in $report"

	index=$(value "$report" shard_index)
	[[ $index =~ ^[0-9]+$ ]] || die "invalid shard index in $report"
	(( index < expected_shards )) || die "out-of-range shard index in $report"
	[[ -z ${seen[$index]:-} ]] || die "duplicate shard index $index"
	seen[$index]=1

	hash=$(value "$report" candidate_sha256)
	[[ $hash =~ ^[0-9a-f]{64}$ ]] || die "invalid candidate hash in $report"
	if [[ -z $candidate_sha256 ]]; then
		candidate_sha256=$hash
	else
		[[ $hash == "$candidate_sha256" ]] || die 'candidate library hashes differ between shards'
	fi

	for key in completed pass not_applicable fail skip; do
		count=$(value "$report" "$key")
		[[ $count =~ ^[0-9]+$ ]] || die "non-numeric $key in $report"
		case "$key" in
			completed) completed=$((completed + count)) ;;
			pass) passed=$((passed + count)) ;;
			not_applicable) not_applicable=$((not_applicable + count)) ;;
			fail) failed=$((failed + count)) ;;
			skip) skipped=$((skipped + count)) ;;
		esac
	done
done

(( ${#seen[@]} == expected_shards )) || die 'shard index inventory is incomplete'
(( completed == expected_completed )) || die "expected $expected_completed completed, found $completed"
(( passed == expected_passed )) || die "expected $expected_passed passes, found $passed"
(( not_applicable == expected_not_applicable )) ||
	die "expected $expected_not_applicable not-applicable, found $not_applicable"
(( failed == 0 )) || die "expected zero failures, found $failed"
(( skipped == expected_skipped )) || die "expected $expected_skipped skip, found $skipped"
(( passed + not_applicable + failed + skipped == completed )) ||
	die 'aggregated result counts do not equal the completed count'

skipped_tests=$(mktemp)
trap 'rm -f -- "$skipped_tests"' EXIT
awk '
	/^  - name: / {
		name = $0
		sub(/^  - name: "/, "", name)
		sub(/"$/, "", name)
	}
	/status: SKIP/ { print name }
' "${raw_reports[@]}" | LC_ALL=C sort > "$skipped_tests"
skipped_test_hash=$(sha256sum "$skipped_tests" | awk '{print $1}')
[[ $skipped_test_hash == "$expected_skipped_test_hash" ]] ||
	die "pinned upstream skipped-test inventory changed: $skipped_test_hash"

mkdir -p -- "$(dirname -- "$output_report")"
cat > "$output_report" <<EOF
target_version = "1.31.3"
target_commit = "$expected_upstream_commit"
candidate_sha256 = "$candidate_sha256"
shard_count = $expected_shards
completed = $completed
pass = $passed
not_applicable = $not_applicable
fail = $failed
skip = $skipped
result = "PASS"
EOF
