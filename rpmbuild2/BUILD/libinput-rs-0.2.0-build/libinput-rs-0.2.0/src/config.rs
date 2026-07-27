use serde::Deserialize;
use std::fs;

#[derive(Deserialize, Debug)]
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
            pointer_acceleration: 1.0,
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
