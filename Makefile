PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
DESTDIR ?=
FC = gfortran
IDRIS2 ?= idris2
AGDA ?= agda
REFERENCE_LIBINPUT ?= /usr/lib64/libinput.so.10
RPM_RUNTIME ?=
RPM_TOPDIR ?= $(HOME)/rpmbuild
PACKAGE_NAME := $(or $(shell rpmspec -q --srpm --qf '%{NAME}' libinput-rs.spec 2>/dev/null),$(shell awk '/^Name:/ { print $$2; exit }' libinput-rs.spec))
PACKAGE_VERSION := $(or $(shell rpmspec -q --srpm --qf '%{VERSION}' libinput-rs.spec 2>/dev/null),$(shell awk '/^Version:/ { print $$2; exit }' libinput-rs.spec))
SOURCE_ARCHIVE := $(RPM_TOPDIR)/SOURCES/$(PACKAGE_NAME)-$(PACKAGE_VERSION).tar.gz
UPSTREAM_TOOLS_URL := $(shell rpmspec -P libinput-rs.spec 2>/dev/null | awk '/^Source1:/ { print $$2; exit }')
UPSTREAM_TOOLS_SHA256 := $(shell awk '$$1 == "%global" && $$2 == "libinput_tools_sha256" { print $$3; exit }' libinput-rs.spec)
UPSTREAM_TOOLS_ARCHIVE := $(RPM_TOPDIR)/SOURCES/$(notdir $(UPSTREAM_TOOLS_URL))
UPSTREAM_TOOLS_ROOT := target/upstream-tools
UPSTREAM_TOOLS_SOURCE_DIR := $(UPSTREAM_TOOLS_ROOT)/source
UPSTREAM_TOOLS_BUILD_DIR := $(UPSTREAM_TOOLS_ROOT)/build
UPSTREAM_TOOLS_STAGE_DIR := $(UPSTREAM_TOOLS_ROOT)/stage

.PHONY: all build shared check packaging-check crate-package-check main-crate-package-check source-archive upstream-tools-source upstream-tools srpm rpm-package-check deb ppa-source ppa-source-unsigned test abi-check proofs proofs-strict install

all: build shared

deb:
	dpkg-buildpackage --build=binary --no-sign

ppa-source:
	sh scripts/build-deb-source.sh

ppa-source-unsigned:
	sh scripts/build-deb-source.sh --unsigned

build:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bins

shared:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 ./build-shared.sh

check: packaging-check
	cargo check --locked --workspace
	cargo clippy --locked --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check

packaging-check:
	grep -Eq '^Provides: *libinput( |%)' libinput-rs.spec
	grep -Eq '^Obsoletes: *libinput ' libinput-rs.spec
	grep -Eq '^Provides: *libinput-devel( |%)' libinput-rs.spec
	grep -Eq '^Obsoletes: *libinput-devel ' libinput-rs.spec
	! grep -q '^%package devel' libinput-rs.spec
	! grep -q '^%files devel' libinput-rs.spec
	! test -e systemd/libinput-rs.service
	! test -e systemd/90-libinput-rs.preset
	test -f systemd/libinput-rs-elan-resume.service
	test -f systemd/91-libinput-rs-elan.preset
	grep -q '%{_libdir}/libinput.so.10.13.0' libinput-rs.spec
	grep -q '%{_libdir}/libinput.so.10' libinput-rs.spec
	grep -q '%{_includedir}/libinput.h' libinput-rs.spec
	grep -q '%{_libdir}/pkgconfig/libinput.pc' libinput-rs.spec
	grep -q '%{_prefix}/lib/udev/libinput-fuzz-to-zero' libinput-rs.spec
	grep -q '%{_udevrulesdir}/90-libinput-fuzz-override.rules' libinput-rs.spec
	grep -q '%{_udevrulesdir}/90-libinput-rs-elantech-crc.rules' libinput-rs.spec
	grep -q '%{_unitdir}/libinput-rs-elan-resume.service' libinput-rs.spec
	grep -q '%{_presetdir}/91-libinput-rs-elan.preset' libinput-rs.spec
	grep -q '%{_datadir}/libinput/\*.quirks' libinput-rs.spec
	grep -q '%{_bindir}/libinput' libinput-rs.spec
	grep -q '%{_bindir}/libinput-rs-chwd' libinput-rs.spec
	grep -q '%{_libexecdir}/libinput/libinput-\*' libinput-rs.spec
	grep -q '^Source1:.*%{libinput_tools_commit}' libinput-rs.spec
	grep -q '^%global libinput_tools_commit 26191d396d74d505541d6311f0b4ae68d791b890' libinput-rs.spec
	grep -q '^%global libinput_tools_sha256 d5d8c8464f9cb24b0897c03edfe7d7c9e75ff5a91fe9b5b48791781aa9642858' libinput-rs.spec
	grep -q 'libinput-tool' libinput-rs.spec
	grep -q 'libinput-replay' libinput-rs.spec
	test -f packaging/libinput-1.31.3-meson-0.63.patch
	grep -q 'libinput-1.31.3-meson-0.63.patch' libinput-rs.spec
	grep -Eq '^BuildRequires: +patch' libinput-rs.spec
	! test -e packaging/libinput.1
	! test -e packaging/_libinput
	grep -Eq '^Requires: +python3-libevdev' libinput-rs.spec
	grep -Eq '^Requires: +python3-pyudev' libinput-rs.spec
	grep -Eq '^Requires: +python3-pyyaml' libinput-rs.spec
	grep -Eq '^install .*%\{_libdir\}/libinput\.so\.10' libinput-rs.spec
	! grep -Eq '^BuildRequires: *(Agda|idris2)' libinput-rs.spec
	grep -Eq '^BuildRequires: *gcc-gfortran' libinput-rs.spec
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
	grep -Fq 'plugindir=$${libdir}/libinput/plugins' packaging/libinput-rs.pc.in
	grep -Fq 'Name: Libinput' packaging/libinput-rs.pc.in
	test -f packaging/libinput-rs-smoke.c
	test -f packaging/rpmlintrc
	test -x scripts/verify-rpm-package.sh
	test -f debian/control
	test -f debian/changelog
	test -x debian/rules
	test -x scripts/build-deb-source.sh
	grep -q '^Package: libinput10$$' debian/control
	grep -q '^Package: libinput-bin$$' debian/control
	grep -q '^Package: libinput-dev$$' debian/control
	grep -q '^Package: libinput-tools$$' debian/control
	grep -q '^Package: libinput-rs$$' debian/control

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
		--exclude='./audit.py' --exclude='./fix_compile.py' \
		--transform='s|^\./|$(PACKAGE_NAME)-$(PACKAGE_VERSION)/|' \
		-czf "$(SOURCE_ARCHIVE)" .

