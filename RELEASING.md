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

The resulting file is written below `$HOME/rpmbuild/SRPMS/`.

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

Create the project if it does not already exist. Fedora 45 is currently the
rawhide target in COPR, so publish against Fedora 44 and Fedora rawhide:

```bash
copr-cli create libinput-rs \
  --chroot fedora-44-x86_64 \
  --chroot fedora-rawhide-x86_64 \
  --description "Rust drop-in replacement for libinput" \
  --instructions "Test from a text console or SSH session and keep the companion daemon disabled until the ABI replacement is confirmed."
```

EPEL and RHEL chroots are not enabled because they do not provide the libwacom
ABI required by this implementation.

Submit the source RPM:

```bash
copr-cli build libinput-rs "$HOME/rpmbuild/SRPMS/libinput-rs-0.2.1-2.fc45.src.rpm"
```

After the build succeeds, test installation and rollback from the COPR before
announcing the repository.

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

The crates.io packages distribute Rust source. Fedora system replacement users
should install the runtime and development RPMs from COPR.

## Tag the release

After both publication paths succeed:

```bash
git tag -s v0.2.1 -m "libinput-rs 0.2.1"
git push origin v0.2.1
```
