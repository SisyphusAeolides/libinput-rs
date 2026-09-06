use evdev::uinput::VirtualDevice as EvdevVirtualDevice;
use evdev::{AttributeSet, InputEvent, KeyCode, RelativeAxisCode};
use log::info;
use std::error::Error;

pub struct VirtualDevice {
    device: EvdevVirtualDevice,
}

impl Drop for VirtualDevice {
    fn drop(&mut self) {
        info!("Virtual input device destroyed");
    }
}

impl VirtualDevice {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let mut keys = AttributeSet::new();
        keys.insert(KeyCode::BTN_LEFT);
        keys.insert(KeyCode::BTN_RIGHT);
        keys.insert(KeyCode::BTN_MIDDLE);

        let mut rel_axes = AttributeSet::new();
        rel_axes.insert(RelativeAxisCode::REL_X);
        rel_axes.insert(RelativeAxisCode::REL_Y);
        rel_axes.insert(RelativeAxisCode::REL_WHEEL);

        let device = EvdevVirtualDevice::builder()?
            .name("libinput-rs Virtual Pointer")
            .with_keys(&keys)?
            .with_relative_axes(&rel_axes)?
            .build()?;

        Ok(Self { device })
    }

    pub fn emit_raw(&mut self, event: InputEvent) -> Result<(), Box<dyn Error>> {
        self.device.emit(&[event])?;
        Ok(())
    }
}
