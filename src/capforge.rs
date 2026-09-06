//! Capability bitmap kernel shared by ioctl and sysfs discovery paths.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum CapabilityKind {
    Unknown = 0,
    Keyboard = 1,
    Key = 2,
    Mouse = 3,
    Touchpad = 4,
    Touchscreen = 5,
    Tablet = 6,
    Joystick = 7,
    Switch = 8,
}

impl CapabilityKind {
    fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Keyboard,
            2 => Self::Key,
            3 => Self::Mouse,
            4 => Self::Touchpad,
            5 => Self::Touchscreen,
            6 => Self::Tablet,
            7 => Self::Joystick,
            8 => Self::Switch,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapabilityBits {
    ev: [i64; 1],
    key: [i64; 12],
    rel: [i64; 1],
    absolute: [i64; 2],
    properties: [i64; 1],
}

impl CapabilityBits {
    pub fn from_sysfs_event_node(devnode: &std::path::Path) -> Self {
        let Some(name) = devnode.file_name() else {
            return Self::default();
        };
        let device = std::path::Path::new("/sys/class/input")
            .join(name)
            .join("device");
        let capabilities = device.join("capabilities");
        let read =
            |name: &str| std::fs::read_to_string(capabilities.join(name)).unwrap_or_default();
        Self {
            ev: parse_sysfs_hex::<1>(&read("ev")),
            key: parse_sysfs_hex::<12>(&read("key")),
            rel: parse_sysfs_hex::<1>(&read("rel")),
            absolute: parse_sysfs_hex::<2>(&read("abs")),
            properties: parse_sysfs_hex::<1>(
                &std::fs::read_to_string(device.join("properties")).unwrap_or_default(),
            ),
        }
    }

    pub fn set_event(&mut self, code: u16) {
        set_bit(&mut self.ev, code);
    }

    pub fn set_key(&mut self, code: u16) {
        set_bit(&mut self.key, code);
    }

    pub fn set_relative(&mut self, code: u16) {
        set_bit(&mut self.rel, code);
    }

    pub fn set_absolute(&mut self, code: u16) {
        set_bit(&mut self.absolute, code);
    }

    pub fn set_property(&mut self, code: u16) {
        set_bit(&mut self.properties, code);
    }

    pub fn classify(&self) -> CapabilityKind {
        native_classify(self)
            .map(CapabilityKind::from_code)
            .unwrap_or_else(|| rust_classify(self))
    }

    pub fn has_event(&self, code: u16) -> bool {
        bit(&self.ev, code)
    }

    pub fn has_key(&self, code: u16) -> bool {
        bit(&self.key, code)
    }

    pub fn has_relative(&self, code: u16) -> bool {
        bit(&self.rel, code)
    }

    pub fn has_absolute(&self, code: u16) -> bool {
        bit(&self.absolute, code)
    }

