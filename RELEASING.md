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
with `LIBINPUT_RS_PARITY_REPORT` set. The unfiltered release result must be
exactly 23,245 completed, 12,185 pass, 11,059 private-interface N/A, one pinned
release-build skip, zero failures, and status `PASS`. Keep the generated
candidate-hash-bound report with the CI artifacts; a focused or shortened run
is not release evidence. Eight isolated workers complete the corpus without
changing its inventory or result requirements.

The release version must match in `Cargo.toml`, `Cargo.lock`, and
`libinput-rs.spec`.

## Build and publish the Arch package

The ArachOS PKGBUILD is the authoritative system package. It is kept in
`ArachOS/packaging/pkgbuild/libinput-rs/`; the ArachOS build script replaces its
source pin from `ArachOS/sources.lock` before calling `makepkg`.

Run the package and repository gates from the coordinated checkouts:

```bash
cd ../ArachOS
make verify-sources
make build-packages
make validate-packages
```

For a signed repository, provide an isolated GPG home and key to the same
build, then sign the resulting pacman database:

```bash
ARACHOS_GPG_HOME=/path/to/gpg-home \
ARACHOS_GPG_KEY_ID=<KEY-ID> \
  make build-packages sign-packages
```

The `libinput-rs` package replaces `libinput`, installs the shared ABI, tools,
headers, udev rules, quirks, and the DMI-gated Elantech recovery unit. Verify
the package with `make validate-packages` before publishing the repository
state. The GitHub Actions workflow in `Sisyphus-Repo` rebuilds only changed
PKGBUILDs, signs the packages and database, and publishes the x86_64 pacman
repository.

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
install the signed `libinput-rs` pacman package from the ArachOS/Sisyphus
repository.

## Tag the release

After both publication paths succeed:

```bash
git tag -s v0.3.1 -m "libinput-rs 0.3.1"
git push origin v0.3.1
```
