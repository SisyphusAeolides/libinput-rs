PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
UNITDIR ?= $(PREFIX)/lib/systemd/system
PRESETDIR ?= $(PREFIX)/lib/systemd/system-preset
DESTDIR ?=
FC = gfortran
REFERENCE_LIBINPUT ?= /usr/lib64/libinput.so.10
RPM_RUNTIME ?=
RPM_TOPDIR ?= $(HOME)/rpmbuild
PACKAGE_NAME := $(shell rpmspec -q --srpm --qf '%{NAME}' libinput-rs.spec 2>/dev/null)
PACKAGE_VERSION := $(shell rpmspec -q --srpm --qf '%{VERSION}' libinput-rs.spec 2>/dev/null)
SOURCE_ARCHIVE := $(RPM_TOPDIR)/SOURCES/$(PACKAGE_NAME)-$(PACKAGE_VERSION).tar.gz

.PHONY: all build shared check packaging-check crate-package-check main-crate-package-check source-archive srpm rpm-package-check test abi-check proofs proofs-strict install

all: build shared

build:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bins

shared:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 ./build-shared.sh

check: packaging-check
	cargo check --locked --workspace
	cargo clippy --locked --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check

packaging-check:
	grep -Eq '^Before=.*display-manager|^DefaultDependencies=no|^Restart=always' systemd/libinput-rs.service
	grep -qx 'enable libinput-rs.service' systemd/90-libinput-rs.preset
	grep -Eq '^Provides: *libinput( |%)' libinput-rs.spec
	grep -Eq '^Obsoletes: *libinput ' libinput-rs.spec
	grep -Eq '^Provides: *libinput-devel( |%)' libinput-rs.spec
	grep -Eq '^Obsoletes: *libinput-devel ' libinput-rs.spec
	! grep -q '^%package devel' libinput-rs.spec
	! grep -q '^%files devel' libinput-rs.spec
	grep -q '%{_presetdir}/90-libinput-rs.preset' libinput-rs.spec
	grep -q '%{_libdir}/libinput.so.10.13.0' libinput-rs.spec
	grep -q '%{_libdir}/libinput.so.10' libinput-rs.spec
	grep -q '%{_includedir}/libinput.h' libinput-rs.spec
	grep -q '%{_libdir}/pkgconfig/libinput.pc' libinput-rs.spec
	grep -q '%{_prefix}/lib/udev/libinput-fuzz-to-zero' libinput-rs.spec
	grep -q '%{_udevrulesdir}/90-libinput-fuzz-override.rules' libinput-rs.spec
	grep -q '%{_datadir}/libinput/\*.quirks' libinput-rs.spec
	grep -Eq '^install .*%\{_libdir\}/libinput\.so\.10' libinput-rs.spec
	! grep -Eq '^BuildRequires: *(Agda|idris2|gcc-gfortran)' libinput-rs.spec
	grep -Fq 'default = []' Cargo.toml
	grep -Fq 'libwacom = []' Cargo.toml
	grep -Fq '#[cfg(feature = "libwacom")]' src/backend.rs
	grep -Fq 'native_libraries+=(-lwacom)' build-shared.sh
	grep -Fq '%if 0%{?fedora}' libinput-rs.spec
	grep -Fq '%global cargo_features --features libwacom' libinput-rs.spec
	grep -Fq 'BuildRequires:  libwacom-devel >= 2.18' libinput-rs.spec
	test "$(PACKAGE_VERSION)" = "$$(awk '/^\[package\]/{package=1; next} package && /^version = /{gsub(/[" ]/, "", $$3); print $$3; exit}' Cargo.toml)"
	test -f packaging/libinput.h
	test -f packaging/libinput-rs.pc.in
	test -f packaging/libinput-rs-smoke.c
	test -f packaging/rpmlintrc
	test -x scripts/verify-rpm-package.sh

crate-package-check:
	cargo metadata --locked --offline --no-deps >/dev/null
	! grep -q '^publish = false' Cargo.toml
	grep -Eq '^evdev-upstream = \{ package = "evdev", version = "=0\.13\.2" \}$$' Cargo.toml
	cargo metadata --locked --offline --no-deps --format-version 1 | \
		grep -Eq '"workspace_members":\["[^"]*libinput-rs#[^"]*"\]'

main-crate-package-check:
	cd /tmp && cargo package --locked --no-verify --allow-dirty --package libinput-rs \
		--manifest-path "$(CURDIR)/Cargo.toml"
	cd /tmp && ! cargo package --locked --list --allow-dirty --package libinput-rs \
		--manifest-path "$(CURDIR)/Cargo.toml" | grep -Eq '/(vendor|\.cargo|rpmbuild)'

