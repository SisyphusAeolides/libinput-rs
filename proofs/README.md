# Fail-open models

These models describe the safety rule used by the Rust daemon: a physical
touchpad can be grabbed only after the uinput sink is ready, and every failure
while forwarding input transitions back to a released state.

The resource-lifecycle models cover the shared-library backend: an acquired
restricted descriptor must be consumed by exactly one reject/remove path, and
only a udev backend can possess hotplug permission. A path backend has no
constructor for that permission.

- Agda proves that no value witnessing permission to grab can exist while the
  sink is absent.
- Idris 2 makes invalid runtime states unconstructable with indexed types and
  total transitions.
- Austral represents the exclusive grab as a linear token that must be
  consumed exactly once by `release`; restricted descriptors and udev hotplug
  permission are linear for the same reason.

Run `make proofs` to check the DNF-packaged Agda and Idris models. If the
Austral compiler is installed, the same target also type-checks the Austral
model; `make proofs-strict` requires all three compilers.
