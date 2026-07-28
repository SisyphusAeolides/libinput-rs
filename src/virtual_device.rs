use evdev::uinput::VirtualDevice as EvdevVirtualDevice;
use evdev::{AttributeSet, BusType, InputEvent, InputId, KeyCode, RelativeAxisCode};
use log::info;
use std::error::Error;

pub struct VirtualDevice {
    device: EvdevVirtualDevice,
}

pub const DEVICE_NAME: &str = "Rust Input Companion Pointer";
pub const DEVICE_VENDOR: u16 = 0x1d6b;
pub const DEVICE_PRODUCT: u16 = 0x1eaf;

pub fn is_companion_device(name: Option<&str>, id: &InputId) -> bool {
    name == Some(DEVICE_NAME)
        || (id.bus_type() == BusType::BUS_VIRTUAL
            && id.vendor() == DEVICE_VENDOR
            && id.product() == DEVICE_PRODUCT)
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
            .name(DEVICE_NAME)
            .input_id(InputId::new(
                BusType::BUS_VIRTUAL,
                DEVICE_VENDOR,
                DEVICE_PRODUCT,
                1,
            ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_is_recognized_by_name_during_upgrade() {
        let legacy_id = InputId::new(BusType::BUS_USB, 0x1234, 0x5678, 0x111);
        assert!(is_companion_device(Some(DEVICE_NAME), &legacy_id));
    }

    #[test]
    fn companion_is_recognized_by_stable_virtual_id() {
        let id = InputId::new(BusType::BUS_VIRTUAL, DEVICE_VENDOR, DEVICE_PRODUCT, 1);
        assert!(is_companion_device(Some("renamed companion"), &id));
    }

    #[test]
    fn physical_pointer_is_not_mistaken_for_companion() {
        let id = InputId::new(BusType::BUS_I2C, 0x04f3, 0x0029, 0);
        assert!(!is_companion_device(Some("Elan TrackPoint"), &id));
    }
}