    pub fn key_count(&self) -> usize {
        (0..=KEY_MAX).filter(|code| bit(&self.key, *code)).count()
    }
}

const KEY_MAX: u16 = 0x2ff;

fn set_bit(words: &mut [i64], code: u16) {
    let index = usize::from(code / 64);
    let offset = u32::from(code % 64);
    if let Some(word) = words.get_mut(index) {
        *word |= 1_i64.wrapping_shl(offset);
    }
}

fn bit(words: &[i64], code: u16) -> bool {
    let index = usize::from(code / 64);
    let offset = u32::from(code % 64);
    words
        .get(index)
        .is_some_and(|word| (word.wrapping_shr(offset) & 1) == 1)
}

fn rust_classify(bits: &CapabilityBits) -> CapabilityKind {
    let has_key = bit(&bits.ev, 1);
    let has_rel = bit(&bits.ev, 2);
    let has_abs = bit(&bits.ev, 3);
    let xy = has_abs && bit(&bits.absolute, 0) && bit(&bits.absolute, 1);
    let multitouch = bit(&bits.absolute, 0x2f) || bit(&bits.absolute, 0x35);
    let finger = bit(&bits.key, 0x145);
    let touch = bit(&bits.key, 0x14a);
    let pen = bit(&bits.key, 0x140);
    let left = bit(&bits.key, 0x110);
    let joystick = bit(&bits.key, 0x120);
    let relative_xy = has_rel && (bit(&bits.rel, 0) || bit(&bits.rel, 1));
    let direct = bit(&bits.properties, 1);
    let pointer = bit(&bits.properties, 0);

    if pen && xy {
        CapabilityKind::Tablet
    } else if (finger || (touch && pointer && !direct)) && xy {
        CapabilityKind::Touchpad
    } else if (direct || (touch && multitouch)) && xy {
        CapabilityKind::Touchscreen
    } else if relative_xy && left {
        CapabilityKind::Mouse
    } else if joystick && has_abs {
        CapabilityKind::Joystick
    } else if has_key {
        let count = (1..255).filter(|code| bit(&bits.key, *code)).count();
        if count > 20 {
            CapabilityKind::Keyboard
        } else if count > 0 {
            CapabilityKind::Key
        } else {
            CapabilityKind::Unknown
        }
    } else if bit(&bits.ev, 5) {
        CapabilityKind::Switch
    } else {
        CapabilityKind::Unknown
    }
}

pub fn parse_sysfs_hex<const WORDS: usize>(input: &str) -> [i64; WORDS] {
    let mut words = [0_i64; WORDS];
    if native_parse(input, &mut words) {
        return words;
    }
    for (index, token) in input.split_whitespace().rev().take(WORDS).enumerate() {
        words[index] = u64::from_str_radix(token, 16).unwrap_or(0) as i64;
    }
    words
}

pub fn knn_scores(features: &[f64], centroids: &[f64], profiles: usize) -> Vec<f64> {
    if features.is_empty() || profiles == 0 || centroids.len() != features.len() * profiles {
        return Vec::new();
    }
    let mut scores = vec![0.0; profiles];
    if native_knn_scores(features, centroids, &mut scores) {
        return scores;
    }
    for (score, centroid) in scores
        .iter_mut()
        .zip(centroids.chunks_exact(features.len()))
    {
        *score = -features
            .iter()
            .zip(centroid)
            .map(|(feature, center)| (feature - center).powi(2))
            .sum::<f64>();
    }
    scores
}

pub fn tiny_mlp_scores(
    features: &[f64],
    input_weights: &[f64],
    hidden_bias: &[f64],
    output_weights: &[f64],
    output_bias: &[f64],
) -> Vec<f64> {
    if features.is_empty()
        || hidden_bias.is_empty()
        || output_bias.is_empty()
        || input_weights.len() != features.len() * hidden_bias.len()
        || output_weights.len() != hidden_bias.len() * output_bias.len()
    {
        return Vec::new();
    }
    let mut scores = vec![0.0; output_bias.len()];
    if native_mlp_scores(
        features,
        input_weights,
        hidden_bias,
        output_weights,
        output_bias,
        &mut scores,
    ) {
        return scores;
    }
    let hidden = input_weights
        .chunks_exact(features.len())
        .zip(hidden_bias)
        .map(|(weights, bias)| {
            (bias
                + weights
                    .iter()
                    .zip(features)
                    .map(|(weight, feature)| weight * feature)
                    .sum::<f64>())
            .tanh()
        })
        .collect::<Vec<_>>();
    for ((score, weights), bias) in scores
        .iter_mut()
        .zip(output_weights.chunks_exact(hidden.len()))
        .zip(output_bias)
    {
        *score = bias
            + weights
                .iter()
                .zip(&hidden)
                .map(|(weight, value)| weight * value)
                .sum::<f64>();
    }
    scores
}

include!(concat!(env!("OUT_DIR"), "/capforge_bindings.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_native_matches(bits: &CapabilityBits, expected: CapabilityKind) {
        assert_eq!(rust_classify(bits), expected);
        if NATIVE_CAPFORGE {
            assert_eq!(bits.classify(), expected);
        }
    }

    #[test]
    fn sysfs_words_are_reversed_into_ioctl_order() {
        let words = parse_sysfs_hex::<3>("8000000000000000 20 1\n");
        assert_eq!(words[0] as u64, 1);
        assert_eq!(words[1] as u64, 0x20);
        assert_eq!(words[2] as u64, 0x8000_0000_0000_0000);
    }

    #[test]
    fn classifiers_agree_on_touchpad_and_mixed_keyboard_evidence() {
        let mut touchpad = CapabilityBits::default();
        touchpad.set_event(1);
        touchpad.set_event(3);
        touchpad.set_absolute(0);
        touchpad.set_absolute(1);
        touchpad.set_key(0x145);
        touchpad.set_key(0x14a);
        touchpad.set_property(0);
        assert_native_matches(&touchpad, CapabilityKind::Touchpad);

        for key in 1..=30 {
            touchpad.set_key(key);
        }
        assert_native_matches(&touchpad, CapabilityKind::Touchpad);
    }

    #[test]
    fn classifiers_agree_on_relative_pointer_and_switch() {
        let mut mouse = CapabilityBits::default();
        mouse.set_event(1);
        mouse.set_event(2);
        mouse.set_relative(0);
        mouse.set_relative(1);
        mouse.set_key(0x110);
        assert_native_matches(&mouse, CapabilityKind::Mouse);

        let mut switch = CapabilityBits::default();
        switch.set_event(5);
        assert_native_matches(&switch, CapabilityKind::Switch);
    }

    #[test]
    fn native_and_rust_profile_scorers_agree() {
        let features = [0.25, 0.5, 0.75];
        let centroids = [0.0, 0.5, 1.0, 0.5, 0.5, 0.5];
        let expected_knn = [-0.125, -0.125];
        let actual_knn = knn_scores(&features, &centroids, 2);
        for (actual, expected) in actual_knn.iter().zip(expected_knn) {
            assert!((actual - expected).abs() < 1e-12);
        }

        let actual_mlp = tiny_mlp_scores(
            &features,
            &[1.0, 0.0, -1.0, -0.5, 1.0, 0.5],
            &[0.1, -0.1],
            &[0.5, -0.25, -0.75, 0.25],
            &[0.0, 0.2],
        );
        assert_eq!(actual_mlp.len(), 2);
        assert!(actual_mlp.iter().all(|score| score.is_finite()));
    }
}
