# Fail-open models

These models describe the safety rule used by the Rust daemon: a physical
touchpad can be grabbed only after the uinput sink is ready, and every failure
while forwarding input transitions back to a released state.

- Agda proves that no value witnessing permission to grab can exist while the
  sink is absent.
- Idris 2 makes invalid runtime states unconstructable with indexed types and
  total transitions.
- Austral represents the exclusive grab as a linear token that must be
  consumed exactly once by `release`.

Run `make proofs` to check the DNF-packaged Agda and Idris models. If the
Austral compiler is installed, the same target also type-checks the Austral
model; `make proofs-strict` requires all three compilers.
