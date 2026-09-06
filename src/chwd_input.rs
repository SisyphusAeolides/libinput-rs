//! chwd-style deterministic profile selection over fused DMI, udev, and evdev facts.
//!
//! Statistical scorers may rank profiles only after their hard predicates
//! match. They never create capabilities, override a udev role, or turn an
//! unmatched device into an input device.

use crate::capforge::{knn_scores, tiny_mlp_scores, CapabilityKind};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const FEATURE_COUNT: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemFacts {
    pub dmi_vendor: String,
    pub dmi_product: String,
    pub dmi_product_version: String,
    pub dmi_board: String,
    pub cpu_vendor: String,
    pub thinkpad: bool,
    pub apple: bool,
    pub handheld: bool,
}

#[derive(Clone, Debug)]
pub struct DeviceFacts<'a> {
    pub name: &'a str,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub kind: CapabilityKind,
    pub properties: &'a HashMap<String, String>,
    pub has_relative_motion: bool,
    pub has_absolute_xy: bool,
    pub has_multitouch: bool,
    pub key_count: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProfileApply {
    pub lenovo_x230_motion: bool,
    pub trackpoint_multiplier: Option<f64>,
    pub phantom_click_filter: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchRule {
    ThinkPad,
    Apple,
    Handheld,
    DmiProduct(&'static str),
    Name(&'static str),
    Kind(CapabilityKind),
    Udev(&'static str, &'static str),
}

#[derive(Clone, Copy, Debug)]
struct Profile {
    id: &'static str,
    priority: i32,
    rules: &'static [MatchRule],
    centroid: [f64; FEATURE_COUNT],
    apply: ProfileApply,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileDecision {
    pub id: &'static str,
    pub priority: i32,
    pub score: f64,
    pub apply: ProfileApply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectSource {
    Profile,
    MlFallback,
    Heuristic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectionResult {
    pub class: CapabilityKind,
    pub profile_id: &'static str,
    pub priority: i32,
    pub confidence: f64,
    pub source: DetectSource,
    pub apply: ProfileApply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub id: &'static str,
    pub priority: i32,
}

#[derive(Clone, Debug)]
pub struct ScannedDevice {
    pub path: std::path::PathBuf,
    pub name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub kind: CapabilityKind,
    pub properties: HashMap<String, String>,
    pub has_relative_motion: bool,
    pub has_absolute_xy: bool,
    pub has_multitouch: bool,
    pub key_count: usize,
}

impl ScannedDevice {
    pub fn facts(&self) -> DeviceFacts<'_> {
        DeviceFacts {
            name: &self.name,
            bus: self.bus,
            vendor: self.vendor,
            product: self.product,
            kind: self.kind,
            properties: &self.properties,
            has_relative_motion: self.has_relative_motion,
            has_absolute_xy: self.has_absolute_xy,
            has_multitouch: self.has_multitouch,
            key_count: self.key_count,
        }
    }
}

const GENERIC_TOUCHPAD_FEATURES: [f64; FEATURE_COUNT] = [
    0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
];
const GENERIC_MOUSE_FEATURES: [f64; FEATURE_COUNT] = [
    0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
];

const PROFILES: &[Profile] = &[
    Profile {
        id: "lenovo-x230-touchpad",
        priority: 100,
        rules: &[
            MatchRule::ThinkPad,
            MatchRule::DmiProduct("x230"),
            MatchRule::Kind(CapabilityKind::Touchpad),
        ],
        centroid: GENERIC_TOUCHPAD_FEATURES,
        apply: ProfileApply {
            lenovo_x230_motion: true,
            trackpoint_multiplier: None,
            phantom_click_filter: false,
        },
    },
    Profile {
        id: "thinkpad-p53-elan",
        priority: 90,
        rules: &[
            MatchRule::ThinkPad,
            MatchRule::DmiProduct("p53"),
            MatchRule::Name("elan"),
            MatchRule::Kind(CapabilityKind::Touchpad),
            // This filter changes click semantics and must be enabled only
            // from an affected packet recording or an equivalent explicit
            // device signature, never from DMI and a broad ELAN name alone.
            MatchRule::Udev("LIBINPUT_RS_P53_PHANTOM_CLICK_SIGNATURE", "1"),
        ],
        centroid: GENERIC_TOUCHPAD_FEATURES,
        apply: ProfileApply {
            lenovo_x230_motion: false,
            trackpoint_multiplier: None,
            phantom_click_filter: true,
        },
    },
    Profile {
        id: "thinkpad-trackpoint",
        priority: 80,
        rules: &[MatchRule::ThinkPad, MatchRule::Name("trackpoint")],
        centroid: GENERIC_MOUSE_FEATURES,
        apply: ProfileApply {
            lenovo_x230_motion: false,
            trackpoint_multiplier: Some(1.0),
            phantom_click_filter: false,
        },
    },
    Profile {
        id: "apple-touchpad",
        priority: 70,
        rules: &[MatchRule::Apple, MatchRule::Kind(CapabilityKind::Touchpad)],
        centroid: GENERIC_TOUCHPAD_FEATURES,
        apply: ProfileApply {
            lenovo_x230_motion: false,
            trackpoint_multiplier: None,
            phantom_click_filter: false,
        },
    },
    Profile {
        id: "handheld-touchpad",
        priority: 60,
        rules: &[
            MatchRule::Handheld,
            MatchRule::Kind(CapabilityKind::Touchpad),
        ],
        centroid: GENERIC_TOUCHPAD_FEATURES,
        apply: ProfileApply {
            lenovo_x230_motion: false,
            trackpoint_multiplier: None,
            phantom_click_filter: false,
        },
    },
    Profile {
        id: "udev-touchpad",
        priority: 10,
        rules: &[MatchRule::Udev("ID_INPUT_TOUCHPAD", "1")],
        centroid: GENERIC_TOUCHPAD_FEATURES,
        apply: ProfileApply {
            lenovo_x230_motion: false,
            trackpoint_multiplier: None,
            phantom_click_filter: false,
        },
    },
];

pub fn profile_inventory() -> impl Iterator<Item = ProfileInfo> {
    PROFILES.iter().map(|profile| ProfileInfo {
        id: profile.id,
        priority: profile.priority,
    })
}

pub fn scan_event_node(path: &Path) -> std::io::Result<ScannedDevice> {
    use evdev_upstream::{AbsoluteAxisCode, RelativeAxisCode};

    let mut bits = crate::capforge::CapabilityBits::from_sysfs_event_node(path);
    let properties = crate::hwdetect::udev_database_properties(path);
    let sys_device = Path::new("/sys/class/input")
        .join(path.file_name().unwrap_or_default())
        .join("device");
    let mut name = read_trimmed(sys_device.join("name"));
    let mut bus = read_hex_u16(sys_device.join("id/bustype"));
    let mut vendor = read_hex_u16(sys_device.join("id/vendor"));
    let mut product = read_hex_u16(sys_device.join("id/product"));

    if let Ok(device) = evdev_upstream::Device::open(path) {
        let input_id = device.input_id();
        name = device.name().unwrap_or("unknown").to_string();
        bus = input_id.bus_type().0;
        vendor = input_id.vendor();
        product = input_id.product();
        for event_type in device.supported_events().iter() {
            bits.set_event(event_type.0);
        }
        if let Some(codes) = device.supported_keys() {
            for code in codes.iter() {
                bits.set_key(code.0);
            }
        }
        if let Some(axes) = device.supported_relative_axes() {
            for axis in axes.iter() {
                bits.set_relative(axis.0);
            }
        }
        if let Some(axes) = device.supported_absolute_axes() {
            for axis in axes.iter() {
                bits.set_absolute(axis.0);
            }
        }
        for property in device.properties().iter() {
            bits.set_property(property.0);
        }
    } else if !sys_device.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "event node has no sysfs device",
        ));
    }

    Ok(ScannedDevice {
        path: path.to_path_buf(),
        name,
        bus,
        vendor,
        product,
        kind: bits.classify(),
        properties,
        has_relative_motion: bits.has_relative(RelativeAxisCode::REL_X.0)
            || bits.has_relative(RelativeAxisCode::REL_Y.0),
        has_absolute_xy: bits.has_absolute(AbsoluteAxisCode::ABS_X.0)
            && bits.has_absolute(AbsoluteAxisCode::ABS_Y.0),
        has_multitouch: bits.has_absolute(AbsoluteAxisCode::ABS_MT_POSITION_X.0)
            && bits.has_absolute(AbsoluteAxisCode::ABS_MT_POSITION_Y.0),
        key_count: bits.key_count(),
    })
}

fn read_hex_u16(path: impl AsRef<Path>) -> u16 {
    u16::from_str_radix(read_trimmed(path).trim_start_matches("0x"), 16).unwrap_or(0)
}

pub fn scan_system_facts() -> SystemFacts {
    let dmi_vendor = read_trimmed("/sys/class/dmi/id/sys_vendor");
    let dmi_product = read_trimmed("/sys/class/dmi/id/product_name");
    let dmi_product_version = read_trimmed("/sys/class/dmi/id/product_version");
    let dmi_board = read_trimmed("/sys/class/dmi/id/board_name");
    let cpu_vendor = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("vendor_id")
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(_, value)| value.trim().to_string())
            })
        })
        .unwrap_or_default();
    let vendor = dmi_vendor.to_ascii_lowercase();
    let product = dmi_product.to_ascii_lowercase();
    let product_version = dmi_product_version.to_ascii_lowercase();
    let board = dmi_board.to_ascii_lowercase();
    SystemFacts {
        thinkpad: vendor.contains("lenovo")
            && (product.contains("thinkpad")
                || product_version.contains("thinkpad")
                || board.contains("thinkpad")),
        apple: vendor.contains("apple")
            || product.contains("macbook")
            || product_version.contains("macbook"),
        handheld: ["jupiter", "galileo", "steam deck", "rog ally", "rc71"]
            .iter()
            .any(|needle| product.contains(needle) || board.contains(needle)),
        dmi_vendor,
        dmi_product,
        dmi_product_version,
        dmi_board,
        cpu_vendor,
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn contains_folded(value: &str, needle: &str) -> bool {
    value.to_ascii_lowercase().contains(needle)
}

fn rule_matches(rule: MatchRule, system: &SystemFacts, device: &DeviceFacts<'_>) -> bool {
    match rule {
        MatchRule::ThinkPad => system.thinkpad,
        MatchRule::Apple => system.apple,
        MatchRule::Handheld => system.handheld,
        MatchRule::DmiProduct(value) => {
            contains_folded(&system.dmi_product, value)
                || contains_folded(&system.dmi_product_version, value)
        }
        MatchRule::Name(value) => contains_folded(device.name, value),
        MatchRule::Kind(kind) => device.kind == kind,
        MatchRule::Udev(key, value) => device.properties.get(key).is_some_and(|v| v == value),
    }
}

fn features(system: &SystemFacts, device: &DeviceFacts<'_>) -> [f64; FEATURE_COUNT] {
    [
        f64::from(device.vendor) / f64::from(u16::MAX),
        f64::from(device.product) / f64::from(u16::MAX),
        f64::from(device.bus) / 32.0,
        f64::from(device.kind == CapabilityKind::Touchpad),
        f64::from(device.has_relative_motion),
        f64::from(device.has_absolute_xy),
        f64::from(device.has_multitouch),
        (device.key_count as f64 / 512.0).min(1.0),
        f64::from(system.thinkpad),
        f64::from(system.apple),
        f64::from(system.handheld),
        f64::from(contains_folded(device.name, "elan")),
        f64::from(contains_folded(device.name, "synaptics")),
        f64::from(contains_folded(device.name, "trackpoint")),
        f64::from(
            device
                .properties
                .get("ID_INPUT_TOUCHPAD")
                .is_some_and(|v| v == "1"),
        ),
        f64::from(
            device
                .properties
                .get("ID_INPUT_MOUSE")
                .is_some_and(|v| v == "1"),
        ),
    ]
}

pub fn select_profile(system: &SystemFacts, device: &DeviceFacts<'_>) -> Option<ProfileDecision> {
    let mut candidates = PROFILES
        .iter()
        .filter(|profile| {
            profile
                .rules
                .iter()
                .all(|rule| rule_matches(*rule, system, device))
        })
        .collect::<Vec<_>>();
    let priority = candidates.iter().map(|profile| profile.priority).max()?;
    candidates.retain(|profile| profile.priority == priority);
    if candidates.len() == 1 {
        let profile = candidates[0];
        return Some(ProfileDecision {
            id: profile.id,
            priority: profile.priority,
            score: 1.0,
            apply: profile.apply,
        });
    }

    let feature_tensor = features(system, device);
    let centroids = candidates
        .iter()
        .flat_map(|profile| profile.centroid)
        .collect::<Vec<_>>();
    let knn = knn_scores(&feature_tensor, &centroids, candidates.len());
    let hidden_bias = [0.0, 0.0, 0.0, 0.0];
    let mut input_weights = vec![0.0; FEATURE_COUNT * hidden_bias.len()];
    for (index, weight) in input_weights.iter_mut().enumerate() {
        *weight = if index % (FEATURE_COUNT + 1) == 0 {
            1.0
        } else {
            0.0
        };
    }
    let output_weights = vec![0.25; candidates.len() * hidden_bias.len()];
    let output_bias = vec![0.0; candidates.len()];
    let mlp = tiny_mlp_scores(
        &feature_tensor,
        &input_weights,
        &hidden_bias,
        &output_weights,
        &output_bias,
    );
    candidates
        .into_iter()
        .zip(knn.into_iter().zip(mlp))
        .map(|(profile, (knn_score, mlp_score))| ProfileDecision {
            id: profile.id,
            priority: profile.priority,
            score: knn_score * 0.75 + mlp_score * 0.25,
            apply: profile.apply,
        })
        .max_by(|left, right| {
            left.score
                .total_cmp(&right.score)
                .then_with(|| right.id.cmp(left.id))
        })
}

pub fn detect_device(
    system: &SystemFacts,
    device: &DeviceFacts<'_>,
    use_ml: bool,
) -> DetectionResult {
    if let Some(profile) = select_profile(system, device) {
        return DetectionResult {
            class: device.kind,
            profile_id: profile.id,
            priority: profile.priority,
            confidence: 1.0,
            source: DetectSource::Profile,
            apply: profile.apply,
        };
    }
    if use_ml {
        if let Some((class, confidence)) = ml_classify(system, device) {
            if class == device.kind && confidence >= 0.55 {
                return DetectionResult {
                    class,
                    profile_id: ml_profile_id(class),
                    priority: 5,
                    confidence,
                    source: DetectSource::MlFallback,
                    apply: ProfileApply::default(),
                };
            }
        }
    }
    DetectionResult {
        class: device.kind,
        profile_id: "heuristic",
        priority: 0,
        confidence: 0.4,
        source: DetectSource::Heuristic,
        apply: ProfileApply::default(),
    }
}

fn ml_profile_id(class: CapabilityKind) -> &'static str {
    match class {
        CapabilityKind::Keyboard => "ml-keyboard",
        CapabilityKind::Key => "ml-key",
        CapabilityKind::Mouse => "ml-mouse",
        CapabilityKind::Touchpad => "ml-touchpad",
        CapabilityKind::Touchscreen => "ml-touchscreen",
        CapabilityKind::Tablet => "ml-tablet",
        CapabilityKind::Joystick => "ml-joystick",
        CapabilityKind::Switch => "ml-switch",
        CapabilityKind::Unknown => "ml-unknown",
    }
}

