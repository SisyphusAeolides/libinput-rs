use serde::{de, Deserialize, Deserializer};
use std::fs;

pub const DEFAULT_POINTER_ACCELERATION: f32 = 2.2;
const NORMALIZATION_REFERENCE_ACCELERATION: f32 = 2.5;
const DEFAULT_POINTER_MULTIPLIER: f32 =
    DEFAULT_POINTER_ACCELERATION / NORMALIZATION_REFERENCE_ACCELERATION;

fn deserialize_pointer_acceleration<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let configured = f32::deserialize(deserializer)?;
    if !configured.is_finite() || configured <= 0.0 {
        return Err(de::Error::custom(
            "pointer_acceleration must be a finite positive number",
        ));
    }

    Ok(configured / NORMALIZATION_REFERENCE_ACCELERATION)
}

#[derive(Deserialize, Debug, PartialEq)]
pub struct InputConfig {
    pub tap_to_click: bool,
    pub natural_scrolling: bool,
    #[serde(deserialize_with = "deserialize_pointer_acceleration")]
    pub pointer_acceleration: f32,
    pub disable_while_typing: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            tap_to_click: true,
            natural_scrolling: true,
            pointer_acceleration: DEFAULT_POINTER_MULTIPLIER,
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

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.000_001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn shipped_config_matches_runtime_defaults() {
        let shipped: InputConfig = serde_json::from_str(include_str!("config.json"))
            .expect("the shipped companion configuration must remain valid JSON");

        assert_eq!(shipped, InputConfig::default());
        assert_close(shipped.pointer_acceleration, 0.88);
    }

    #[test]
    fn configured_speed_preserves_the_legacy_scale() {
        let calibrated: InputConfig = serde_json::from_str(
            r#"{
                "tap_to_click": true,
                "natural_scrolling": true,
                "pointer_acceleration": 2.2,
                "disable_while_typing": true
            }"#,
        )
        .expect("calibrated configuration must parse");
        let reference: InputConfig = serde_json::from_str(
            r#"{
                "tap_to_click": true,
                "natural_scrolling": true,
                "pointer_acceleration": 2.5,
                "disable_while_typing": true
            }"#,
        )
        .expect("reference configuration must parse");

        assert_close(calibrated.pointer_acceleration, 0.88);
        assert_close(reference.pointer_acceleration, 1.0);
        assert_close(0.45 * calibrated.pointer_acceleration, 0.18 * 2.2);
    }

    #[test]
    fn non_positive_speed_is_rejected() {
        let invalid = r#"{
            "tap_to_click": true,
            "natural_scrolling": true,
            "pointer_acceleration": 0.0,
            "disable_while_typing": true
        }"#;

        assert!(serde_json::from_str::<InputConfig>(invalid).is_err());
    }
}
