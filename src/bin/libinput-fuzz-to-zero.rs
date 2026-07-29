use input::evdev::Device;
use input::udev_callout::{set_abs_fuzz, DeviceHandle, FUZZ_AXES};
use std::env;
use std::fs::OpenOptions;
use std::path::Path;

fn main() {
    let arguments: Vec<_> = env::args_os().collect();
    if arguments.len() != 2 {
        std::process::exit(1);
    }
    let Some(handle) = DeviceHandle::from_syspath(Path::new(&arguments[1])) else {
        std::process::exit(1);
    };
    let Some(devnode) = handle.devnode() else {
        return;
    };
    let Ok(file) = OpenOptions::new().read(true).write(true).open(devnode) else {
        return;
    };
    let Ok(device) = Device::from_fd(file.into()) else {
        return;
    };
    for axis in FUZZ_AXES {
        set_abs_fuzz(&device, axis, 0);
    }
}