fn ml_classify(system: &SystemFacts, device: &DeviceFacts<'_>) -> Option<(CapabilityKind, f64)> {
    const CLASSES: [CapabilityKind; 6] = [
        CapabilityKind::Keyboard,
        CapabilityKind::Mouse,
        CapabilityKind::Touchpad,
        CapabilityKind::Touchscreen,
        CapabilityKind::Tablet,
        CapabilityKind::Switch,
    ];
    const CENTROIDS: [[f64; FEATURE_COUNT]; 6] = [
        [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ],
        GENERIC_MOUSE_FEATURES,
        GENERIC_TOUCHPAD_FEATURES,
        [
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ],
        [
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ],
        [0.0; FEATURE_COUNT],
    ];
    let tensor = features(system, device);
    let flat_centroids = CENTROIDS.into_iter().flatten().collect::<Vec<_>>();
    let mut scores = knn_scores(&tensor, &flat_centroids, CLASSES.len());
    let hidden_bias = [0.0, 0.0, 0.0, 0.0];
    let input_weights = (0..FEATURE_COUNT * hidden_bias.len())
        .map(|index| {
            if index % (FEATURE_COUNT + 1) == 0 {
                1.0
            } else {
                0.0
            }
        })
        .collect::<Vec<_>>();
    let output_weights = vec![0.25; CLASSES.len() * hidden_bias.len()];
    let output_bias = vec![0.0; CLASSES.len()];
    let mlp = tiny_mlp_scores(
        &tensor,
        &input_weights,
        &hidden_bias,
        &output_weights,
        &output_bias,
    );
    for (score, mlp_score) in scores.iter_mut().zip(mlp) {
        *score = *score * 4.0 + mlp_score;
    }
    let maximum = scores.iter().copied().reduce(f64::max)?;
    let normalizer = scores
        .iter()
        .map(|score| (score - maximum).exp())
        .sum::<f64>();
    let (index, score) = scores
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))?;
    Some((CLASSES[index], (*score - maximum).exp() / normalizer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(name: &str, kind: CapabilityKind) -> DeviceFacts<'_> {
        DeviceFacts {
            name,
            bus: 0x11,
            vendor: 0x04f3,
            product: 0x0001,
            kind,
            properties: Box::leak(Box::default()),
            has_relative_motion: false,
            has_absolute_xy: true,
            has_multitouch: true,
            key_count: 3,
        }
    }

    #[test]
    fn p53_phantom_click_filter_requires_an_explicit_device_signature() {
        let system = SystemFacts {
            dmi_vendor: "LENOVO".into(),
            dmi_product: "ThinkPad P53".into(),
            thinkpad: true,
            ..SystemFacts::default()
        };
        let unsigned = facts("ELAN Touchpad", CapabilityKind::Touchpad);
        assert!(select_profile(&system, &unsigned).is_none());

        let mut properties = HashMap::new();
        properties.insert(
            "LIBINPUT_RS_P53_PHANTOM_CLICK_SIGNATURE".to_string(),
            "1".to_string(),
        );
        let signed = DeviceFacts {
            properties: &properties,
            ..unsigned
        };
        let decision = select_profile(&system, &signed).unwrap();
        assert_eq!(decision.id, "thinkpad-p53-elan");
        assert!(decision.apply.phantom_click_filter);
    }

    #[test]
    fn unmatched_device_cannot_be_invented_by_ml() {
        assert!(select_profile(
            &SystemFacts::default(),
            &facts("Unknown sensor", CapabilityKind::Unknown),
        )
        .is_none());
    }

    #[test]
    fn ml_fallback_cannot_cross_the_capability_lattice() {
        let unknown = facts("Unknown sensor", CapabilityKind::Unknown);
        let result = detect_device(&SystemFacts::default(), &unknown, true);
        assert_eq!(result.class, CapabilityKind::Unknown);
        assert_ne!(result.source, DetectSource::MlFallback);
    }
}
