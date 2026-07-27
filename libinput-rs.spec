Name:           libinput-rs
Version:        0.2.0
Release:        1%{?dist}
Summary:        Fail-open Rust touchpad companion

%global __provides_exclude_from ^%{_libdir}/libinput-rs/.*$

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
device handling. It also installs an experimental libinput ABI implementation
in an isolated directory for explicit application testing. It does not replace,
obsolete, or provide the system libinput package or its shared library.

%package devel
Summary:        Development files for isolated libinput-rs ABI testing
Requires:       %{name}%{?_isa} = %{version}-%{release}
Requires:       pkgconfig(libudev)

%description devel
This package provides private headers, linker symbolic links, and pkg-config
data for explicit testing against libinput-rs. It does not replace the system
libinput development package.

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
install -Dm755 target/release/libinput.so %{buildroot}%{_libdir}/libinput-rs/libinput.so.10.13.0
ln -s libinput.so.10.13.0 %{buildroot}%{_libdir}/libinput-rs/libinput.so.10
ln -s libinput.so.10 %{buildroot}%{_libdir}/libinput-rs/libinput.so
install -Dm644 packaging/libinput.h %{buildroot}%{_includedir}/libinput-rs/libinput.h
install -d %{buildroot}%{_libdir}/pkgconfig
sed 's|@LIBDIR@|%{_libdir}|g' packaging/libinput-rs.pc.in \
    > %{buildroot}%{_libdir}/pkgconfig/libinput-rs.pc
install -Dm644 packaging/libinput-rs.8 %{buildroot}%{_mandir}/man8/libinput-rs.8

%check
CARGO_NET_OFFLINE=true cargo test --frozen
make proofs-strict
test ! -e %{buildroot}%{_libdir}/libinput.so.10

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
%dir %{_libdir}/libinput-rs
%{_libdir}/libinput-rs/libinput.so.10.13.0
%{_libdir}/libinput-rs/libinput.so.10
%{_mandir}/man8/libinput-rs.8*

%files devel
%dir %{_includedir}/libinput-rs
%{_includedir}/libinput-rs/libinput.h
%{_libdir}/libinput-rs/libinput.so
%{_libdir}/pkgconfig/libinput-rs.pc

%changelog
* Sun Jul 26 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.0-1
- Convert packaging and CI to DNF and RPM
- Validate safety models with Rust, Fortran, Idris 2, and Agda toolchains
- Preserve the system libinput library and isolate the experimental ABI build
- Make touchpad grabbing fail open and remove display-manager ordering
