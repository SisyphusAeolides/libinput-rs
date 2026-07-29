mod config;
mod device;
mod evdev;
mod event_loop;
mod virtual_device;

use log::{info, warn};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();
    info!("Starting libinput-rs daemon...");

    let config = config::load_config().unwrap_or_default();

    // The output device must exist before any physical touchpad is grabbed.
    // If uinput is unavailable, failing here leaves the system input stack
    // completely untouched.
    let mut v_device = virtual_device::VirtualDevice::new()?;

    let devices = device::scan_input_devices()?;
    if devices.is_empty() {
        warn!("No suitable input devices found currently, waiting for hotplug events...");
    }

    info!("Entering mio event loop...");
    event_loop::run(devices, &mut v_device, &config)?;

    Ok(())
}
