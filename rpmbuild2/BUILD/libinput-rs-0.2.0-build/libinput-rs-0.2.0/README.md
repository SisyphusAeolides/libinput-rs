# libinput-rs

`libinput-rs` is a Rust input research project with two deliberately separated
artifacts:

- an optional touchpad companion daemon using evdev and uinput;
- an experimental implementation of the `libinput.so.10` C ABI for explicit,
  per-application compatibility testing.

The RPM never replaces Fedora or Enterprise Linux's system `libinput` library.
Display managers and compositors continue to use the distribution-supported
implementation.

## Why 0.2.0 changes installation

Earlier packaging installed the experimental library as the system
`libinput.so.10`. The exported symbol names matched, but many behaviors were
not yet equivalent to upstream libinput. Because GDM, SDDM, plasma-login,
GNOME Shell, and KWin all depend on that library, systemwide replacement could
cause a black screen, remove the password prompt, or leave a session without
working input.

The standalone daemon also grabbed touchpads before proving that its uinput
sink was available. A restart loop could therefore interrupt input repeatedly.

Version 0.2.0 fixes the deployment and lifecycle hazards:

- the system `libinput` package and `/usr/lib64/libinput.so.10` are preserved;
- the experimental ABI library lives under `/usr/lib64/libinput-rs/`;
- uinput is created before any physical touchpad is grabbed;
- keyboards are never grabbed or forwarded;
- only capability-identified touchpads are grabbed;
- forwarding failures terminate the daemon and release every grab;
- systemd rate-limits failures without imposing display-manager ordering;
- suspend and resume now release and reopen ABI-backend devices;
- kernel-module reset and autosuspend modifications were removed.

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

If an older package replaced the system library, restore it before testing
0.2.0:

```bash
sudo dnf reinstall libinput
sudo ldconfig
```

## Build with DNF dependencies

```bash
sudo dnf install rust cargo gcc make systemd-devel libwacom-devel pkgconf-pkg-config
make all
make check
make test
```

The daemon is built at `target/release/libinput-rs`. The experimental ABI
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

## Experimental ABI testing

The ABI library is not in the system linker path. Test it only with a single
non-critical application:

```bash
LD_LIBRARY_PATH=/usr/lib64/libinput-rs your-test-program
```

Do not copy or symlink it over the distribution's `libinput.so.10`. Matching
an ABI surface is not the same as matching libinput's complete udev, seat,
device-quirk, gesture, and lifecycle behavior.

For isolated C consumer testing, install the private development surface and
use its distinct pkg-config name:

```bash
sudo dnf install libinput-rs-devel
pkg-config --cflags --libs libinput-rs
```

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
The Rust implementation intentionally remains opt-in until behavioral tests,
not only symbol tests, demonstrate compositor-safe compatibility.

## License

MIT
