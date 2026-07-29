use crate::evdev::{AbsoluteAxisCode, Device};
use std::ffi::{CStr, CString};
use std::os::fd::AsRawFd;
use std::path::Path;

#[link(name = "udev")]
extern "C" {
    fn udev_new() -> *mut libc::c_void;
    fn udev_unref(udev: *mut libc::c_void) -> *mut libc::c_void;
    fn udev_device_new_from_syspath(
        udev: *mut libc::c_void,
        syspath: *const libc::c_char,
    ) -> *mut libc::c_void;
    fn udev_device_unref(device: *mut libc::c_void) -> *mut libc::c_void;
    fn udev_device_get_devnode(device: *mut libc::c_void) -> *const libc::c_char;
    fn udev_device_get_parent(device: *mut libc::c_void) -> *mut libc::c_void;
    fn udev_device_get_property_value(
        device: *mut libc::c_void,
        key: *const libc::c_char,
    ) -> *const libc::c_char;
    fn udev_device_get_sysattr_value(
        device: *mut libc::c_void,
        key: *const libc::c_char,
    ) -> *const libc::c_char;
}

pub const FUZZ_AXES: [AbsoluteAxisCode; 4] = [
    AbsoluteAxisCode::ABS_X,
    AbsoluteAxisCode::ABS_Y,
    AbsoluteAxisCode::ABS_MT_POSITION_X,
    AbsoluteAxisCode::ABS_MT_POSITION_Y,
];

pub struct DeviceHandle {
    udev: *mut libc::c_void,
    device: *mut libc::c_void,
}

impl DeviceHandle {
    pub fn from_syspath(path: &Path) -> Option<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        unsafe {
            let udev = udev_new();
            if udev.is_null() {
                return None;
            }
            let device = udev_device_new_from_syspath(udev, path.as_ptr());
            if device.is_null() {
                udev_unref(udev);
                return None;
            }
            Some(Self { udev, device })
        }
    }

    pub fn devnode(&self) -> Option<String> {
        unsafe { string_from_ptr(udev_device_get_devnode(self.device)) }
    }

    pub fn property(&self, key: &str) -> Option<String> {
        let key = CString::new(key).ok()?;
        unsafe { string_from_ptr(udev_device_get_property_value(self.device, key.as_ptr())) }
    }

    pub fn first_parent_value(&self, key: &str) -> Option<(String, Option<String>)> {
        let key = CString::new(key).ok()?;
        let product = CString::new("PRODUCT").ok()?;
        unsafe {
            let mut device = self.device;
            while !device.is_null() {
                if let Some(value) =
                    string_from_ptr(udev_device_get_sysattr_value(device, key.as_ptr()))
                {
                    let product =
                        string_from_ptr(udev_device_get_property_value(device, product.as_ptr()));
                    return Some((value, product));
                }
                device = udev_device_get_parent(device);
            }
        }
        None
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        unsafe {
            udev_device_unref(self.device);
            udev_unref(self.udev);
        }
    }
}

unsafe fn string_from_ptr(value: *const libc::c_char) -> Option<String> {
    (!value.is_null()).then(|| CStr::from_ptr(value).to_string_lossy().into_owned())
}

pub fn set_abs_fuzz(device: &Device, axis: AbsoluteAxisCode, fuzz: i32) -> bool {
    let Ok(absinfo) = device.get_absinfo() else {
        return false;
    };
    let Some((_, info)) = absinfo
        .into_iter()
        .find(|(candidate, _)| *candidate == axis)
    else {
        return true;
    };
    if info.fuzz() == fuzz {
        return true;
    }

    let replacement = crate::evdev::AbsInfo::new(
        info.value(),
        info.minimum(),
        info.maximum(),
        fuzz,
        info.flat(),
        info.resolution(),
    );
    let request = (1_u64 << 30)
        | ((std::mem::size_of::<crate::evdev::AbsInfo>() as u64) << 16)
        | (u64::from(b'E') << 8)
        | u64::from(0xc0_u16 + axis.0);
    unsafe { libc::ioctl(device.as_raw_fd(), request as libc::c_ulong, &replacement) >= 0 }
}

pub fn parse_evdev_abs_fuzz(value: &str) -> Option<i32> {
    let field = value.split(':').nth(3)?;
    if field.is_empty() {
        return None;
    }
    field.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_evdev_abs_fuzz;

    #[test]
    fn extracts_the_fourth_evdev_abs_field() {
        assert_eq!(parse_evdev_abs_fuzz("0:100:20:7"), Some(7));
        assert_eq!(parse_evdev_abs_fuzz("::20:12"), Some(12));
        assert_eq!(parse_evdev_abs_fuzz("0:100:20:"), None);
    }
}