upstream-tools-source:
	mkdir -p "$(RPM_TOPDIR)/SOURCES"
	@if ! echo "$(UPSTREAM_TOOLS_SHA256)  $(UPSTREAM_TOOLS_ARCHIVE)" | sha256sum -c --status 2>/dev/null; then \
		curl --fail --location --retry 3 --output "$(UPSTREAM_TOOLS_ARCHIVE).part" "$(UPSTREAM_TOOLS_URL)"; \
		echo "$(UPSTREAM_TOOLS_SHA256)  $(UPSTREAM_TOOLS_ARCHIVE).part" | sha256sum -c -; \
		mv "$(UPSTREAM_TOOLS_ARCHIVE).part" "$(UPSTREAM_TOOLS_ARCHIVE)"; \
	fi
	echo "$(UPSTREAM_TOOLS_SHA256)  $(UPSTREAM_TOOLS_ARCHIVE)" | sha256sum -c -

$(UPSTREAM_TOOLS_SOURCE_DIR)/.stamp: | upstream-tools-source
	mkdir -p "$(UPSTREAM_TOOLS_SOURCE_DIR)"
	tar -xzf "$(UPSTREAM_TOOLS_ARCHIVE)" --strip-components=1 -C "$(UPSTREAM_TOOLS_SOURCE_DIR)"
	patch -d "$(UPSTREAM_TOOLS_SOURCE_DIR)" -p1 < packaging/libinput-1.31.3-meson-0.63.patch
	touch "$@"

$(UPSTREAM_TOOLS_BUILD_DIR)/.stamp: $(UPSTREAM_TOOLS_SOURCE_DIR)/.stamp
	meson setup "$(UPSTREAM_TOOLS_BUILD_DIR)" "$(UPSTREAM_TOOLS_SOURCE_DIR)" \
		--buildtype=release --prefix="$(PREFIX)" --libdir="$(notdir $(LIBDIR))" \
		-Dtests=false -Ddocumentation=false -Ddebug-gui=false \
		-Dlibwacom=false -Dlua-plugins=disabled
	meson compile -C "$(UPSTREAM_TOOLS_BUILD_DIR)"
	touch "$@"

$(UPSTREAM_TOOLS_STAGE_DIR)/.stamp: $(UPSTREAM_TOOLS_BUILD_DIR)/.stamp
	mkdir -p "$(UPSTREAM_TOOLS_STAGE_DIR)"
	DESTDIR="$(abspath $(UPSTREAM_TOOLS_STAGE_DIR))" \
		meson install -C "$(UPSTREAM_TOOLS_BUILD_DIR)" --no-rebuild
	touch "$@"

upstream-tools: $(UPSTREAM_TOOLS_STAGE_DIR)/.stamp

srpm: source-archive upstream-tools-source
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
	IDRIS2="$(IDRIS2)" AGDA="$(AGDA)" FC="$(FC)" scripts/check-formal.sh

