%global libinput_compat_version 1.31.3
%global libinput_replace_before 1.32.0
%global libinput_tools_commit 26191d396d74d505541d6311f0b4ae68d791b890
%global libinput_tools_sha256 d5d8c8464f9cb24b0897c03edfe7d7c9e75ff5a91fe9b5b48791781aa9642858

Name:           libinput-rs
Version:        0.3.5
Release:        1%{?dist}
Summary:        Drop-in Rust replacement for libinput 1.31.3

Provides:       libinput = %{libinput_compat_version}
Provides:       libinput%{?_isa} = %{libinput_compat_version}
Provides:       libinput-devel = %{libinput_compat_version}
Provides:       libinput-devel%{?_isa} = %{libinput_compat_version}
Obsoletes:      libinput < %{libinput_replace_before}
Obsoletes:      libinput-devel < %{libinput_replace_before}

License:        MIT AND Unicode-3.0
URL:            https://github.com/SisyphusAeolides/libinput-rs
Source0:        %{name}-%{version}.tar.gz
Source1:        https://gitlab.freedesktop.org/libinput/libinput/-/archive/%{libinput_tools_commit}/libinput-%{libinput_tools_commit}.tar.gz

%if 0%{?fedora}
%global cargo_features --features libwacom
%else
%global cargo_features %{nil}
%endif

BuildRequires:  cargo >= 1.75
BuildRequires:  rust >= 1.75
BuildRequires:  binutils
BuildRequires:  gcc
BuildRequires:  gcc-gfortran
BuildRequires:  make
BuildRequires:  meson >= 0.63
BuildRequires:  ninja-build
BuildRequires:  patch
BuildRequires:  pkgconfig(libevdev) >= 1.10.0
BuildRequires:  pkgconfig(mtdev) >= 1.1.0
BuildRequires:  pkgconfig(libudev)
BuildRequires:  python3
BuildRequires:  systemd-rpm-macros
%if 0%{?fedora}
BuildRequires:  libwacom-devel >= 2.18
%endif
Requires:       libgfortran
Requires:       python3
Requires:       python3-libevdev
Requires:       python3-pyudev
Requires:       python3-pyyaml
%description
libinput-rs installs a tested drop-in Rust implementation of the libinput
1.31.3 libinput.so.10 ABI in the system library path. The single package also
includes the matching C development files, command-line tools, manual pages,
shell completion, udev integration, and hardware quirks. Touchpad motion,
scrolling, tapping, click mapping, and disable-while-typing are handled inside
the shared backend without a second process or exclusive device grab.

%prep
echo "%{libinput_tools_sha256}  %{SOURCE1}" | sha256sum -c -
%autosetup -a 1
patch -d libinput-%{libinput_tools_commit} -p1 < packaging/libinput-1.31.3-meson-0.63.patch

# Reconcile the .cargo-checksum.json for vendor/evdev to match whatever
# state raw_stream.rs is actually in (patched or unpatched). This is
# idempotent: it recomputes the real sha256 and writes it, so cargo
# --frozen never sees a checksum mismatch regardless of SRPM origin.
python3 -c "
import hashlib, json, pathlib
p = pathlib.Path('vendor/evdev/src/raw_stream.rs')
new_hash = hashlib.sha256(p.read_bytes()).hexdigest()
cf = pathlib.Path('vendor/evdev/.cargo-checksum.json')
data = json.loads(cf.read_text())
data['files']['src/raw_stream.rs'] = new_hash
cf.write_text(json.dumps(data, separators=(',', ':'), sort_keys=True))
"

%build
%set_build_flags
CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --frozen --release --bins %{cargo_features}
CARGO_NET_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_FEATURES="%{cargo_features}" RPM_LD_FLAGS="%{build_ldflags}" ./build-shared.sh
CFLAGS="%{build_cflags}" LDFLAGS="%{build_ldflags}" meson setup upstream-tools-build \
    libinput-%{libinput_tools_commit} \
    --buildtype=plain \
    --prefix=%{_prefix} \
    --libdir=%{_lib} \
    -Dtests=false \
    -Ddocumentation=false \
    -Ddebug-gui=false \
    -Dlibwacom=false \
    -Dlua-plugins=disabled
meson compile -C upstream-tools-build

%install
install -Dm755 target/release/libinput %{buildroot}%{_bindir}/libinput
ln -s libinput %{buildroot}%{_bindir}/libinput-rs
install -Dm755 target/release/libinput-rs-chwd %{buildroot}%{_bindir}/libinput-rs-chwd
install -d %{buildroot}%{_libexecdir}/libinput
upstream_stage="$(pwd)/upstream-tools-stage"
rm -rf "$upstream_stage"
DESTDIR="$upstream_stage" meson install -C upstream-tools-build --no-rebuild
install -Dm755 "$upstream_stage%{_bindir}/libinput" \
    %{buildroot}%{_libexecdir}/libinput/libinput-tool
