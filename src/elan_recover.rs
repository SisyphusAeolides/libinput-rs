//! Narrow recovery support for ELAN I2C controllers that stop reporting.
//!
//! Rebinding the kernel driver reruns its complete probe and initialization
//! path.  This module deliberately accepts only devices already bound to the
//! `elan_i2c` driver; it cannot be used as a generic sysfs driver writer.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const DRIVER_RELATIVE_PATH: &str = "bus/i2c/drivers/elan_i2c";
const DEVICES_RELATIVE_PATH: &str = "bus/i2c/devices";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredDevice {
    pub id: String,
    pub event_nodes: Vec<String>,
}

pub fn discover(sysfs_root: &Path) -> io::Result<Vec<String>> {
    let driver = sysfs_root.join(DRIVER_RELATIVE_PATH);
    let canonical_driver = fs::canonicalize(&driver)?;
    let mut devices = Vec::new();
    for entry in fs::read_dir(&driver)? {
        let entry = entry?;
        let id = entry.file_name();
        let Some(id) = id.to_str() else {
            continue;
        };
        if valid_i2c_id(id) && device_uses_driver(sysfs_root, id, &canonical_driver) {
            devices.push(id.to_string());
        }
    }
    devices.sort();
    Ok(devices)
}

pub fn recover(sysfs_root: &Path, id: &str) -> io::Result<RecoveredDevice> {
    if !valid_i2c_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid I2C device identifier '{id}'"),
        ));
    }

    let driver = sysfs_root.join(DRIVER_RELATIVE_PATH);
    let canonical_driver = fs::canonicalize(&driver)?;
    if !device_uses_driver(sysfs_root, id, &canonical_driver) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{id} is not bound to elan_i2c"),
        ));
    }

    write_control(&driver.join("unbind"), id)?;
    wait_until(Duration::from_secs(1), || {
        !sysfs_root
            .join(DEVICES_RELATIVE_PATH)
            .join(id)
            .join("driver")
            .exists()
    })?;
    thread::sleep(Duration::from_millis(100));

    if let Err(error) = bind_with_retry(&driver.join("bind"), id) {
        return Err(io::Error::new(
            error.kind(),
            format!("ELAN {id} was unbound but could not be rebound: {error}"),
        ));
    }

    wait_until(Duration::from_secs(3), || {
        device_uses_driver(sysfs_root, id, &canonical_driver)
    })?;
    let event_nodes = wait_for_event_nodes(sysfs_root, id, Duration::from_secs(3))?;
    Ok(RecoveredDevice {
        id: id.to_string(),
        event_nodes,
    })
}

fn bind_with_retry(path: &Path, id: &str) -> io::Result<()> {
    let delays = [
        Duration::from_millis(0),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
    ];
    let mut last_error = None;
    for delay in delays {
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        match write_control(path, id) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("ELAN bind failed")))
}

pub fn affected_thinkpad_p53(sysfs_root: &Path) -> bool {
    let vendor = read_trimmed(sysfs_root.join("class/dmi/id/sys_vendor"));
    let version = read_trimmed(sysfs_root.join("class/dmi/id/product_version"));
    vendor.as_deref() == Some("LENOVO")
        && version
            .as_deref()
            .is_some_and(|value| value.starts_with("ThinkPad P53"))
}

fn valid_i2c_id(id: &str) -> bool {
    let Some((bus, address)) = id.split_once('-') else {
        return false;
    };
    !bus.is_empty()
        && bus.bytes().all(|byte| byte.is_ascii_digit())
        && address.len() == 4
        && address.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn device_uses_driver(sysfs_root: &Path, id: &str, canonical_driver: &Path) -> bool {
    fs::canonicalize(
        sysfs_root
            .join(DEVICES_RELATIVE_PATH)
            .join(id)
            .join("driver"),
    )
    .is_ok_and(|path| path == canonical_driver)
}

fn write_control(path: &Path, id: &str) -> io::Result<()> {
    let mut control = OpenOptions::new().write(true).open(path)?;
    let payload = format!("{id}\n");
    let written = control.write(payload.as_bytes())?;
    if written == payload.len() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short write to ELAN driver control",
        ))
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    if predicate() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for the ELAN kernel driver",
        ))
    }
}

fn wait_for_event_nodes(sysfs_root: &Path, id: &str, timeout: Duration) -> io::Result<Vec<String>> {
    let mut nodes = Vec::new();
    wait_until(timeout, || {
        nodes = event_nodes(sysfs_root, id);
        !nodes.is_empty()
    })?;
    Ok(nodes)
}

fn event_nodes(sysfs_root: &Path, id: &str) -> Vec<String> {
    let input_root = sysfs_root
        .join(DEVICES_RELATIVE_PATH)
        .join(id)
        .join("input");
    let Ok(inputs) = fs::read_dir(input_root) else {
        return Vec::new();
    };
    let mut nodes = Vec::new();
    for input in inputs.flatten() {
        let Ok(entries) = fs::read_dir(input.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.strip_prefix("event").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            }) {
                nodes.push(name.to_string());
            }
        }
    }
    nodes.sort();
    nodes.dedup();
    nodes
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{affected_thinkpad_p53, valid_i2c_id};

    #[test]
    fn accepts_only_canonical_i2c_identifiers() {
        assert!(valid_i2c_id("5-0015"));
        assert!(valid_i2c_id("12-00AF"));
        assert!(!valid_i2c_id("../elan_i2c"));
        assert!(!valid_i2c_id("5-15"));
        assert!(!valid_i2c_id("i2c-5-0015"));
    }

    #[test]
    fn affected_machine_gate_fails_closed_without_dmi() {
        let missing =
            std::env::temp_dir().join(format!("libinput-rs-no-dmi-{}", std::process::id()));
        assert!(!affected_thinkpad_p53(&missing));
    }
}
