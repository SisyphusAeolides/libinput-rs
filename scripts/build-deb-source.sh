#!/bin/sh
set -eu

case "${1:-}" in
    "") sign_option="" ;;
    --unsigned) sign_option="--no-sign" ;;
    *)
        echo "usage: $0 [--unsigned]" >&2
        exit 2
        ;;
esac

upstream_commit=26191d396d74d505541d6311f0b4ae68d791b890
upstream_sha256=d5d8c8464f9cb24b0897c03edfe7d7c9e75ff5a91fe9b5b48791781aa9642858
upstream_url="https://gitlab.freedesktop.org/libinput/libinput/-/archive/$upstream_commit/libinput-$upstream_commit.tar.gz"

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output_dir=$(dirname -- "$project_root")
package_name=$(dpkg-parsechangelog -l"$project_root/debian/changelog" -S Source)
full_version=$(dpkg-parsechangelog -l"$project_root/debian/changelog" -S Version)
file_version=${full_version#*:}
upstream_version=${file_version%%-*}
source_name="$package_name-$upstream_version"
build_root=$(mktemp -d)

cleanup() {
    rm -rf -- "$build_root"
}
trap cleanup EXIT HUP INT TERM

source_dir="$build_root/$source_name"
file_manifest="$build_root/files"
mkdir -p -- "$source_dir"

cd "$project_root"
git ls-files -z > "$file_manifest"
git ls-files --others --exclude-standard -z >> "$file_manifest"
tar --null --exclude='rpmbuild' --exclude='rpmbuild/*' \
    --exclude='rpmbuild2' --exclude='rpmbuild2/*' \
    --files-from="$file_manifest" \
    --create --file="$build_root/source.tar"
tar --extract --file="$build_root/source.tar" --directory="$source_dir"

vendor_dir="$build_root/vendor"
cd "$source_dir"
cargo vendor --offline --locked "$vendor_dir" >/dev/null
rm -rf -- "$source_dir/vendor"
mv -- "$vendor_dir" "$source_dir/vendor"

# Re-apply the EINTR retry patch to the vendored evdev raw_stream.rs and
# update its .cargo-checksum.json so --locked builds accept the modified file.
_pf=$(mktemp /tmp/eintr-patch.XXXXXX)
cat > "$_pf" <<'EINTR_PATCH'
--- vendor/evdev/src/raw_stream.rs.orig	2026-01-01 00:00:00
+++ vendor/evdev/src/raw_stream.rs	2026-01-01 00:00:00
@@ -432,8 +432,17 @@
         let spare_capacity = self.event_buf.spare_capacity_mut();
         let spare_capacity_size = std::mem::size_of_val(spare_capacity);
 
-        // use libc::read instead of nix::unistd::read b/c we need to pass an uninitialized buf
-        let res = unsafe { libc::read(fd, spare_capacity.as_mut_ptr() as _, spare_capacity_size) };
-        let bytes_read = nix::errno::Errno::result(res)?;
+        // use libc::read instead of nix::unistd::read b/c we need to pass an uninitialized buf
+        // Retry on EINTR: a signal (e.g. SIGWINCH) must not silently break the event drain
+        // loop and leave a touch frame open, which would stall pointer motion.
+        let bytes_read = loop {
+            let res = unsafe {
+                libc::read(fd, spare_capacity.as_mut_ptr() as _, spare_capacity_size)
+            };
+            match nix::errno::Errno::result(res) {
+                Err(nix::errno::Errno::EINTR) => continue,
+                other => break other?,
+            }
+        };
         let num_read = bytes_read as usize / mem::size_of::<input_event>();
EINTR_PATCH
patch -p0 < "$_pf"
rm -f "$_pf"
python3 -c "
import hashlib, json, pathlib
p = pathlib.Path('vendor/evdev/src/raw_stream.rs')
new_hash = hashlib.sha256(p.read_bytes()).hexdigest()
cf = pathlib.Path('vendor/evdev/.cargo-checksum.json')
data = json.loads(cf.read_text())
data['files']['src/raw_stream.rs'] = new_hash
cf.write_text(json.dumps(data, separators=(',', ':'), sort_keys=True))
print('Updated vendor checksum:', new_hash)
"

upstream_archive="$build_root/libinput-$upstream_commit.tar.gz"
curl --fail --location --retry 3 --retry-all-errors \
    --output "$upstream_archive" "$upstream_url"
echo "$upstream_sha256  $upstream_archive" | sha256sum -c -
mkdir -p -- "$source_dir/upstream/libinput-tools"
tar --extract --gzip --file="$upstream_archive" --strip-components=1 \
    --directory="$source_dir/upstream/libinput-tools"

cd "$project_root"
source_date_epoch=$(git log -1 --format=%ct)
orig_archive="$build_root/${package_name}_${upstream_version}.orig.tar.xz"
tar --sort=name --mtime="@$source_date_epoch" --owner=0 --group=0 \
    --numeric-owner --exclude="$source_name/debian" \
    --create --xz --file="$orig_archive" \
    --directory="$build_root" "$source_name"

cd "$source_dir"
if [ -n "$sign_option" ]; then
    dpkg-buildpackage --build=source -sa -d "$sign_option"
else
    dpkg-buildpackage --build=source -sa -d
fi

install -m 0644 "$orig_archive" "$output_dir/"
install -m 0644 "$build_root/${package_name}_${file_version}.debian.tar.xz" "$output_dir/"
install -m 0644 "$build_root/${package_name}_${file_version}.dsc" "$output_dir/"
install -m 0644 "$build_root/${package_name}_${file_version}_source.buildinfo" "$output_dir/"
install -m 0644 "$build_root/${package_name}_${file_version}_source.changes" "$output_dir/"

echo "Source package written to $output_dir"
