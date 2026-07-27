use serde::Deserialize;
use std::fs;

pub const DEFAULT_POINTER_ACCELERATION: f32 = 1.0;

#[derive(Deserialize, Debug, PartialEq)]
pub struct InputConfig {
    pub tap_to_click: bool,
    pub natural_scrolling: bool,
    pub pointer_acceleration: f32,
    pub disable_while_typing: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            tap_to_click: true,
            natural_scrolling: true,
            pointer_acceleration: DEFAULT_POINTER_ACCELERATION,
            disable_while_typing: true,
        }
    }
}

#[allow(dead_code)]
pub fn load_config() -> Option<InputConfig> {
    let path = "/etc/libinput-rs/config.json";
    if let Ok(data) = fs::read_to_string(path) {
        serde_json::from_str(&data).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_config_matches_runtime_defaults() {
        let shipped: InputConfig = serde_json::from_str(include_str!("config.json"))
            .expect("the shipped companion configuration must remain valid JSON");

        assert_eq!(shipped, InputConfig::default());
        assert_eq!(shipped.pointer_acceleration, DEFAULT_POINTER_ACCELERATION);
    }
}
