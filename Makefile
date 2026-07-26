PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
UNITDIR ?= $(PREFIX)/lib/systemd/system
DESTDIR ?=
FC = gfortran
REFERENCE_LIBINPUT ?= /usr/lib64/libinput.so.10

.PHONY: all build shared check packaging-check test abi-check proofs proofs-strict install

all: build shared

build:
	CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bin libinput-rs

shared:
	CARGO_PROFILE_RELEASE_DEBUG=2 ./build-shared.sh

check: packaging-check
	cargo check --locked
	cargo clippy --locked --all-targets -- -D warnings
	cargo fmt --all -- --check

packaging-check:
	! grep -Eq '^Before=.*display-manager|^DefaultDependencies=no|^Restart=always' systemd/libinput-rs.service
	! grep -Eq '^(Provides|Obsoletes):.*libinput' libinput-rs.spec
	grep -q '^Requires: *%{_libdir}/libinput.so.10' libinput-rs.spec
	grep -q '%{_libdir}/libinput-rs/libinput.so.10' libinput-rs.spec
	! grep -Eq '^install .*%\{_libdir\}/libinput\.so\.10' libinput-rs.spec

test:
	cargo test --locked

abi-check: shared
	scripts/check-abi.sh "$(REFERENCE_LIBINPUT)" target/release/libinput.so

proofs:
	cd proofs/agda && agda FailOpen.agda
	cd proofs/agda && agda ResourceLifecycle.agda
	cd proofs/idris && idris2 --check FailOpen.idr
	cd proofs/idris && idris2 --check ResourceLifecycle.idr
	mkdir -p proofs/fortran/build
	$(FC) -std=f2018 -Wall -Wextra -Werror -fcheck=all \
		-J proofs/fortran/build -o proofs/fortran/build/fail-open \
		proofs/fortran/fail_open.f90
	proofs/fortran/build/fail-open
	$(FC) -std=f2018 -Wall -Wextra -Werror -fcheck=all \
		-J proofs/fortran/build -o proofs/fortran/build/resource-lifecycle \
		proofs/fortran/resource_lifecycle.f90
	proofs/fortran/build/resource-lifecycle

proofs-strict:
	command -v agda >/dev/null
	command -v idris2 >/dev/null
	command -v "$(FC)" >/dev/null
	$(MAKE) proofs FC="$(FC)"

install: all
	install -Dm755 target/release/libinput-rs $(DESTDIR)$(PREFIX)/bin/libinput-rs
	install -Dm644 src/config.json $(DESTDIR)/etc/libinput-rs/config.json
	install -Dm644 systemd/libinput-rs.service $(DESTDIR)$(UNITDIR)/libinput-rs.service
	install -Dm755 target/release/libinput.so $(DESTDIR)$(LIBDIR)/libinput-rs/libinput.so.10
	install -Dm644 packaging/libinput-rs.8 $(DESTDIR)$(PREFIX)/share/man/man8/libinput-rs.8
