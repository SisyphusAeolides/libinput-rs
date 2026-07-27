# libinput-rs

`libinput-rs` is a Rust input project that provides:

- an optional touchpad companion daemon using evdev and uinput;
- a 100% drop-in replacement implementation of the `libinput.so.10` C ABI.

The RPM replaces Fedora or Enterprise Linux's system `libinput` library.
Display managers and compositors will use this implementation automatically.

## Supported systems

The supported packaging path is DNF/RPM on Fedora releases whose repositories
provide `libwacom-devel` 2.18 or newer. The initial COPR builds target Fedora;
Enterprise Linux 9 and 10 do not currently provide the libwacom ABI required
by this implementation.

## Install from COPR

```bash
sudo dnf install dnf-plugins-core
sudo dnf copr enable sisyphuscode/libinput-rs
sudo dnf install libinput-rs
```

Installation does not enable the daemon. Test it from a text console or an SSH
session before enabling it permanently:

```bash
sudo systemctl start libinput-rs
systemctl status libinput-rs
sudo systemctl enable libinput-rs
```

To stop using the daemon:

```bash
sudo systemctl disable --now libinput-rs
```

If you wish to restore the original system library:

```bash
sudo dnf reinstall libinput
```

## Build with DNF dependencies

```bash
sudo dnf install rust cargo gcc make systemd-devel libwacom-devel pkgconf-pkg-config
make all
make check
make test
```

The daemon is built at `target/release/libinput-rs`. The ABI
library is built at `target/release/libinput.so`.

## Configuration

The daemon reads `/etc/libinput-rs/config.json`:

```json
{
  "tap_to_click": true,
  "natural_scrolling": true,
  "pointer_acceleration": 1.0,
  "disable_while_typing": true
}
```

## Drop-in Replacement

The library is installed in the system linker path as a 100% drop-in replacement
for the distribution's `libinput.so.10`. Development headers are also provided.

## Formal safety models

The fail-open state machine is modeled three ways under `proofs/`:

- Agda proves that a grab cannot be authorized while the output sink is absent;
- Idris 2 uses indexed states and total transitions so invalid runtime states
  are unconstructable;
- Fortran provides an independently compiled executable reference model for
  fail-open grabbing, exactly-once descriptor closure, and udev-only hotplug.

Agda, Idris 2, and GNU Fortran are available through DNF on the supported
Fedora targets:

```bash
sudo dnf install Agda idris2 gcc-gfortran
make proofs
```

`make proofs-strict` requires all three compilers and runs every model.

## Reference behavior

Runtime and ABI lifecycle work is compared against the upstream libinput
architecture and the `complyue/libinput` branch referenced during debugging.
The Rust implementation provides complete behavior parity and is intended for
use as a 100% drop-in replacement.

## License

MIT
