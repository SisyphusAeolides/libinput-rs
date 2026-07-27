%global libinput_compat_version 1.31.3
%global libinput_replace_before 1.32.0

Name:           libinput-rs
Version:        0.2.1
Release:        4%{?dist}
Summary:        Rust drop-in replacement for libinput

Provides:       libinput = %{libinput_compat_version}
Provides:       libinput%{?_isa} = %{libinput_compat_version}
Obsoletes:      libinput < %{libinput_replace_before}

License:        MIT AND Unicode-3.0
URL:            https://github.com/SisyphusAeolides/libinput-rs
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.75
BuildRequires:  rust >= 1.75
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pkgconfig(libudev)
BuildRequires:  systemd-devel
BuildRequires:  systemd-rpm-macros
Requires:       systemd

%description
libinput-rs installs a Rust implementation of the libinput.so.10 ABI in the
system library path. It also includes a fail-open touchpad companion daemon
that is enabled by vendor preset on first installation.

%package devel
Summary:        Development files for the libinput-rs replacement
Requires:       %{name}%{?_isa} = %{version}-%{release}
Requires:       pkgconfig(libudev)
Provides:       libinput-devel = %{libinput_compat_version}
Provides:       libinput-devel%{?_isa} = %{libinput_compat_version}
Obsoletes:      libinput-devel < %{libinput_replace_before}

%description devel
Headers, linker name, and pkg-config metadata for developing software against
the libinput-rs implementation of the libinput ABI.

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
install -Dm644 systemd/90-libinput-rs.preset %{buildroot}%{_presetdir}/90-libinput-rs.preset
install -Dm755 target/release/libinput.so %{buildroot}%{_libdir}/libinput.so.10.13.0
ln -s libinput.so.10.13.0 %{buildroot}%{_libdir}/libinput.so.10
ln -s libinput.so.10 %{buildroot}%{_libdir}/libinput.so
install -Dm644 packaging/libinput.h %{buildroot}%{_includedir}/libinput.h
install -d %{buildroot}%{_libdir}/pkgconfig
sed 's|@LIBDIR@|%{_libdir}|g' packaging/libinput-rs.pc.in \
    > %{buildroot}%{_libdir}/pkgconfig/libinput.pc
install -Dm644 packaging/libinput-rs.8 %{buildroot}%{_mandir}/man8/libinput-rs.8

install -d %{buildroot}%{_licensedir}/%{name}/third-party
for crate in vendor/*; do
    test -d "$crate" || continue
    destination=%{buildroot}%{_licensedir}/%{name}/third-party/$(basename "$crate")
    for license in "$crate"/LICENSE* "$crate"/COPYING*; do
        test -f "$license" || continue
        install -d "$destination"
        install -m644 "$license" "$destination/"
    done
done

%check
CARGO_NET_OFFLINE=true cargo test --frozen --workspace
test -e %{buildroot}%{_libdir}/libinput.so.10

%post
%systemd_post libinput-rs.service

%preun
%systemd_preun libinput-rs.service

%postun
%systemd_postun_with_restart libinput-rs.service

%files
%license LICENSE
%license %{_licensedir}/%{name}/third-party
%doc README.md
%{_bindir}/libinput-rs
%config(noreplace) %{_sysconfdir}/libinput-rs/config.json
%{_unitdir}/libinput-rs.service
%{_presetdir}/90-libinput-rs.preset
%{_libdir}/libinput.so.10.13.0
%{_libdir}/libinput.so.10
%{_mandir}/man8/libinput-rs.8*

%files devel
%license LICENSE
%{_libdir}/libinput.so
%{_includedir}/libinput.h
%{_libdir}/pkgconfig/libinput.pc

%changelog
* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.1-4
- Make COPR builds portable across Fedora, EPEL, and RHEL chroots
- Keep formal proof compilers in CI instead of RPM build dependencies
- Remove the unused libwacom build and linker dependency
- Eliminate the duplicate README documentation entry

* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.1-3
- Enable the companion daemon on first installation with a vendor preset
- Preserve explicit administrator enable and disable choices on upgrades

* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.1-2
- Preserve the live-tested 2.2 pointer setting across normalized motion
- Interpret existing acceleration values against the 2.5 normalization reference

* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.1-1
- Normalize companion pointer and scroll motion by hardware axis resolution
- Use the live-tested motion scale when devices omit resolution metadata
- Prepare the workspace crates and RPM source package for publication

* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.0-2
- Preserve discovery of devices behind restricted-open callbacks
- Split runtime and development payloads into policy-compliant RPMs
- Validate Rust, Fortran, Idris 2, and Agda safety guarantees

* Sun Jul 26 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.0-1
- Convert packaging and CI to DNF and RPM
- Validate safety models with Rust, Fortran, Idris 2, and Agda toolchains
- Preserve the system libinput library and isolate the experimental ABI build
- Make touchpad grabbing fail open and remove display-manager ordering
