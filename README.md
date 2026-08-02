# libinput-rs

`libinput-rs` is a 100% drop-in replacement for libinput 1.31.3 on
x86_64 Arch-based systems. It implements the `libinput.so.10` C ABI and
ships the matching runtime, development, command-line, udev, and quirks
package surface.

Release 0.3.1 is validated against upstream libinput 1.31.3 commit
`26191d396d74d505541d6311f0b4ae68d791b890`. The release gate covers all 309
public symbols, 25 symbol-version nodes, SONAME `libinput.so.10`, and all
23,245 pinned behavioral cases. The verified result is 12,185 passes, 11,059
tests that require an upstream-private configuration interface and are
therefore not applicable to a public-ABI replacement, one upstream release
build skip, and zero failures. The skipped test exercises internal event
debugging that upstream explicitly disables in release builds.

## Supported systems

The supported packaging path is the Sisyphus Arch repository on x86_64
Arch-based distributions. The package replaces the distribution `libinput`
package and installs the same shared-library ABI, tools, headers, udev rules,
and quirks tree.

Agda and Idris 2 proofs are verified in CI and are not runtime dependencies.
GNU Fortran compiles the capability bitmap kernel during the package build; the
packaged library therefore depends on the standard libgfortran runtime.

## Install from the Sisyphus repository

Add the repository to `/etc/pacman.conf`:

```ini
[sisyphus]
SigLevel = Optional TrustAll
Server = https://sisyphusaeolides.github.io/Sisyphus-Repo/$arch
```

Then install it:

```bash
sudo pacman -Syy
sudo pacman -S libinput-rs
```

The package replaces `libinput` and does not run a resident companion service.
Resolution-aware motion, scrolling,
tapping, click mapping, and disable-while-typing run in the shared backend used
by the compositor. This avoids a second process exclusively grabbing a
mixed-capability device and prevents two independent input state machines from
competing.

To restore the distribution's original runtime package:

```bash
sudo pacman -S libinput
```

## Build on Arch

```bash
sudo pacman -S --needed base-devel rust cargo gcc-fortran meson ninja patch \
  libevdev mtdev systemd pkgconf python-libevdev python-pyudev python-yaml \
  curl rpm-tools
make all
make check
make test
```

The command-line tools are built under `target/release/`. The ABI library is
built at `target/release/libinput.so`.

### Elantech transport recovery

Elantech v4 controllers may expose a touchpad through PS/2 and SMBus. Keep the
kernel's automatic transport selection unless the journal specifically shows a
failed SMBus handoff. On a machine where SMBus probing itself is the failure,
the PS/2 path can be forced with:

```bash
sudoedit /etc/default/grub
# Add psmouse.elantech_smbus=0 to GRUB_CMDLINE_LINUX_DEFAULT, then run:
sudo grub-mkconfig -o /boot/grub/grub.cfg
sudo reboot
```

Confirm the workaround after reboot with
`cat /sys/module/psmouse/parameters/elantech_smbus`; it should print `0`.

ThinkPad P53 systems exposing the affected `LEN0408` Elantech v4 controller
should not force SMBus off. The Arch package installs a narrowly matched udev rule that
keeps the PS/2 driver's packet CRC validation enabled as a fallback across boot
and hotplug. This rejects corrupted combined TrackPoint, button, and touchpad
packets before they reach userspace.

The P53's I2C controller can also remain enumerated while silently ceasing to
deliver kernel events. `sudo libinput elan-recover` safely discovers only
devices already bound to `elan_i2c`, unbinds and rebinds each controller, and
waits for its evdev nodes to return. The Arch package enables a non-resident systemd
sleep unit that runs this recovery after resume only when DMI identifies a
ThinkPad P53. It does not open or grab input devices and exits immediately.

The shared backend normalizes touchpad movement with the kernel-reported axis
resolution while preserving libinput's separate accelerated and unaccelerated
coordinate channels. Runtime settings use the standard libinput configuration
API, so existing compositor and desktop preferences continue to apply.

## Replacement layout

The Arch package installs `libinput.so.10` in the system linker path together
with `libinput.h`, the unversioned linker name, `libinput.pc`, and the libinput
udev callouts and rules.

The udev backend enumerates `/dev/input/event*` by directory entry and delegates
the first device open to the compositor's `open_restricted` callback. This keeps
startup discovery compatible with logind-managed permissions where the
compositor can list device nodes but cannot open them directly.

Discovery is fused in `src/hwdetect.rs`: a raw post-udev netlink socket,
inotify, periodic reconciliation, and direct reads of the udev property
database identify candidates without opening them. After restricted-open
succeeds, ioctl capabilities and sysfs bitmap fallbacks are combined by the
Fortran `capforge` kernel. Rust and Fortran share classifier regression vectors.
`src/evtrans.rs` centralizes mixed KEY/BTN routing, per-code seat counts, and
exactly-once button transitions while libevdev retains packet framing and
SYN_DROPPED recovery.

### Quirk and hardware-profile resolution

Each context loads one lexically ordered snapshot of the installed quirks
tree. A device probe contains its kernel identity and raw udev roles. Matching
sections produce one immutable applied-quirks object; `AttrEventCode` and
`AttrInputProp` mutations then feed the only runtime classifier. The resulting
object is retained by the tracked device and seeds motion, click, palm, thumb,
tablet, switch, and integration behavior before `DEVICE_ADDED` is queued.
Unknown required fields fail the parity gate. A local `AttrDeviceClass` hint is
accepted only when the kernel capability lattice supports that class, so a
profile cannot fabricate input capabilities.

The chwd-style inspection tool shows the deterministic hardware profile plan:

```bash
libinput-rs-chwd --auto
libinput-rs-chwd --list-profiles
libinput-rs-chwd --identify /dev/input/event6
```

Hard DMI, identity, udev, and capability predicates take precedence. The
Fortran k-nearest-neighbor and tiny-MLP scorers rank equally eligible profiles
and may label an otherwise unmatched device only when their result agrees with
the deterministic capability class. Statistical scoring never adds a kernel
capability or overrides a hard profile.

## Formal safety models

The fail-open and restricted-discovery state machines are modeled three ways
under `proofs/`:

- Agda proves that a grab cannot be authorized while the output sink is absent
  and that listed event nodes remain discoverable without direct-open access,
  while also proving balanced physical-button transitions;
- Idris 2 uses indexed states and total transitions so invalid runtime and
  restricted-open states are unconstructable;
- Fortran provides independently compiled executable reference models for
  fail-open grabbing, permission-independent discovery, exactly-once descriptor
  closure, udev-only hotplug, and balanced physical-button lifecycles.

`HwDetect.agda` additionally proves the device lifecycle and capability-lattice
join laws. `HwSpec.idr` makes hardware classification total and keeps ignored
or unclassifiable devices out of the live registry by type.

On Arch, install the formal verification toolchains when needed:

```bash
sudo pacman -S --needed agda idris2 gcc-fortran
make proofs
```

`make proofs-strict` requires all three compilers and runs every model.

## Publishing

The Arch package and crates.io release procedure is documented in
[RELEASING.md](RELEASING.md). Crates.io publishes one `libinput-rs` source
crate; system replacement installations should use the Sisyphus package.

## Reference behavior

The replacement is pinned to upstream libinput 1.31.3 and tested through its
public C ABI. The Arch package also builds and installs the upstream 1.31.3 utility,
manual-page, completion, udev-callout, and quirks payload alongside the Rust
runtime and development files. `make packaging-check` and the package build
verify the installed payload, loader resolution, dependencies, hardening, and
an external C consumer before publication.

## License

MIT
