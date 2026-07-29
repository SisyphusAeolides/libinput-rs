# Releasing libinput-rs

## Preflight

Run every local gate before publishing:

```bash
make check
make test
make proofs-strict
make shared
make abi-check
make crate-package-check
make packaging-check
```

Run the pinned upstream public-ABI behavioral suite from the release test VM
with `LIBINPUT_RS_PARITY_REPORT` set. An unfiltered release run must report all
23,245 pinned cases, zero failures, and a passing status. Keep the generated
candidate-hash-bound report with the CI artifacts; a focused or shortened run
is not release evidence.

The release version must match in `Cargo.toml`, `Cargo.lock`, and
`libinput-rs.spec`.

## Build the source RPM

```bash
rpmdev-setuptree
make srpm
```

The resulting file is written below `$HOME/rpmbuild/SRPMS/`. The 0.3.1 RPM
contains one shared input engine and no resident input companion. Its P53-only
ELAN resume recovery is a root one-shot that never opens or grabs evdev nodes.

## Publish to COPR

Install the COPR client:

```bash
sudo dnf install copr-cli
```

Download your API configuration from the COPR API page into
`~/.config/copr`, then verify authentication:

```bash
chmod 600 ~/.config/copr
copr-cli whoami
```

Create the project if it does not already exist:

```bash
copr-cli create libinput-rs \
  --chroot epel-9-x86_64 \
  --chroot epel-10-x86_64 \
  --chroot fedora-44-x86_64 \
  --chroot fedora-rawhide-x86_64 \
  --chroot rhel-9-x86_64 \
  --chroot rhel-10-x86_64 \
  --description "Rust drop-in replacement for libinput" \
  --instructions "Install from a text console or SSH-capable system. The ABI replacement is active after clients restart."
```

Submit the source RPM:

```bash
copr-cli build libinput-rs "$HOME/rpmbuild/SRPMS/libinput-rs-0.3.1-1.fc45.src.rpm"
```

Agda and Idris 2 are verified separately in Fedora CI and are not COPR build
dependencies. GNU Fortran is an RPM build dependency for the native capability
bitmap kernel. Fedora RPMs enable the optional libwacom integration; EPEL and
RHEL RPMs use the portable fallback and do not require libwacom.

After the build succeeds, verify that the runtime RPM contains the three
callouts below `/usr/lib/udev/`, all libinput udev rule files, and the quirks
database below `/usr/share/libinput/`. Confirm that it contains no resident
input companion and that the ELAN recovery unit is DMI-gated, then test
installation and rollback from COPR before announcing the repository.

## Publish to crates.io

Authenticate once with a crates.io API token. Run `cargo login` and paste the
token at its prompt so it is not written into shell history:

```bash
cargo login
```

Validate and publish the single source package:

```bash
make main-crate-package-check
cargo publish --dry-run --locked --package libinput-rs --registry crates-io
cargo publish --locked --package libinput-rs --registry crates-io
```

The crates.io package distributes Rust source. System replacement users should
install the single RPM from COPR.

## Tag the release

After both publication paths succeed:

```bash
git tag -s v0.3.1 -m "libinput-rs 0.3.1"
git push origin v0.3.1
```