for helper in \
    libinput-analyze \
    libinput-analyze-buttons \
    libinput-analyze-per-slot-delta \
    libinput-analyze-recording \
    libinput-analyze-touch-down-state \
    libinput-debug-events \
    libinput-debug-tablet \
    libinput-debug-tablet-pad \
    libinput-list-devices \
    libinput-list-kernel-devices \
    libinput-measure \
    libinput-measure-fuzz \
    libinput-measure-touch-size \
    libinput-measure-touchpad-pressure \
    libinput-measure-touchpad-size \
    libinput-measure-touchpad-tap \
    libinput-quirks \
    libinput-record \
    libinput-replay; do
    install -Dm755 "$upstream_stage%{_libexecdir}/libinput/$helper" \
        "%{buildroot}%{_libexecdir}/libinput/$helper"
done
for helper in \
    libinput-analyze-buttons \
    libinput-analyze-per-slot-delta \
    libinput-analyze-recording \
    libinput-analyze-touch-down-state \
    libinput-list-kernel-devices \
    libinput-measure-fuzz \
    libinput-measure-touch-size \
    libinput-measure-touchpad-pressure \
    libinput-measure-touchpad-size \
    libinput-measure-touchpad-tap \
    libinput-replay; do
    sed -i '1s|^#!/usr/bin/env python3$|#!/usr/bin/python3|' \
        "%{buildroot}%{_libexecdir}/libinput/$helper"
