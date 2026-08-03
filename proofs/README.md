# Fail-open models

The historical fail-open models describe the retired companion pipeline. They
remain regression evidence that no physical device may be grabbed without a
ready output sink; the production shared backend never uses EVIOCGRAB.

The resource-lifecycle models cover the shared-library backend: an acquired
restricted descriptor must be consumed by exactly one reject/remove path, and
only a udev backend can possess hotplug permission. A path backend has no
constructor for that permission.

The restricted-discovery models cover compositor-managed permissions. Event
nodes are discovered from directory entries without a direct open, and the
privileged callback remains the only transition from a candidate to an open
device. A denied callback leaves the device closed.

`HwDetect.agda` models the fused discovery lifecycle from a listed candidate
through restricted-open, classification, announcement, and terminal removal.
It proves that capability-set union is an upper bound and the least such bound.
`HwSpec.idr` defines the total udev-plus-capability classifier and a registry
whose element type cannot represent a phantom device. The compiled Fortran
`capforge` kernel parses sysfs bitmaps and classifies ioctl capability words;
Rust regression vectors require its answers to match the fallback classifier.

`ProfileSelection.agda` proves that one evidence value cannot authorize two
different profiles and that every selected profile has matching evidence.
`ProfileSelection.idr` checks the same selector as a total function. The
production Fortran scorer ranks only hard-matched candidates supplied by Rust;
it cannot manufacture a device class or bypass these selection laws.

- Agda proves that no value witnessing permission to grab can exist while the
  sink is absent, and that name-only discovery is independent of direct-open
  permission.
- Idris 2 makes invalid runtime states unconstructable with indexed types and
  total transitions, including the restricted-open discovery path.
- Fortran supplies independent executable state-machine models. Their runtime
  checks cover fail-open grabbing, permission-independent discovery,
  exactly-once restricted-descriptor closure, and the rule that only a udev
  backend may enable hotplug.

Run `make proofs` to check the DNF-packaged Agda, Idris 2, and GNU Fortran
models. `make proofs-strict` first verifies that all three compilers are
installed.

## Current ArachOS integration status

This project is maintained as part of the ArachOS production graph. Its role is
formal input-safety and qualification evidence..

CI and release evidence are evaluated on immutable revisions. Hardware support
is reported by bounded route and support level; this README does not claim
universal native support. Gate 3 requires signed hardware identity, target
kernel provenance, package authority, health checks, rollback behavior, and
representative physical-hardware evidence before production qualification.

## Current ArachOS integration status

This project is maintained as part of the ArachOS production graph. Its role is
formal input-safety and qualification evidence.

CI and release evidence are evaluated on immutable revisions. Hardware support
is reported by bounded route and support level; this README does not claim
universal native support. Gate 3 requires signed hardware identity, target
kernel provenance, package authority, health checks, rollback behavior, and
representative physical-hardware evidence before production qualification.
