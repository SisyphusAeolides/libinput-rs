# Fail-open models

These models describe the safety rule used by the Rust daemon: a physical
touchpad can be grabbed only after the uinput sink is ready, and every failure
while forwarding input transitions back to a released state.

The resource-lifecycle models cover the shared-library backend: an acquired
restricted descriptor must be consumed by exactly one reject/remove path, and
only a udev backend can possess hotplug permission. A path backend has no
constructor for that permission.

The restricted-discovery models cover compositor-managed permissions. Event
nodes are discovered from directory entries without a direct open, and the
privileged callback remains the only transition from a candidate to an open
device. A denied callback leaves the device closed.

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