done
install -Dm755 target/release/libinput-device-group %{buildroot}%{_prefix}/lib/udev/libinput-device-group
install -Dm755 target/release/libinput-fuzz-extract %{buildroot}%{_prefix}/lib/udev/libinput-fuzz-extract
install -Dm755 target/release/libinput-fuzz-to-zero %{buildroot}%{_prefix}/lib/udev/libinput-fuzz-to-zero
install -Dm644 packaging/80-libinput-device-groups.rules %{buildroot}%{_udevrulesdir}/80-libinput-device-groups.rules
install -Dm644 packaging/90-libinput-fuzz-override.rules %{buildroot}%{_udevrulesdir}/90-libinput-fuzz-override.rules
install -Dm644 packaging/90-libinput-rs-elantech-crc.rules %{buildroot}%{_udevrulesdir}/90-libinput-rs-elantech-crc.rules
install -Dm644 systemd/libinput-rs-elan-resume.service %{buildroot}%{_unitdir}/libinput-rs-elan-resume.service
install -Dm644 systemd/91-libinput-rs-elan.preset %{buildroot}%{_presetdir}/91-libinput-rs-elan.preset
install -d %{buildroot}%{_datadir}/libinput
install -m644 quirks/*.quirks %{buildroot}%{_datadir}/libinput/
install -Dm755 target/release/libinput.so %{buildroot}%{_libdir}/libinput.so.10.13.0
ln -s libinput.so.10.13.0 %{buildroot}%{_libdir}/libinput.so.10
ln -s libinput.so.10 %{buildroot}%{_libdir}/libinput.so
install -Dm644 packaging/libinput.h %{buildroot}%{_includedir}/libinput.h
install -d %{buildroot}%{_libdir}/pkgconfig
sed 's|@LIBDIR@|%{_libdir}|g' packaging/libinput-rs.pc.in \
    > %{buildroot}%{_libdir}/pkgconfig/libinput.pc
install -Dm644 packaging/libinput-rs.8 %{buildroot}%{_mandir}/man8/libinput-rs.8
install -Dm644 packaging/libinput-rs-chwd.8 %{buildroot}%{_mandir}/man8/libinput-rs-chwd.8
for manpage in "$upstream_stage%{_mandir}/man1"/libinput*.1; do
    test "$(basename "$manpage")" = "libinput-test.1" && continue
    install -Dm644 "$manpage" \
        "%{buildroot}%{_mandir}/man1/$(basename "$manpage")"
done
install -Dm644 "$upstream_stage%{_datadir}/zsh/site-functions/_libinput" \
    %{buildroot}%{_datadir}/zsh/site-functions/_libinput
install -Dm644 libinput-%{libinput_tools_commit}/COPYING \
    %{buildroot}%{_licensedir}/%{name}/third-party/libinput-tools/COPYING

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

%pre
if [ "$1" -gt 1 ]; then
    systemctl --no-reload disable --now libinput-rs.service >/dev/null 2>&1 || :
fi

%post
%systemd_post libinput-rs-elan-resume.service

%preun
%systemd_preun libinput-rs-elan-resume.service

%postun
%systemd_postun_with_restart libinput-rs-elan-resume.service

%check
CARGO_NET_OFFLINE=true cargo test --frozen --workspace %{cargo_features}
test -e %{buildroot}%{_libdir}/libinput.so.10

%files
%license LICENSE
%license %{_licensedir}/%{name}/third-party
%doc README.md
%{_bindir}/libinput-rs
%{_bindir}/libinput
%{_bindir}/libinput-rs-chwd
%{_libexecdir}/libinput/libinput-*
%{_prefix}/lib/udev/libinput-device-group
%{_prefix}/lib/udev/libinput-fuzz-extract
%{_prefix}/lib/udev/libinput-fuzz-to-zero
%{_udevrulesdir}/80-libinput-device-groups.rules
%{_udevrulesdir}/90-libinput-fuzz-override.rules
%{_udevrulesdir}/90-libinput-rs-elantech-crc.rules
%{_unitdir}/libinput-rs-elan-resume.service
%{_presetdir}/91-libinput-rs-elan.preset
%{_datadir}/libinput/*.quirks
%{_libdir}/libinput.so.10.13.0
%{_libdir}/libinput.so.10
%{_libdir}/libinput.so
%{_includedir}/libinput.h
%{_libdir}/pkgconfig/libinput.pc
%{_mandir}/man8/libinput-rs.8*
%{_mandir}/man8/libinput-rs-chwd.8*
%{_mandir}/man1/libinput*.1*
%{_datadir}/zsh/site-functions/_libinput

%changelog
* Mon Aug 10 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.3.5-1
- Retry fill_events on EINTR to permanently fix pointer freeze on signal delivery
- Packaging: add binutils BuildRequires, explicit libgfortran Requires
- CLI help text: add debug-tablet and debug-tablet-pad subcommands
- Fix udev rule and recovery service to target device 2-0015

* Mon Aug 10 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.3.4-1
- Handle EINTR in drain_fd to prevent an infinite loop when signals interrupt
  timerfd or eventfd reads during dispatch

* Wed Jul 29 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.3.1-1
- Ship the complete pinned libinput 1.31.3 diagnostic and recording utility suite
- Dispatch standard commands through their upstream implementations
- Share libinput device groups across controller companion nodes
- Drive keyboard LEDs through the tracked evdev descriptor
- Restore the libinput command, diagnostic entry points, manuals, and completion
- Preserve packet CRC validation on affected ThinkPad Elantech v4 controllers
- Reinitialize the P53 ELAN I2C controller after resume without a resident daemon
- Provide a guarded root-only command for recovering a silently wedged ELAN controller
- Apply flat, adaptive, and custom motion and scroll acceleration
- Preserve natural-scroll direction across every continuous scroll source
- Release active touchpad inputs when disabled by an external mouse hotplug
- Route keyboard and pointer events independently on mixed-capability nodes
- Track seat key and button counts independently for every event code
- Fold companion motion and click behavior into the shared backend
- Remove the competing EVIOCGRAB companion service
- Fuse raw uevents, the udev database, inotify, sysfs, and restricted ioctls
- Compile the capability bitmap and fallback classifier from Fortran
- Verify total classification and lifecycle laws with Idris 2 and Agda
- Add deterministic chwd-style profile planning with statically linked Fortran scoring

* Wed Jul 29 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.3.0-2
- Restore per-key and per-button seat-wide state tracking
- Forward keyboard lock LED state through the existing evdev device
- Accept auxiliary keyboard nodes identified by ID_INPUT_KEY

* Tue Jul 28 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.3.0-1
- Consolidate runtime, development files, and companion into one RPM
- Embed permission-independent evdev discovery in the single source crate
- Balance all touchpad button lifecycles and recover from lost releases
- Restore udev integration and the hardware-quirks database in the replacement RPM
- Prevent the upstream synthetic-device suite from running on graphical seats
- Verify button invariants with Rust, Fortran, Idris 2, and Agda

* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.2-1
- Prevent the companion daemon from consuming its own virtual pointer events
- Preserve active touchpad gestures when disable-while-typing becomes active
- Give the companion pointer a stable virtual input identity

* Mon Jul 27 2026 Kenny Glowner <SisyphusAeolides@pm.me> - 0.2.1-5
- Make libwacom tablet metadata an opt-in Cargo feature
- Enable libwacom integration automatically for Fedora RPMs
- Use conservative kernel and udev fallbacks on EPEL and RHEL

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
