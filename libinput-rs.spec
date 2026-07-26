Name:           libinput-rs
Version:        0.2.0
Release:        1%{?dist}
Summary:        Fail-open Rust touchpad companion

%global __provides_exclude_from ^%{_libdir}/libinput-rs/.*$

License:        MIT
URL:            https://github.com/SisyphusAeolides/libinput-rs
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.75
BuildRequires:  rust >= 1.75
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  systemd-devel
BuildRequires:  systemd-rpm-macros
BuildRequires:  libwacom-devel >= 2.18
Requires:       %{_libdir}/libinput.so.10
Requires:       systemd

%description
libinput-rs provides an optional touchpad companion daemon with fail-open
device handling. It also installs an experimental libinput ABI implementation
in an isolated directory for explicit application testing. It does not replace,
obsolete, or provide the system libinput package or its shared library.

%prep
%autosetup

%build
CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bin libinput-rs
CARGO_PROFILE_RELEASE_DEBUG=2 ./build-shared.sh

%install
install -Dm755 target/release/libinput-rs %{buildroot}%{_bindir}/libinput-rs
install -Dm644 src/config.json %{buildroot}%{_sysconfdir}/libinput-rs/config.json
install -Dm644 systemd/libinput-rs.service %{buildroot}%{_unitdir}/libinput-rs.service
install -Dm755 target/release/libinput.so %{buildroot}%{_libdir}/libinput-rs/libinput.so.10
install -Dm644 packaging/libinput-rs.8 %{buildroot}%{_mandir}/man8/libinput-rs.8

%check
CARGO_NET_OFFLINE=true cargo test --frozen
test ! -e %{buildroot}%{_libdir}/libinput.so.10

%post
%systemd_post libinput-rs.service

%preun
%systemd_preun libinput-rs.service

%postun
%systemd_postun_with_restart libinput-rs.service

%files
%license LICENSE
%doc README.md proofs/README.md
%{_bindir}/libinput-rs
%config(noreplace) %{_sysconfdir}/libinput-rs/config.json
%{_unitdir}/libinput-rs.service
%dir %{_libdir}/libinput-rs
%{_libdir}/libinput-rs/libinput.so.10
%{_mandir}/man8/libinput-rs.8*

%changelog
* Sun Jul 26 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.0-1
- Convert packaging and CI to DNF and RPM
- Preserve the system libinput library and isolate the experimental ABI build
- Make touchpad grabbing fail open and remove display-manager ordering
