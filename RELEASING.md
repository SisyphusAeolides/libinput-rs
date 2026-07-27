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
```

The release version must match in `Cargo.toml`, `Cargo.lock`, and
`libinput-rs.spec`.

## Build the source RPM

```bash
rpmdev-setuptree
make srpm
```

The resulting file is written below `$HOME/rpmbuild/SRPMS/`. The 0.2.1 RPM
release installs a vendor preset that enables `libinput-rs.service` on first
installation; explicit administrator enable and disable choices remain
preserved during upgrades.

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
  --instructions "Install from a text console or SSH-capable system. The companion daemon is enabled automatically on first installation and can be disabled with systemctl disable --now libinput-rs.service."
```

Submit the source RPM:

```bash
copr-cli build libinput-rs "$HOME/rpmbuild/SRPMS/libinput-rs-0.2.1-4.fc45.src.rpm"
```

Formal proof compilers are verified separately in Fedora CI and are not COPR
build dependencies. The shared library does not link to libwacom because this
implementation does not call its API.

After the build succeeds, verify that the runtime RPM contains
`/usr/lib/systemd/system-preset/90-libinput-rs.preset`, then test installation,
boot-time service activation, and rollback from COPR before announcing the
repository.

## Publish to crates.io

Authenticate once with a crates.io API token. Run `cargo login` and paste the
token at its prompt so it is not written into shell history:

```bash
cargo login
```

Package and publish the compatibility crate first:

```bash
cargo publish --dry-run --locked --package libinput-rs-evdev
cargo publish --locked --package libinput-rs-evdev
```

Wait until version `0.1.0` is visible in the crates.io index:

```bash
cargo info libinput-rs-evdev@0.1.0
```

Then validate and publish the main package:

```bash
make main-crate-package-check
cargo publish --dry-run --locked --package libinput-rs
cargo publish --locked --package libinput-rs
```

The crates.io packages distribute Rust source. System replacement users should
install the runtime and development RPMs from COPR.

## Tag the release

After both publication paths succeed:

```bash
git tag -s v0.2.1 -m "libinput-rs 0.2.1"
git push origin v0.2.1
```