proofs-strict:
	command -v "$(AGDA)" >/dev/null
	command -v "$(IDRIS2)" >/dev/null
	command -v "$(FC)" >/dev/null
	$(MAKE) proofs FC="$(FC)"

install: all upstream-tools
	install -Dm755 target/release/libinput $(DESTDIR)$(PREFIX)/bin/libinput
	ln -sf libinput $(DESTDIR)$(PREFIX)/bin/libinput-rs
	install -Dm755 target/release/libinput-rs-chwd $(DESTDIR)$(PREFIX)/bin/libinput-rs-chwd
	install -d $(DESTDIR)$(PREFIX)/libexec/libinput
	install -Dm755 $(UPSTREAM_TOOLS_STAGE_DIR)$(PREFIX)/bin/libinput \
		$(DESTDIR)$(PREFIX)/libexec/libinput/libinput-tool
	for helper in $(UPSTREAM_TOOLS_STAGE_DIR)$(PREFIX)/libexec/libinput/libinput-*; do \
		test "$$(basename "$$helper")" = libinput-test && continue; \
		install -Dm755 "$$helper" $(DESTDIR)$(PREFIX)/libexec/libinput/"$$(basename "$$helper")"; \
	done
	for helper in libinput-analyze-buttons libinput-analyze-per-slot-delta \
		libinput-analyze-recording libinput-analyze-touch-down-state \
		libinput-list-kernel-devices libinput-measure-fuzz \
		libinput-measure-touch-size libinput-measure-touchpad-pressure \
		libinput-measure-touchpad-size libinput-measure-touchpad-tap \
		libinput-replay; do \
		sed -i '1s|^#!/usr/bin/env python3$$|#!/usr/bin/python3|' \
			$(DESTDIR)$(PREFIX)/libexec/libinput/"$$helper"; \
	done
	install -Dm755 target/release/libinput-device-group $(DESTDIR)$(PREFIX)/lib/udev/libinput-device-group
	install -Dm755 target/release/libinput-fuzz-extract $(DESTDIR)$(PREFIX)/lib/udev/libinput-fuzz-extract
	install -Dm755 target/release/libinput-fuzz-to-zero $(DESTDIR)$(PREFIX)/lib/udev/libinput-fuzz-to-zero
	install -Dm644 packaging/80-libinput-device-groups.rules $(DESTDIR)$(PREFIX)/lib/udev/rules.d/80-libinput-device-groups.rules
	install -Dm644 packaging/90-libinput-fuzz-override.rules $(DESTDIR)$(PREFIX)/lib/udev/rules.d/90-libinput-fuzz-override.rules
	install -Dm644 packaging/90-libinput-rs-elantech-crc.rules $(DESTDIR)$(PREFIX)/lib/udev/rules.d/90-libinput-rs-elantech-crc.rules
	install -Dm644 systemd/libinput-rs-elan-resume.service $(DESTDIR)$(PREFIX)/lib/systemd/system/libinput-rs-elan-resume.service
	install -Dm644 systemd/91-libinput-rs-elan.preset $(DESTDIR)$(PREFIX)/lib/systemd/system-preset/91-libinput-rs-elan.preset
	install -d $(DESTDIR)$(PREFIX)/share/libinput
	install -m644 quirks/*.quirks $(DESTDIR)$(PREFIX)/share/libinput/
	install -Dm755 target/release/libinput.so $(DESTDIR)$(LIBDIR)/libinput.so.10.13.0
	ln -sf libinput.so.10.13.0 $(DESTDIR)$(LIBDIR)/libinput.so.10
	ln -sf libinput.so.10 $(DESTDIR)$(LIBDIR)/libinput.so
	install -Dm644 packaging/libinput.h $(DESTDIR)$(PREFIX)/include/libinput.h
	install -d $(DESTDIR)$(LIBDIR)/pkgconfig
	sed 's|@LIBDIR@|$(LIBDIR)|g' packaging/libinput-rs.pc.in > $(DESTDIR)$(LIBDIR)/pkgconfig/libinput.pc
	install -Dm644 packaging/libinput-rs.8 $(DESTDIR)$(PREFIX)/share/man/man8/libinput-rs.8
	install -Dm644 packaging/libinput-rs-chwd.8 $(DESTDIR)$(PREFIX)/share/man/man8/libinput-rs-chwd.8
	for manpage in $(UPSTREAM_TOOLS_STAGE_DIR)$(PREFIX)/share/man/man1/libinput*.1; do \
		test "$$(basename "$$manpage")" = libinput-test.1 && continue; \
		install -Dm644 "$$manpage" $(DESTDIR)$(PREFIX)/share/man/man1/"$$(basename "$$manpage")"; \
	done
	install -Dm644 $(UPSTREAM_TOOLS_STAGE_DIR)$(PREFIX)/share/zsh/site-functions/_libinput \
		$(DESTDIR)$(PREFIX)/share/zsh/site-functions/_libinput
