Name:           libinput-rs
Version:        0.2.0
Release:        1%{?dist}
Summary:        Fail-open Rust touchpad companion

Provides:       libinput = %{version}-%{release}
Provides:       libinput%{?_isa} = %{version}-%{release}
Provides:       libinput-devel = %{version}-%{release}
Obsoletes:      libinput < 0.2.0-1
Obsoletes:      libinput-devel < 0.2.0-1

License:        MIT AND Unicode-3.0
URL:            https://github.com/SisyphusAeolides/libinput-rs
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.75
BuildRequires:  rust >= 1.75
BuildRequires:  Agda
BuildRequires:  idris2
BuildRequires:  gcc
BuildRequires:  gcc-gfortran
BuildRequires:  make
BuildRequires:  pkgconfig(libudev)
BuildRequires:  systemd-devel
BuildRequires:  systemd-rpm-macros
BuildRequires:  libwacom-devel >= 2.18
Requires:       systemd

%description
libinput-rs provides an optional touchpad companion daemon with fail-open
device handling. It also installs a libinput ABI implementation
that replaces the system libinput package and its shared library.

%prep
%autosetup

%build
%set_build_flags
CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bin libinput-rs
CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 RPM_LD_FLAGS="%{build_ldflags}" ./build-shared.sh

%install
install -Dm755 target/release/libinput-rs %{buildroot}%{_bindir}/libinput-rs
install -Dm644 src/config.json %{buildroot}%{_sysconfdir}/libinput-rs/config.json
install -Dm644 systemd/libinput-rs.service %{buildroot}%{_unitdir}/libinput-rs.service
install -Dm755 target/release/libinput.so %{buildroot}%{_libdir}/libinput.so.10.13.0
ln -s libinput.so.10.13.0 %{buildroot}%{_libdir}/libinput.so.10
ln -s libinput.so.10 %{buildroot}%{_libdir}/libinput.so
install -Dm644 packaging/libinput.h %{buildroot}%{_includedir}/libinput.h
install -d %{buildroot}%{_libdir}/pkgconfig
sed 's|@LIBDIR@|%{_libdir}|g' packaging/libinput-rs.pc.in \
    > %{buildroot}%{_libdir}/pkgconfig/libinput.pc
install -Dm644 packaging/libinput-rs.8 %{buildroot}%{_mandir}/man8/libinput-rs.8

%check
CARGO_NET_OFFLINE=true cargo test --frozen
make proofs-strict
test -e %{buildroot}%{_libdir}/libinput.so.10

%post
%systemd_post libinput-rs.service

%preun
%systemd_preun libinput-rs.service

%postun
%systemd_postun_with_restart libinput-rs.service

%files
%license LICENSE
%license vendor/*/LICENSE*
%license vendor/*/COPYING
%doc README.md proofs/README.md
%{_bindir}/libinput-rs
%config(noreplace) %{_sysconfdir}/libinput-rs/config.json
%{_unitdir}/libinput-rs.service
%{_libdir}/libinput.so.10.13.0
%{_libdir}/libinput.so.10
%{_libdir}/libinput.so
%{_includedir}/libinput.h
%{_libdir}/pkgconfig/libinput.pc
%{_mandir}/man8/libinput-rs.8*

%changelog
* Sun Jul 26 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.0-1
- Convert packaging and CI to DNF and RPM
- Validate safety models with Rust, Fortran, Idris 2, and Agda toolchains
- Preserve the system libinput library and isolate the experimental ABI build
- Make touchpad grabbing fail open and remove display-manager ordering
