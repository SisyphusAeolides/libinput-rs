PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
UNITDIR ?= $(PREFIX)/lib/systemd/system
DESTDIR ?=
FC = gfortran
REFERENCE_LIBINPUT ?= /usr/lib64/libinput.so.10
RPM_RUNTIME ?=
RPM_DEVEL ?=

.PHONY: all build shared check packaging-check crate-package-check rpm-devel-check test abi-check proofs proofs-strict install

all: build shared

build:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bin libinput-rs

shared:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 ./build-shared.sh

check: packaging-check
	cargo check --locked
	cargo clippy --locked --all-targets -- -D warnings
	cargo fmt --all -- --check

packaging-check:
	grep -Eq '^Before=.*display-manager|^DefaultDependencies=no|^Restart=always' systemd/libinput-rs.service
	grep -Eq '^(Provides|Obsoletes):.*libinput' libinput-rs.spec
	! grep -q '^Requires: *libinput$$' libinput-rs.spec
	! grep -q '^%package devel' libinput-rs.spec
	grep -q '%{_libdir}/libinput.so.10.13.0' libinput-rs.spec
	grep -q '%{_libdir}/libinput.so.10' libinput-rs.spec
	grep -q '%{_includedir}/libinput.h' libinput-rs.spec
	grep -q '%{_libdir}/pkgconfig/libinput.pc' libinput-rs.spec
	grep -Eq '^install .*%\{_libdir\}/libinput\.so\.10' libinput-rs.spec
	test -f packaging/libinput.h
	test -f packaging/libinput-rs.pc.in
	test -f packaging/libinput-rs-smoke.c
	test -x scripts/verify-rpm-devel.sh

crate-package-check:
	cargo package --locked
	! cargo package --locked --list | grep -Eq '/(vendor|\.cargo)/'

rpm-devel-check:
	test -n "$(RPM_RUNTIME)"
	test -n "$(RPM_DEVEL)"
	scripts/verify-rpm-devel.sh "$(RPM_RUNTIME)" "$(RPM_DEVEL)"

test:
	cargo test --locked

abi-check: shared
	scripts/check-abi.sh "$(REFERENCE_LIBINPUT)" target/release/libinput.so

proofs:
	cd proofs/agda && agda FailOpen.agda
	cd proofs/agda && agda ResourceLifecycle.agda
	cd proofs/agda && agda RestrictedDiscovery.agda
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

proofs-strict:
	command -v agda >/dev/null
	command -v idris2 >/dev/null
	command -v "$(FC)" >/dev/null
	$(MAKE) proofs FC="$(FC)"

install: all
	install -Dm755 target/release/libinput-rs $(DESTDIR)$(PREFIX)/bin/libinput-rs
	install -Dm644 src/config.json $(DESTDIR)/etc/libinput-rs/config.json
	install -Dm644 systemd/libinput-rs.service $(DESTDIR)$(UNITDIR)/libinput-rs.service
	install -Dm755 target/release/libinput.so $(DESTDIR)$(LIBDIR)/libinput.so.10.13.0
	ln -sf libinput.so.10.13.0 $(DESTDIR)$(LIBDIR)/libinput.so.10
	ln -sf libinput.so.10 $(DESTDIR)$(LIBDIR)/libinput.so
	install -Dm644 packaging/libinput.h $(DESTDIR)$(PREFIX)/include/libinput.h
	install -d $(DESTDIR)$(LIBDIR)/pkgconfig
	sed 's|@LIBDIR@|$(LIBDIR)|g' packaging/libinput-rs.pc.in > $(DESTDIR)$(LIBDIR)/pkgconfig/libinput.pc
	install -Dm644 packaging/libinput-rs.8 $(DESTDIR)$(PREFIX)/share/man/man8/libinput-rs.8