source-archive:
	mkdir -p "$(RPM_TOPDIR)/SOURCES"
	tar --exclude='./target' --exclude='./rpmbuild' --exclude='./rpmbuild2' \
		--exclude='./proofs/fortran/build' --exclude='./.git' \
		--transform='s|^\./|$(PACKAGE_NAME)-$(PACKAGE_VERSION)/|' \
		-czf "$(SOURCE_ARCHIVE)" .

srpm: source-archive
	mkdir -p "$(RPM_TOPDIR)/SRPMS"
	rpmbuild -bs libinput-rs.spec --define "_topdir $(RPM_TOPDIR)"

rpm-package-check:
	test -n "$(RPM_RUNTIME)"
	scripts/verify-rpm-package.sh "$(RPM_RUNTIME)"

test:
	cargo test --locked --workspace

abi-check: shared
	scripts/check-abi.sh "$(REFERENCE_LIBINPUT)" target/release/libinput.so

proofs:
	cd proofs/agda && agda ButtonLifecycle.agda
	cd proofs/agda && agda FailOpen.agda
	cd proofs/agda && agda ResourceLifecycle.agda
	cd proofs/agda && agda RestrictedDiscovery.agda
	cd proofs/idris && idris2 --check ButtonLifecycle.idr
	cd proofs/idris && idris2 --check FailOpen.idr
	cd proofs/idris && idris2 --check ResourceLifecycle.idr
	cd proofs/idris && idris2 --check RestrictedDiscovery.idr
	mkdir -p proofs/fortran/build
	$(FC) -std=f2018 -Wall -Wextra -Werror -fcheck=all \
		-J proofs/fortran/build -o proofs/fortran/build/fail-open \
		proofs/fortran/fail_open.f90
	proofs/fortran/build/fail-open
	$(FC) -std=f2018 -Wall -Wextra -Werror -fcheck=all \
		-J proofs/fortran/build -o proofs/fortran/build/resource-lifecycle \
		proofs/fortran/resource_lifecycle.f90
	proofs/fortran/build/resource-lifecycle
	$(FC) -std=f2018 -Wall -Wextra -Werror -fcheck=all \
		-J proofs/fortran/build -o proofs/fortran/build/restricted-discovery \
		proofs/fortran/restricted_discovery.f90
	proofs/fortran/build/restricted-discovery
	$(FC) -std=f2018 -Wall -Wextra -Werror -fcheck=all \
		-J proofs/fortran/build -o proofs/fortran/build/button-lifecycle \
		proofs/fortran/button_lifecycle.f90
	proofs/fortran/build/button-lifecycle

proofs-strict:
	command -v agda >/dev/null
	command -v idris2 >/dev/null
	command -v "$(FC)" >/dev/null
	$(MAKE) proofs FC="$(FC)"

install: all
	install -Dm755 target/release/libinput-rs $(DESTDIR)$(PREFIX)/bin/libinput-rs
	install -Dm755 target/release/libinput-device-group $(DESTDIR)$(PREFIX)/lib/udev/libinput-device-group
	install -Dm755 target/release/libinput-fuzz-extract $(DESTDIR)$(PREFIX)/lib/udev/libinput-fuzz-extract
	install -Dm755 target/release/libinput-fuzz-to-zero $(DESTDIR)$(PREFIX)/lib/udev/libinput-fuzz-to-zero
	install -Dm644 packaging/80-libinput-device-groups.rules $(DESTDIR)$(PREFIX)/lib/udev/rules.d/80-libinput-device-groups.rules
	install -Dm644 packaging/90-libinput-fuzz-override.rules $(DESTDIR)$(PREFIX)/lib/udev/rules.d/90-libinput-fuzz-override.rules
	install -d $(DESTDIR)$(PREFIX)/share/libinput
	install -m644 quirks/*.quirks $(DESTDIR)$(PREFIX)/share/libinput/
	install -Dm644 src/config.json $(DESTDIR)/etc/libinput-rs/config.json
	install -Dm644 systemd/libinput-rs.service $(DESTDIR)$(UNITDIR)/libinput-rs.service
	install -Dm644 systemd/90-libinput-rs.preset $(DESTDIR)$(PRESETDIR)/90-libinput-rs.preset
	install -Dm755 target/release/libinput.so $(DESTDIR)$(LIBDIR)/libinput.so.10.13.0
	ln -sf libinput.so.10.13.0 $(DESTDIR)$(LIBDIR)/libinput.so.10
	ln -sf libinput.so.10 $(DESTDIR)$(LIBDIR)/libinput.so
	install -Dm644 packaging/libinput.h $(DESTDIR)$(PREFIX)/include/libinput.h
	install -d $(DESTDIR)$(LIBDIR)/pkgconfig
	sed 's|@LIBDIR@|$(LIBDIR)|g' packaging/libinput-rs.pc.in > $(DESTDIR)$(LIBDIR)/pkgconfig/libinput.pc
	install -Dm644 packaging/libinput-rs.8 $(DESTDIR)$(PREFIX)/share/man/man8/libinput-rs.8
