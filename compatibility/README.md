# Compatibility gates

`libinput-rs` targets library ABI and behavioral compatibility with the
reference in `libinput-1.31.3.toml`. The checked-in file defines required
evidence; it does not become proof merely because a field was manually set.
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

Library-compatibility evidence does not establish full distribution-package
parity for every upstream utility or downstream distribution patch. Individual
passing tests are useful progress, but do not turn a group green.
