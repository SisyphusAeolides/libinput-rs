use input::evdev::Device;
use input::udev_callout::{parse_evdev_abs_fuzz, DeviceHandle, FUZZ_AXES};
use std::env;
use std::path::Path;

fn main() {
    let arguments: Vec<_> = env::args_os().collect();
    if arguments.len() != 2 {
        std::process::exit(1);
    }
    let Some(handle) = DeviceHandle::from_syspath(Path::new(&arguments[1])) else {
        std::process::exit(1);
    };

    if let Some(devnode) = handle.devnode() {
        if let Ok(device) = Device::open(devnode) {
            if let Ok(absinfo) = device.get_absinfo() {
                for (axis, info) in absinfo {
                    if FUZZ_AXES.contains(&axis) && info.fuzz() != 0 {
                        println!("LIBINPUT_FUZZ_{:02x}={}", axis.0, info.fuzz());
                    }
                }
            }
        }
    }

    for axis in FUZZ_AXES {
        let key = format!("EVDEV_ABS_{:02X}", axis.0);
        if let Some(fuzz) = handle
            .property(&key)
            .as_deref()
            .and_then(parse_evdev_abs_fuzz)
        {
            println!("LIBINPUT_FUZZ_{:02x}={fuzz}", axis.0);
        }
    }
}
