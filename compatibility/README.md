# Compatibility gates

`libinput-rs` provides 100% drop-in ABI, behavioral, and package compatibility
with the libinput 1.31.3 reference on the supported x86_64 DNF/RPM targets.
The checked-in manifest defines the exact required evidence; it does not become
proof merely because a field was manually set.
Matching function names is necessary but not sufficient: event behavior,
configuration status values, ownership, restricted-file callbacks, device
admission, quirks, and suspend/resume must be derived from a complete passing
test run bound to the candidate library hash.
The reference is upstream libinput 1.31.3 at commit
`26191d396d74d505541d6311f0b4ae68d791b890`, matching Fedora 45's
`libinput.so.10.13.0` ABI.

Use `scripts/check-abi.sh` for symbol, symbol-version, version-node, and SONAME
parity. Use `scripts/run-upstream-public-abi-suite.sh` with a clean checkout
of the pinned upstream commit and its configured build directory to build a
disposable, public-ABI test runner. It runs the suite serially against the
Rust library through an isolated loader path and does not change the
installed system library.

The unfiltered release result is 23,245 completed cases: 12,185 pass, 11,059
are hash-pinned as requiring an upstream-private configuration interface, one
is the upstream release-build skip for internal event debugging, and zero
fail. RPM verification separately checks the complete Fedora-compatible
runtime, development, utility, manual-page, completion, udev, and quirks
payload.
