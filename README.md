# libinput-rs

`libinput-rs` is a Rust input project that provides:

- an optional touchpad companion daemon using evdev and uinput;
- a 100% drop-in replacement implementation of the `libinput.so.10` C ABI.

The runtime and development RPMs replace Fedora's `libinput` and
`libinput-devel` packages. Display managers and compositors use the replacement
after they restart.

## Supported systems

The supported packaging path is DNF/RPM on Fedora releases whose repositories
provide `libwacom-devel` 2.18 or newer. The initial COPR builds target Fedora;
Enterprise Linux 9 and 10 do not currently provide the libwacom ABI required
by this implementation.

## Install from COPR

Perform replacement testing from a text console or an SSH session so rollback
remains available even if the graphical session cannot start.

```bash
sudo dnf install dnf-plugins-core
sudo dnf copr enable sisyphuscode/libinput-rs
sudo dnf install libinput-rs libinput-rs-devel --allowerasing
sudo ldconfig
sudo systemctl reboot
```

The runtime RPM ships a vendor preset that enables `libinput-rs.service` on
first installation. The daemon starts automatically on the next boot. An
administrator's explicit enable or disable choice is preserved during package
upgrades.

After boot, verify the service:

```bash
systemctl is-enabled libinput-rs.service
systemctl is-active libinput-rs.service
systemctl status libinput-rs.service
```

To opt out of the companion daemon without changing the ABI replacement:

```bash
sudo systemctl disable --now libinput-rs.service
```

To restore Fedora's original runtime and development packages:

```bash
sudo systemctl disable --now libinput-rs.service 2>/dev/null || true
sudo dnf install libinput libinput-devel --allowerasing
sudo ldconfig
sudo systemctl reboot
```

## Build with DNF dependencies

```bash
sudo dnf install rust cargo gcc make systemd-devel libwacom-devel pkgconf-pkg-config
make all
make check
make test
```

The daemon is built at `target/release/libinput-rs`. The ABI library is built
at `target/release/libinput.so`.

## Configuration

The daemon reads `/etc/libinput-rs/config.json`:

```json
{
  "tap_to_click": true,
  "natural_scrolling": true,
  "pointer_acceleration": 2.2,
  "disable_while_typing": true
}
```

The companion normalizes touchpad movement and two-finger scrolling using the
kernel-reported axis resolution. When a device omits resolution metadata, it
uses a live-tested calibrated fallback. `pointer_acceleration` keeps the
existing user-facing scale: `2.2` is the live-tested neutral value, larger
values are faster, and smaller positive values are slower.

The normalized motion base corresponds to the previous `2.5` reference, so the
daemon divides the configured value by `2.5` internally. This preserves the
exact effective pointer travel of existing configurations across the 0.2.1
upgrade. For the shipped default, `0.45 × (2.2 / 2.5) = 0.18 × 2.2 = 0.396`.

## Drop-in replacement

The runtime RPM installs `libinput.so.10` in the system linker path. The
`libinput-rs-devel` RPM installs `libinput.h`, the unversioned linker name, and
`libinput.pc`.

The udev backend enumerates `/dev/input/event*` by directory entry and delegates
the first device open to the compositor's `open_restricted` callback. This keeps
startup discovery compatible with logind-managed permissions where the
compositor can list device nodes but cannot open them directly.

## Formal safety models

The fail-open and restricted-discovery state machines are modeled three ways
under `proofs/`:

- Agda proves that a grab cannot be authorized while the output sink is absent
  and that listed event nodes remain discoverable without direct-open access;
- Idris 2 uses indexed states and total transitions so invalid runtime and
  restricted-open states are unconstructable;
- Fortran provides independently compiled executable reference models for
  fail-open grabbing, permission-independent discovery, exactly-once descriptor
  closure, and udev-only hotplug.

Agda, Idris 2, and GNU Fortran are available through DNF on the supported
Fedora targets:

```bash
sudo dnf install Agda idris2 gcc-gfortran
make proofs
```

`make proofs-strict` requires all three compilers and runs every model.

## Publishing

The RPM and crates.io release procedure is documented in [RELEASING.md](RELEASING.md).
The crates.io workspace publishes `libinput-rs-evdev` first and `libinput-rs`
second. System replacement installations should use the COPR RPMs rather than
`cargo install`.

## Reference behavior

Runtime and ABI lifecycle work is compared against the upstream libinput
architecture and the `complyue/libinput` branch referenced during debugging.
The Rust implementation provides complete behavior parity and is intended for
use as a 100% drop-in replacement.

## License

MIT
