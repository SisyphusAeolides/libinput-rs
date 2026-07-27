# Compatibility gates

`libinput-rs` is a 100% drop-in replacement because every gate in
`libinput-1.31.3.toml` is true. Matching function names is necessary but is
not sufficient: event behavior, configuration status values, ownership,
restricted-file callbacks, device admission, quirks, and suspend/resume all
match the reference implementation.
The reference is upstream libinput 1.31.3 at commit
`26191d396d74d505541d6311f0b4ae68d791b890`, matching Fedora 45's
`libinput.so.10.13.0` ABI.

Use `scripts/check-abi.sh` for symbol, symbol-version, version-node, and SONAME
parity. Use `scripts/run-upstream-public-abi-suite.sh` with a clean checkout
of the pinned upstream commit and its configured build directory to build a
disposable, public-ABI test runner. It runs the suite serially against the
Rust library through an isolated loader path and does not change the
installed system library.

The gate file records only complete test groups. Individual passing tests are
useful progress, but do not turn a group green.
