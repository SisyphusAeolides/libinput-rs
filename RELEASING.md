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

Install and configure the COPR client once:

```bash
sudo dnf install copr-cli
copr-cli create-token
```

Create the project if it does not already exist:

```bash
copr-cli create libinput-rs \
  --chroot fedora-45-x86_64 \
  --description "Rust drop-in replacement for libinput" \
  --instructions "Test from a text console or SSH session and keep the companion daemon disabled until the ABI replacement is confirmed."
```

Submit the source RPM:

```bash
copr-cli build libinput-rs "$HOME/rpmbuild/SRPMS/libinput-rs-0.2.1-1.fc45.src.rpm"
```

After the build succeeds, test installation and rollback from the COPR before
announcing the repository.

## Publish to crates.io

Authenticate once with a crates.io API token:

```bash
cargo login
```

Package and publish the compatibility crate first:

```bash
cargo publish --dry-run --locked --package libinput-rs-evdev
cargo publish --locked --package libinput-rs-evdev
```

Wait until version `0.1.0` is visible in the crates.io index, then publish the
main package:

```bash
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
