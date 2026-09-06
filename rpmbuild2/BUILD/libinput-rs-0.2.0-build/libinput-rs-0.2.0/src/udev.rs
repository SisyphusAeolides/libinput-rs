use std::ffi::{CStr, CString};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[link(name = "udev")]
unsafe extern "C" {
    fn udev_new() -> *mut libc::c_void;
    fn udev_unref(udev: *mut libc::c_void) -> *mut libc::c_void;
    fn udev_device_new_from_devnum(
        udev: *mut libc::c_void,
        device_type: libc::c_char,
        device_number: libc::dev_t,
    ) -> *mut libc::c_void;
    pub fn udev_device_ref(device: *mut libc::c_void) -> *mut libc::c_void;
    pub fn udev_device_unref(device: *mut libc::c_void) -> *mut libc::c_void;
    fn udev_device_get_property_value(
        device: *mut libc::c_void,
        key: *const libc::c_char,
    ) -> *const libc::c_char;
}

pub unsafe fn device_from_path(path: &Path) -> *mut libc::c_void {
    let Ok(metadata) = path.metadata() else {
        return std::ptr::null_mut();
    };
    let udev = udev_new();
    if udev.is_null() {
        return std::ptr::null_mut();
    }
    let device = udev_device_new_from_devnum(udev, b'c' as libc::c_char, metadata.rdev());
    udev_unref(udev);
    device
}

pub struct UdevDevice {
    pointer: *mut libc::c_void,
}

impl UdevDevice {
    pub unsafe fn from_path(path: &Path) -> Self {
        Self {
            pointer: device_from_path(path),
        }
    }

    pub fn as_ptr(&self) -> *mut libc::c_void {
        self.pointer
    }

    pub fn into_raw(mut self) -> *mut libc::c_void {
        let pointer = self.pointer;
        self.pointer = std::ptr::null_mut();
        pointer
    }
}

impl Drop for UdevDevice {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            unsafe {
                udev_device_unref(self.pointer);
            }
        }
    }
}

pub unsafe fn property_equals(device: *mut libc::c_void, key: &str, expected: &str) -> bool {
    if device.is_null() {
        return false;
    }
    let Ok(key) = CString::new(key) else {
        return false;
    };
    let value = udev_device_get_property_value(device, key.as_ptr());
    !value.is_null() && CStr::from_ptr(value).to_bytes() == expected.as_bytes()
}

pub unsafe fn property_value(device: *mut libc::c_void, key: &str) -> Option<String> {
    if device.is_null() {
        return None;
    }
    let key = CString::new(key).ok()?;
    let value = udev_device_get_property_value(device, key.as_ptr());
    (!value.is_null()).then(|| CStr::from_ptr(value).to_string_lossy().into_owned())
}
