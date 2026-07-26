PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib64
UNITDIR ?= $(PREFIX)/lib/systemd/system
DESTDIR ?=
AUSTRAL ?= austral

.PHONY: all build shared check packaging-check test proofs proofs-strict install

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

proofs:
	cd proofs/agda && agda FailOpen.agda
	cd proofs/idris && idris2 --check FailOpen.idr
	@if command -v "$(AUSTRAL)" >/dev/null 2>&1; then \
		cd proofs/austral && "$(AUSTRAL)" compile --target-type=tc FailOpen.aui,FailOpen.aum; \
	else \
		echo "Austral compiler not found; Agda and Idris proofs passed"; \
	fi

proofs-strict:
	command -v agda >/dev/null
	command -v idris2 >/dev/null
	command -v "$(AUSTRAL)" >/dev/null
	$(MAKE) proofs AUSTRAL="$(AUSTRAL)"

install: all
	install -Dm755 target/release/libinput-rs $(DESTDIR)$(PREFIX)/bin/libinput-rs
	install -Dm644 src/config.json $(DESTDIR)/etc/libinput-rs/config.json
	install -Dm644 systemd/libinput-rs.service $(DESTDIR)$(UNITDIR)/libinput-rs.service
	install -Dm755 target/release/libinput.so $(DESTDIR)$(LIBDIR)/libinput-rs/libinput.so.10
	install -Dm644 packaging/libinput-rs.8 $(DESTDIR)$(PREFIX)/share/man/man8/libinput-rs.8
