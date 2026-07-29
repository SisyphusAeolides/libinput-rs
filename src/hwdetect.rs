//! Universal input-hardware fusion for libinput-rs.
//!
//! Coldplug enumerates names without opening event nodes. Hotplug combines a
//! raw post-udev netlink stream with inotify, and the udev database supplies
//! seat and classification properties. The backend remains responsible for
//! restricted-open and ioctl probing before it emits DEVICE_ADDED.

use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
use std::collections::HashMap;
use std::fs;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const UDEV_MONITOR_GROUP: u32 = 2;
const KERNEL_MONITOR_GROUP: u32 = 1;
const UEVENT_BUFFER_SIZE: usize = 8192;
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

pub struct HardwareDiscovery {
    netlink: Option<OwnedFd>,
    inotify: Option<Inotify>,
    last_reconcile: Instant,
}

impl HardwareDiscovery {
    pub fn new() -> Self {
        let inotify = Inotify::init(InitFlags::IN_CLOEXEC | InitFlags::IN_NONBLOCK)
            .ok()
            .and_then(|inotify| {
                inotify
                    .add_watch(
                        "/dev/input",
                        AddWatchFlags::IN_CREATE
                            | AddWatchFlags::IN_ATTRIB
                            | AddWatchFlags::IN_DELETE
                            | AddWatchFlags::IN_MOVED_FROM
                            | AddWatchFlags::IN_MOVED_TO,
                    )
                    .ok()?;
                Some(inotify)
            });
        Self {
            netlink: open_uevent_socket(),
            inotify,
            last_reconcile: Instant::now(),
        }
    }

    pub fn fds(&self) -> Vec<RawFd> {
        let mut fds = Vec::with_capacity(2);
        if let Some(netlink) = &self.netlink {
            fds.push(netlink.as_raw_fd());
        }
        if let Some(inotify) = &self.inotify {
            fds.push(inotify.as_fd().as_raw_fd());
        }
        fds
    }

    /// Drain both notification sources. Either one triggers a full
    /// reconciliation because devnode and udev-database readiness may arrive
    /// in either order.
    pub fn drain_changed(&mut self) -> bool {
        let changed = self.drain_netlink() | self.drain_inotify();
        let due = self.last_reconcile.elapsed() >= RECONCILE_INTERVAL;
        if changed || due {
            self.last_reconcile = Instant::now();
        }
        changed || due
    }

    pub fn mark_reconciled(&mut self) {
        self.last_reconcile = Instant::now();
    }

    pub fn next_reconcile(&self) -> Duration {
        RECONCILE_INTERVAL.saturating_sub(self.last_reconcile.elapsed())
    }

    fn drain_netlink(&self) -> bool {
        let Some(netlink) = &self.netlink else {
            return false;
        };
        let mut changed = false;
        let mut buffer = [0_u8; UEVENT_BUFFER_SIZE];
        loop {
            let length = unsafe {
                libc::recv(
                    netlink.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };
            if length <= 0 {
                break;
            }
            changed |= is_input_uevent(&buffer[..length as usize]);
        }
        changed
    }

    fn drain_inotify(&self) -> bool {
        self.inotify
            .as_ref()
            .and_then(|inotify| inotify.read_events().ok())
            .is_some_and(|events| !events.is_empty())
    }
}

impl Default for HardwareDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

fn open_uevent_socket() -> Option<OwnedFd> {
    [
        UDEV_MONITOR_GROUP,
        UDEV_MONITOR_GROUP | KERNEL_MONITOR_GROUP,
        u32::MAX,
    ]
    .into_iter()
    .find_map(bind_uevent_socket)
}

fn bind_uevent_socket(groups: u32) -> Option<OwnedFd> {
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        );
        if fd < 0 {
            return None;
        }
        let mut address: libc::sockaddr_nl = std::mem::zeroed();
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_pid = 0;
        address.nl_groups = groups;
        if libc::bind(
            fd,
            (&address as *const libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        ) < 0
        {
            libc::close(fd);
            return None;
        }
        Some(OwnedFd::from_raw_fd(fd))
    }
}

fn is_input_uevent(message: &[u8]) -> bool {
    message.split(|byte| *byte == 0).any(|field| {
        field == b"SUBSYSTEM=input"
            || field
                .strip_prefix(b"DEVNAME=")
                .is_some_and(|name| name.starts_with(b"input/event"))
    })
}

/// Read the post-udev property database without opening the event node.
pub fn udev_database_properties(devnode: &Path) -> HashMap<String, String> {
    let mut properties = HashMap::new();
    let Ok(metadata) = fs::metadata(devnode) else {
        return properties;
    };
    let database_name = format!(
        "c{}:{}",
        nix::sys::stat::major(metadata.rdev()),
        nix::sys::stat::minor(metadata.rdev())
    );
    for root in ["/run/udev/data", "/var/run/udev/data", "/dev/.udev/db"] {
        let Ok(contents) = fs::read_to_string(PathBuf::from(root).join(&database_name)) else {
            continue;
        };
        parse_udev_database(&contents, &mut properties);
        break;
    }
    properties
}

pub fn udev_database_is_present() -> bool {
    ["/run/udev/data", "/var/run/udev/data", "/dev/.udev/db"]
        .into_iter()
        .any(|root| Path::new(root).is_dir())
}

fn parse_udev_database(contents: &str, properties: &mut HashMap<String, String>) {
    for line in contents.lines() {
        let Some(property) = line.strip_prefix("E:") else {
            continue;
        };
        let Some((name, value)) = property.split_once('=') else {
            continue;
        };
        properties.insert(name.to_string(), value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uevent_parser_accepts_only_input_messages() {
        assert!(is_input_uevent(
            b"libudev\0ACTION=add\0SUBSYSTEM=input\0DEVNAME=input/event8\0"
        ));
        assert!(!is_input_uevent(
            b"libudev\0ACTION=add\0SUBSYSTEM=block\0DEVNAME=sda\0"
        ));
    }

    #[test]
    fn udev_database_parser_keeps_environment_properties() {
        let mut properties = HashMap::new();
        parse_udev_database(
            "I:123\nE:ID_INPUT=1\nE:ID_SEAT=seat-test\nG:seat\n",
            &mut properties,
        );
        assert_eq!(properties.get("ID_INPUT").map(String::as_str), Some("1"));
        assert_eq!(
            properties.get("ID_SEAT").map(String::as_str),
            Some("seat-test")
        );
        assert_eq!(properties.len(), 2);
    }

    #[test]
    fn discovery_sources_are_optional_and_have_unique_fds() {
        let discovery = HardwareDiscovery::new();
        let fds = discovery.fds();
        assert!(fds.iter().all(|fd| *fd >= 0));
        assert!(fds.len() <= 2);
        if fds.len() == 2 {
            assert_ne!(fds[0], fds[1]);
        }
    }
}
