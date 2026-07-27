#![cfg(unix)]

pub use evdev_upstream::*;

use std::path::{Path, PathBuf};

/// Crawls `/dev/input` for event-device paths without opening the device nodes.
///
/// The caller remains responsible for opening each returned path. This is
/// required by libinput-style restricted-open callbacks, where the compositor
/// may list device nodes but only logind is allowed to open them.
pub fn enumerate() -> EnumerateDevices {
    enumerate_directory(Path::new("/dev/input"))
}

/// An iterator over event-device paths. The second tuple element is retained
/// for source compatibility with the upstream iterator and is intentionally
/// empty because opening belongs to the caller's restricted-open path.
pub struct EnumerateDevices {
    paths: std::vec::IntoIter<PathBuf>,
}

impl Iterator for EnumerateDevices {
    type Item = (PathBuf, ());

    fn next(&mut self) -> Option<Self::Item> {
        self.paths.next().map(|path| (path, ()))
    }
}

fn enumerate_directory(directory: &Path) -> EnumerateDevices {
    let mut paths = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_event_node(path))
        .collect::<Vec<_>>();
    paths.sort();
    EnumerateDevices {
        paths: paths.into_iter(),
    }
}

fn is_event_node(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(index) = name.strip_prefix("event") else {
        return false;
    };
    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock precedes the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "libinput-rs-discovery-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test input directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn enumeration_does_not_open_event_nodes() {
        let directory = TestDirectory::new();
        symlink(directory.0.join("missing-target"), directory.0.join("event0"))
            .expect("create unopenable event node");
        fs::write(directory.0.join("event12"), b"").expect("create event node");
        fs::write(directory.0.join("eventx"), b"").expect("create invalid event name");
        fs::write(directory.0.join("mouse0"), b"").expect("create non-event node");

        let names = enumerate_directory(&directory.0)
            .map(|(path, ())| {
                path.file_name()
                    .expect("event path has a file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["event0".to_string(), "event12".to_string()]);
    }

    #[test]
    fn missing_input_directory_is_empty() {
        let directory = TestDirectory::new();
        let missing = directory.0.join("missing");
        assert_eq!(enumerate_directory(&missing).count(), 0);
    }
}
