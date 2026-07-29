use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

static DATABASE_CACHE: OnceLock<Mutex<std::collections::HashMap<PathBuf, Arc<QuirksDatabase>>>> =
    OnceLock::new();

#[derive(Default)]
struct Section {
    name: String,
    matches: Vec<(String, String)>,
    event_codes: Vec<String>,
    input_props: Vec<String>,
    size_hint: Option<(f64, f64)>,
    resolution_hint: Option<(f64, f64)>,
    is_virtual: bool,
    model_lenovo_scrollpoint: bool,
    model_alps_serial_touchpad: bool,
    model_dell_canvas_totem: bool,
    model_apple_touchpad: bool,
    model_apple_touchpad_onebutton: bool,
    model_clickfinger_default: bool,
    model_wacom_touchpad: bool,
    model_trackball: bool,
    model_tablet_mode_switch_unreliable: bool,
    model_bouncing_keys: bool,
    model_hp_pavilion_dm4_touchpad: bool,
    model_hp_zbook_studio_g3: bool,
    model_invert_horizontal_scrolling: bool,
    model_lenovo_t450_touchpad: bool,
    model_lenovo_x1_gen6_touchpad: bool,
    model_lenovo_x230: bool,
    model_scroll_on_middle_click: bool,
    model_synaptics_serial_touchpad: bool,
    model_tablet_mode_no_suspend: bool,
    model_touchpad_phantom_clicks: bool,
    model_touchpad_visible_marker: Option<bool>,
    model_pressure_pad: bool,
    model_wacom_intuos_pro3rd: bool,
    tpkb_combo_layout_below: bool,
    keyboard_integration: Option<KeyboardIntegration>,
    pointing_stick_integration: Option<KeyboardIntegration>,
    palm_pressure_threshold: Option<u32>,
    palm_size_threshold: Option<u32>,
    thumb_pressure_threshold: Option<u32>,
    thumb_size_threshold: Option<u32>,
    trackpoint_multiplier: Option<f64>,
    tablet_smoothing: Option<bool>,
    msc_timestamp_watch: Option<bool>,
    pressure_range: Option<(i32, i32)>,
    touch_size_range: Option<(i32, i32)>,
    lid_switch_reliability: Option<String>,
    class_hint: Option<DeviceClassHint>,
    ignore_device: Option<bool>,
    parity_failures: Vec<String>,
}

/// Immutable facts used to resolve quirks before runtime classification.
pub struct QuirkProbe<'a> {
    pub name: &'a str,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub udev_types: &'a [&'a str],
}

struct DeviceIdentity<'a> {
    name: &'a str,
    bus: u16,
    vendor: u16,
    product: u16,
    version: u16,
    udev_types: &'a [&'a str],
    dmi_modalias: &'a str,
    device_tree: &'a str,
}

/// A quirk-provided primary class. The backend still requires the matching
/// kernel capability lattice, so a profile cannot invent device features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClassHint {
    Keyboard,
    Mouse,
    Touchpad,
    Touchscreen,
    Tablet,
    TabletPad,
    Switch,
}

/// How a keyboard is physically integrated with the system.  This is a
/// quirk-derived property: bus type alone is not enough to decide whether a
/// keyboard should participate in a touchpad's disable-while-typing state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardIntegration {
    Internal,
    External,
}

#[derive(Clone, Debug, Default)]
pub struct AppliedQuirks {
    pub matched_sections: Vec<String>,
    pub matched_files: Vec<PathBuf>,
    pub messages: Vec<String>,
    pub size_hint: Option<(f64, f64)>,
    pub resolution_hint: Option<(f64, f64)>,
    pub disable_hi_res_wheel_vertical: bool,
    pub disable_hi_res_wheel_horizontal: bool,
    pub disable_tablet_tilt_x: bool,
    pub disable_tablet_tilt_y: bool,
    pub disable_abs_distance: bool,
    pub disable_abs_mt_pressure: bool,
    pub disable_abs_pressure: bool,
    pub disable_abs_mt_tool_type: bool,
    pub disable_all_absolute_axes: bool,
    pub is_virtual: bool,
    pub model_lenovo_scrollpoint: bool,
    pub model_alps_serial_touchpad: bool,
    pub model_dell_canvas_totem: bool,
    pub model_apple_touchpad: bool,
    pub model_apple_touchpad_onebutton: bool,
    pub model_clickfinger_default: bool,
    pub model_wacom_touchpad: bool,
    pub model_trackball: bool,
    pub model_tablet_mode_switch_unreliable: bool,
    pub model_bouncing_keys: bool,
    pub model_hp_pavilion_dm4_touchpad: bool,
    pub model_hp_zbook_studio_g3: bool,
    pub model_invert_horizontal_scrolling: bool,
    pub model_lenovo_t450_touchpad: bool,
    pub model_lenovo_x1_gen6_touchpad: bool,
    pub model_lenovo_x230: bool,
    pub model_scroll_on_middle_click: bool,
    pub model_synaptics_serial_touchpad: bool,
    pub model_tablet_mode_no_suspend: bool,
    pub model_touchpad_phantom_clicks: bool,
    pub model_touchpad_visible_marker: bool,
    pub model_pressure_pad: bool,
    pub model_wacom_intuos_pro3rd: bool,
    pub tpkb_combo_layout_below: bool,
    pub keyboard_integration: Option<KeyboardIntegration>,
    pub pointing_stick_integration: Option<KeyboardIntegration>,
    pub palm_pressure_threshold: Option<u32>,
    pub palm_size_threshold: Option<u32>,
    pub thumb_pressure_threshold: Option<u32>,
    pub thumb_size_threshold: Option<u32>,
    pub trackpoint_multiplier: Option<f64>,
    pub tablet_smoothing: Option<bool>,
    pub msc_timestamp_watch: bool,
    pub pressure_range: Option<(i32, i32)>,
    pub touch_size_range: Option<(i32, i32)>,
    pub lid_switch_reliability: Option<String>,
    pub class_hint: Option<DeviceClassHint>,
    pub ignore_device: bool,
    pub enabled_input_props: Vec<String>,
    pub disabled_input_props: Vec<String>,
    pub parity_failures: Vec<String>,
}

/// An ordered, context-local snapshot of the quirks database. Existing
/// contexts keep one consistent view even if files change during a session.
pub struct QuirksEngine {
    database: Arc<QuirksDatabase>,
    dmi_modalias: String,
    device_tree: String,
}

struct QuirksDatabase {
    files: Vec<(PathBuf, String)>,
    validation_failures: Vec<String>,
}

impl AppliedQuirks {
    pub fn palm_pressure_or_default(&self) -> u32 {
        self.palm_pressure_threshold.unwrap_or(130)
    }

    pub fn trackpoint_multiplier_or_default(&self) -> f64 {
        self.trackpoint_multiplier.unwrap_or(1.0)
    }

    pub fn is_clickfinger_default(&self) -> bool {
        self.model_apple_touchpad
            || self.model_apple_touchpad_onebutton
            || self.model_clickfinger_default
    }
}

impl QuirksEngine {
    pub fn load_default() -> Self {
        let directory = std::env::var_os("LIBINPUT_QUIRKS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/share/libinput"));
        Self::load_directory(&directory)
    }

    fn load_directory(directory: &Path) -> Self {
        let database_cache = DATABASE_CACHE.get_or_init(|| Mutex::new(Default::default()));
        let database = database_cache
            .lock()
            .ok()
            .map(|mut cache| {
                if let Some(database) = cache.get(directory) {
                    return Arc::clone(database);
                }
                let database = Arc::new(load_database(directory));
                cache.insert(directory.to_path_buf(), Arc::clone(&database));
                database
            })
            .unwrap_or_else(|| Arc::new(load_database(directory)));
        let dmi_modalias = fs::read_to_string("/sys/devices/virtual/dmi/id/modalias")
            .unwrap_or_else(|_| "dmi:*".to_string());
        let device_tree = fs::read("/sys/firmware/devicetree/base/compatible")
            .ok()
            .and_then(|bytes| bytes.split(|byte| *byte == 0).next().map(Vec::from))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();
        Self {
            database,
            dmi_modalias,
            device_tree,
        }
    }

    /// Resolve the complete immutable quirk set for one probe. Raw udev roles
    /// are match evidence only; mutations feed the later classifier.
    pub fn resolve(&self, probe: &QuirkProbe<'_>, event_codes: &mut Vec<u16>) -> AppliedQuirks {
        let mut applied = AppliedQuirks {
            parity_failures: self.database.validation_failures.clone(),
            ..AppliedQuirks::default()
        };
        let identity = DeviceIdentity {
            name: probe.name,
            bus: probe.bus,
            vendor: probe.vendor,
            product: probe.product,
            version: probe.version,
            udev_types: probe.udev_types,
            dmi_modalias: self.dmi_modalias.trim(),
            device_tree: &self.device_tree,
        };
        for (path, contents) in &self.database.files {
            apply_file(path, contents, &identity, event_codes, &mut applied);
        }
        applied
    }
}

fn load_database(directory: &Path) -> QuirksDatabase {
    let mut validation_failures = validate_quirk_directory(directory).1;
    let mut paths = fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "quirks")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    paths.sort();
    let files = paths
        .into_iter()
        .filter_map(|path| match fs::read_to_string(&path) {
            Ok(contents) => Some((path, contents)),
            Err(error) => {
                validation_failures.push(format!("{}: {error}", path.display()));
                None
            }
        })
        .collect();
    QuirksDatabase {
        files,
        validation_failures,
    }
}

fn apply_file(
    path: &Path,
    contents: &str,
    identity: &DeviceIdentity<'_>,
    event_codes: &mut Vec<u16>,
    applied: &mut AppliedQuirks,
) {
    let mut section = Section::default();
    let mut in_section = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            if in_section {
                apply_section(path, &section, identity, event_codes, applied);
            }
            section = Section {
                name: line[1..line.len() - 1].to_string(),
                ..Section::default()
            };
            in_section = true;
            continue;
        }
        if !in_section || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.starts_with("Match") {
            if !matches!(
                key,
                "MatchName"
                    | "MatchBus"
                    | "MatchVendor"
                    | "MatchProduct"
                    | "MatchVersion"
                    | "MatchUdevType"
                    | "MatchDMIModalias"
                    | "MatchDeviceTree"
            ) {
                section
                    .parity_failures
                    .push(format!("unsupported match field {key}"));
            }
            section
                .matches
                .push((key.to_string(), value.trim().to_string()));
        } else if key == "AttrEventCode" {
            section.event_codes.push(value.trim().to_string());
        } else if key.trim() == "AttrInputProp" {
            section.input_props.push(value.trim().to_string());
        } else if key.trim() == "AttrSizeHint" {
            section.size_hint = parse_dimensions(value);
        } else if key.trim() == "AttrResolutionHint" {
            section.resolution_hint = parse_dimensions(value);
        } else if key.trim() == "AttrTPKComboLayout" {
            section.tpkb_combo_layout_below = value.trim().eq_ignore_ascii_case("below");
        } else if key.trim() == "AttrKeyboardIntegration" {
            section.keyboard_integration = match value.trim().to_ascii_lowercase().as_str() {
                "internal" => Some(KeyboardIntegration::Internal),
                "external" => Some(KeyboardIntegration::External),
                _ => None,
            };
        } else if key.trim() == "AttrPointingStickIntegration" {
            section.pointing_stick_integration = match value.trim().to_ascii_lowercase().as_str() {
                "internal" => Some(KeyboardIntegration::Internal),
                "external" => Some(KeyboardIntegration::External),
                _ => None,
            };
        } else if key.trim() == "AttrPalmPressureThreshold" {
            section.palm_pressure_threshold = value.trim().parse().ok();
        } else if key.trim() == "AttrPalmSizeThreshold" {
            section.palm_size_threshold = value.trim().parse().ok();
        } else if key.trim() == "AttrThumbPressureThreshold" {
            section.thumb_pressure_threshold = value.trim().parse().ok();
        } else if key.trim() == "AttrThumbSizeThreshold" {
            section.thumb_size_threshold = value.trim().parse().ok();
        } else if key.trim() == "AttrTrackpointMultiplier" {
            section.trackpoint_multiplier = value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|multiplier| multiplier.is_finite() && *multiplier > 0.0);
        } else if key.trim() == "AttrTabletSmoothing" {
            section.tablet_smoothing = parse_bool(value);
        } else if key.trim() == "AttrMscTimestamp" {
            section.msc_timestamp_watch = match value.trim() {
                "watch" => Some(true),
                "ignore" => Some(false),
                _ => None,
            };
        } else if key.trim() == "AttrPressureRange" {
            section.pressure_range = parse_pressure_range(value);
        } else if key.trim() == "AttrTouchSizeRange" {
            section.touch_size_range = parse_pressure_range(value);
        } else if key.trim() == "AttrLidSwitchReliability" {
            section.lid_switch_reliability = Some(value.trim().to_string());
        } else if key.trim() == "AttrDeviceClass" {
            section.class_hint = parse_device_class(value);
        } else if matches!(
            key.trim(),
            "AttrLibinputIgnore" | "ModelInputDeviceDisabled"
        ) {
            section.ignore_device = parse_bool(value);
        } else if key.trim() == "AttrIsVirtual" {
            section.is_virtual = value.trim() == "1";
        } else if key.trim() == "ModelLenovoScrollPoint" {
            section.model_lenovo_scrollpoint = value.trim() == "1";
        } else if key.trim() == "ModelALPSSerialTouchpad" {
            section.model_alps_serial_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelDellCanvasTotem" {
            section.model_dell_canvas_totem = value.trim() == "1";
        } else if key.trim() == "ModelAppleTouchpad" {
            section.model_apple_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelAppleTouchpadOneButton" {
            section.model_apple_touchpad_onebutton = value.trim() == "1";
        } else if key.trim() == "ModelWacomTouchpad" {
            section.model_wacom_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelTrackball" {
            section.model_trackball = value.trim() == "1";
        } else if key.trim() == "ModelTabletModeSwitchUnreliable" {
            section.model_tablet_mode_switch_unreliable = value.trim() == "1";
        } else if key.trim() == "ModelBouncingKeys" {
            section.model_bouncing_keys = value.trim() == "1";
        } else if key.trim() == "ModelHPPavilionDM4Touchpad" {
            section.model_hp_pavilion_dm4_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelHPZBookStudioG3" {
            section.model_hp_zbook_studio_g3 = value.trim() == "1";
        } else if key.trim() == "ModelInvertHorizontalScrolling" {
            section.model_invert_horizontal_scrolling = value.trim() == "1";
        } else if key.trim() == "ModelLenovoT450Touchpad" {
            section.model_lenovo_t450_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelLenovoX1Gen6Touchpad" {
            section.model_lenovo_x1_gen6_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelLenovoX230" {
            section.model_lenovo_x230 = value.trim() == "1";
        } else if key.trim() == "ModelScrollOnMiddleClick" {
            section.model_scroll_on_middle_click = value.trim() == "1";
        } else if key.trim() == "ModelSynapticsSerialTouchpad" {
            section.model_synaptics_serial_touchpad = value.trim() == "1";
        } else if key.trim() == "ModelTabletModeNoSuspend" {
            section.model_tablet_mode_no_suspend = value.trim() == "1";
        } else if key.trim() == "ModelTouchpadPhantomClicks" {
            section.model_touchpad_phantom_clicks = value.trim() == "1";
        } else if key.trim() == "ModelTouchpadVisibleMarker" {
            section.model_touchpad_visible_marker = parse_bool(value);
        } else if key.trim() == "ModelPressurePad" {
            section.model_pressure_pad = value.trim() == "1";
        } else if key.trim() == "ModelWacomIntuosPro3rd" {
            section.model_wacom_intuos_pro3rd = value.trim() == "1";
        } else if matches!(
            key,
            "ModelChromebook"
                | "ModelSystem76Bonobo"
                | "ModelSystem76Galago"
                | "ModelSystem76Kudu"
                | "ModelClevoW740SU"
        ) {
            section.model_clickfinger_default = value.trim() == "1";
        } else {
            section
                .parity_failures
                .push(format!("unsupported quirk field {key}"));
        }
    }
    if in_section {
        apply_section(path, &section, identity, event_codes, applied);
    }
}

fn apply_section(
    path: &Path,
    section: &Section,
    identity: &DeviceIdentity<'_>,
    event_codes: &mut Vec<u16>,
    applied: &mut AppliedQuirks,
) {
    if !section
        .matches
        .iter()
        .all(|(key, value)| match_property(key, value, identity))
    {
        return;
    }
    if !applied.matched_files.iter().any(|matched| matched == path) {
        applied.matched_files.push(path.to_path_buf());
    }
    applied.matched_sections.push(section.name.clone());
    applied
        .parity_failures
        .extend(section.parity_failures.iter().cloned());
    for expression in &section.event_codes {
        for token in expression
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let (enable, code_name) = match token.as_bytes().first() {
                Some(b'+') => (true, &token[1..]),
                Some(b'-') => (false, &token[1..]),
                _ => continue,
            };
            let relative_code = code_name.strip_prefix("EV_REL:").unwrap_or(code_name);
            if relative_code == "REL_WHEEL_HI_RES" {
                applied.disable_hi_res_wheel_vertical = !enable;
                continue;
            }
            if relative_code == "REL_HWHEEL_HI_RES" {
                applied.disable_hi_res_wheel_horizontal = !enable;
                continue;
            }
            let absolute_code = code_name.strip_prefix("EV_ABS:").unwrap_or(code_name);
            if absolute_code == "ABS_TILT_X" {
                applied.disable_tablet_tilt_x = !enable;
                continue;
            }
            if absolute_code == "ABS_TILT_Y" {
                applied.disable_tablet_tilt_y = !enable;
                continue;
            }
            if absolute_code == "ABS_DISTANCE" {
                applied.disable_abs_distance = !enable;
                continue;
            }
            if absolute_code == "ABS_MT_PRESSURE" {
                applied.disable_abs_mt_pressure = !enable;
                continue;
            }
            if absolute_code == "ABS_PRESSURE" {
                applied.disable_abs_pressure = !enable;
                continue;
            }
            if absolute_code == "ABS_MT_TOOL_TYPE" {
                applied.disable_abs_mt_tool_type = !enable;
                continue;
            }
            if code_name == "EV_ABS" {
                applied.disable_all_absolute_axes = !enable;
                continue;
            }
            let Some(code) = parse_key_code(code_name) else {
                applied
                    .parity_failures
                    .push(format!("unsupported AttrEventCode token {code_name}"));
                continue;
            };
            let action = if enable { "enabling" } else { "disabling" };
            applied.messages.push(format!(
                "{action} EV_KEY {}",
                key_code_name(code).unwrap_or(code_name)
            ));
            if enable {
                if !event_codes.contains(&code) {
                    event_codes.push(code);
                }
            } else {
                event_codes.retain(|existing| *existing != code);
            }
        }
    }
    for expression in &section.input_props {
        for token in expression
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let (enable, action, property) = match token.as_bytes().first() {
                Some(b'+') => (true, "enabling", &token[1..]),
                Some(b'-') => (false, "disabling", &token[1..]),
                _ => continue,
            };
            applied.messages.push(format!("{action} {property}"));
            applied
                .enabled_input_props
                .retain(|value| value != property);
            applied
                .disabled_input_props
                .retain(|value| value != property);
            if enable {
                applied.enabled_input_props.push(property.to_string());
            } else {
                applied.disabled_input_props.push(property.to_string());
            }
        }
    }
    if let Some(size) = section.size_hint {
        applied.size_hint = Some(size);
    }
    if let Some(resolution) = section.resolution_hint {
        applied.resolution_hint = Some(resolution);
    }
    applied.is_virtual |= section.is_virtual;
    applied.model_lenovo_scrollpoint |= section.model_lenovo_scrollpoint;
    applied.model_alps_serial_touchpad |= section.model_alps_serial_touchpad;
    applied.model_dell_canvas_totem |= section.model_dell_canvas_totem;
    applied.model_apple_touchpad |= section.model_apple_touchpad;
    applied.model_apple_touchpad_onebutton |= section.model_apple_touchpad_onebutton;
    applied.model_clickfinger_default |= section.model_clickfinger_default;
    applied.model_wacom_touchpad |= section.model_wacom_touchpad;
    applied.model_trackball |= section.model_trackball;
    applied.model_tablet_mode_switch_unreliable |= section.model_tablet_mode_switch_unreliable;
    applied.model_bouncing_keys |= section.model_bouncing_keys;
    applied.model_hp_pavilion_dm4_touchpad |= section.model_hp_pavilion_dm4_touchpad;
    applied.model_hp_zbook_studio_g3 |= section.model_hp_zbook_studio_g3;
    applied.model_invert_horizontal_scrolling |= section.model_invert_horizontal_scrolling;
    applied.model_lenovo_t450_touchpad |= section.model_lenovo_t450_touchpad;
    applied.model_lenovo_x1_gen6_touchpad |= section.model_lenovo_x1_gen6_touchpad;
    applied.model_lenovo_x230 |= section.model_lenovo_x230;
    applied.model_scroll_on_middle_click |= section.model_scroll_on_middle_click;
    applied.model_synaptics_serial_touchpad |= section.model_synaptics_serial_touchpad;
    applied.model_tablet_mode_no_suspend |= section.model_tablet_mode_no_suspend;
    applied.model_touchpad_phantom_clicks |= section.model_touchpad_phantom_clicks;
    if let Some(visible) = section.model_touchpad_visible_marker {
        applied.model_touchpad_visible_marker = visible;
    }
    applied.model_pressure_pad |= section.model_pressure_pad;
    applied.model_wacom_intuos_pro3rd |= section.model_wacom_intuos_pro3rd;
    applied.tpkb_combo_layout_below |= section.tpkb_combo_layout_below;
    if let Some(integration) = section.keyboard_integration {
        // Quirk files are applied in lexical order, matching the precedence
        // model of the upstream quirk database. A later matching rule is
        // therefore allowed to replace an earlier integration classification.
        applied.keyboard_integration = Some(integration);
    }
    if let Some(integration) = section.pointing_stick_integration {
        applied.pointing_stick_integration = Some(integration);
    }
    if let Some(threshold) = section.palm_pressure_threshold {
        applied.palm_pressure_threshold = Some(threshold);
    }
    if let Some(threshold) = section.palm_size_threshold {
        applied.palm_size_threshold = Some(threshold);
    }
    if let Some(threshold) = section.thumb_pressure_threshold {
        applied.thumb_pressure_threshold = Some(threshold);
    }
    if let Some(threshold) = section.thumb_size_threshold {
        applied.thumb_size_threshold = Some(threshold);
    }
    if let Some(multiplier) = section.trackpoint_multiplier {
        applied.trackpoint_multiplier = Some(multiplier);
    }
    if let Some(smoothing) = section.tablet_smoothing {
        applied.tablet_smoothing = Some(smoothing);
    }
    if let Some(watch) = section.msc_timestamp_watch {
        applied.msc_timestamp_watch = watch;
    }
    if let Some(range) = section.pressure_range {
        applied.pressure_range = Some(range);
    }
    if let Some(range) = section.touch_size_range {
        applied.touch_size_range = Some(range);
    }
    if let Some(reliability) = &section.lid_switch_reliability {
        applied.lid_switch_reliability = Some(reliability.clone());
    }
    if let Some(class_hint) = section.class_hint {
        applied.class_hint = Some(class_hint);
    }
    if let Some(ignore_device) = section.ignore_device {
        applied.ignore_device = ignore_device;
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn parse_device_class(value: &str) -> Option<DeviceClassHint> {
    match value.trim().to_ascii_lowercase().as_str() {
        "keyboard" => Some(DeviceClassHint::Keyboard),
        "mouse" | "pointingstick" => Some(DeviceClassHint::Mouse),
        "touchpad" => Some(DeviceClassHint::Touchpad),
        "touchscreen" => Some(DeviceClassHint::Touchscreen),
        "tablet" => Some(DeviceClassHint::Tablet),
        "tablet-pad" => Some(DeviceClassHint::TabletPad),
        "switch" => Some(DeviceClassHint::Switch),
        _ => None,
    }
}

fn validate_quirk_entry(key: &str, value: &str) -> Option<String> {
    let invalid = || Some(format!("invalid {key} value {value}"));
    match key {
        "MatchName" | "MatchDMIModalias" | "MatchDeviceTree" => {
            if value.is_empty() {
                invalid()
            } else {
                None
            }
        }
        "MatchBus" => {
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "usb" | "bluetooth" | "ps2" | "i8042" | "i2c" | "spi" | "rmi"
            ) {
                None
            } else {
                invalid()
            }
        }
        "MatchVendor" | "MatchProduct" | "MatchVersion" => {
            if parse_number(value).is_some() {
                None
            } else {
                invalid()
            }
        }
        "MatchUdevType" => {
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "keyboard"
                    | "key"
                    | "mouse"
                    | "pointingstick"
                    | "touchpad"
                    | "touchscreen"
                    | "tablet"
                    | "tablet-pad"
                    | "switch"
            ) {
                None
            } else {
                invalid()
            }
        }
        "AttrEventCode" => invalid_event_code_token(value)
            .map(|token| format!("unsupported AttrEventCode token {token}")),
        "AttrInputProp" => invalid_input_prop_token(value)
            .map(|token| format!("unsupported AttrInputProp token {token}")),
        "AttrSizeHint" | "AttrResolutionHint" => {
            if parse_dimensions(value).is_some() {
                None
            } else {
                invalid()
            }
        }
        "AttrTPKComboLayout" => {
            if value.eq_ignore_ascii_case("below") {
                None
            } else {
                invalid()
            }
        }
        "AttrKeyboardIntegration" | "AttrPointingStickIntegration" => {
            if matches!(value.to_ascii_lowercase().as_str(), "internal" | "external") {
                None
            } else {
                invalid()
            }
        }
        "AttrPalmPressureThreshold"
        | "AttrPalmSizeThreshold"
        | "AttrThumbPressureThreshold"
        | "AttrThumbSizeThreshold" => {
            if value.parse::<u32>().is_ok() {
                None
            } else {
                invalid()
            }
        }
        "AttrTrackpointMultiplier" => {
            if value
                .parse::<f64>()
                .ok()
                .is_some_and(|number| number.is_finite() && number > 0.0)
            {
                None
            } else {
                invalid()
            }
        }
        "AttrTabletSmoothing" | "AttrIsVirtual" | "ModelTouchpadVisibleMarker" => {
            if parse_bool(value).is_some() {
                None
            } else {
                invalid()
            }
        }
        "AttrMscTimestamp" => {
            if value == "watch" {
                None
            } else {
                invalid()
            }
        }
        "AttrPressureRange" | "AttrTouchSizeRange" => {
            if parse_pressure_range(value).is_some() {
                None
            } else {
                invalid()
            }
        }
        "AttrLidSwitchReliability" => {
            if matches!(value, "reliable" | "unreliable" | "write_open") {
                None
            } else {
                invalid()
            }
        }
        "AttrDeviceClass" => {
            if parse_device_class(value).is_some() {
                None
            } else {
                invalid()
            }
        }
        "AttrLibinputIgnore" | "ModelInputDeviceDisabled" => {
            if parse_bool(value).is_some() {
                None
            } else {
                invalid()
            }
        }
        key if key.starts_with("Model") => {
            if matches!(
                key,
                "ModelLenovoScrollPoint"
                    | "ModelALPSSerialTouchpad"
                    | "ModelDellCanvasTotem"
                    | "ModelAppleTouchpad"
                    | "ModelAppleTouchpadOneButton"
                    | "ModelWacomTouchpad"
                    | "ModelTrackball"
                    | "ModelTabletModeSwitchUnreliable"
                    | "ModelBouncingKeys"
                    | "ModelHPPavilionDM4Touchpad"
                    | "ModelHPZBookStudioG3"
                    | "ModelInvertHorizontalScrolling"
                    | "ModelLenovoT450Touchpad"
                    | "ModelLenovoX1Gen6Touchpad"
                    | "ModelLenovoX230"
                    | "ModelScrollOnMiddleClick"
                    | "ModelSynapticsSerialTouchpad"
                    | "ModelTabletModeNoSuspend"
                    | "ModelTouchpadPhantomClicks"
                    | "ModelPressurePad"
                    | "ModelWacomIntuosPro3rd"
                    | "ModelChromebook"
                    | "ModelSystem76Bonobo"
                    | "ModelSystem76Galago"
                    | "ModelSystem76Kudu"
                    | "ModelClevoW740SU"
            ) && parse_bool(value).is_some()
            {
                None
            } else {
                Some(format!("unsupported or invalid quirk field {key}={value}"))
            }
        }
        _ => Some(format!("unsupported quirk field {key}")),
    }
}

fn invalid_event_code_token(value: &str) -> Option<&str> {
    value
        .split(';')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .find(|token| {
            if token.len() < 2 {
                return true;
            }
            let (sign, code) = token.split_at(1);
            if !matches!(sign, "+" | "-") {
                return true;
            }
            !(matches!(
                code,
                "REL_WHEEL_HI_RES"
                    | "REL_HWHEEL_HI_RES"
                    | "EV_REL:REL_WHEEL_HI_RES"
                    | "EV_REL:REL_HWHEEL_HI_RES"
                    | "ABS_TILT_X"
                    | "ABS_TILT_Y"
                    | "ABS_DISTANCE"
                    | "ABS_MT_PRESSURE"
                    | "ABS_PRESSURE"
                    | "ABS_MT_TOOL_TYPE"
                    | "EV_ABS"
            ) || parse_key_code(code).is_some())
        })
}

fn invalid_input_prop_token(value: &str) -> Option<&str> {
    value
        .split(';')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .find(|token| {
            if token.len() < 2 {
                return true;
            }
            let (sign, property) = token.split_at(1);
            !matches!(sign, "+" | "-")
                || !matches!(
                    property,
                    "INPUT_PROP_POINTER"
                        | "INPUT_PROP_DIRECT"
                        | "INPUT_PROP_BUTTONPAD"
                        | "INPUT_PROP_SEMI_MT"
                        | "INPUT_PROP_TOPBUTTONPAD"
                        | "INPUT_PROP_POINTING_STICK"
                        | "INPUT_PROP_ACCELEROMETER"
                        | "INPUT_PROP_PRESSUREPAD"
                )
        })
}

pub fn validate_quirk_directory(directory: &Path) -> (usize, Vec<String>) {
    let mut files = match fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "quirks")
            })
            .collect::<Vec<_>>(),
        Err(error) => return (0, vec![format!("{}: {error}", directory.display())]),
    };
    files.sort();
    let mut failures = Vec::new();
    for path in &files {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) => {
                failures.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let mut in_section = false;
        for (line_number, raw_line) in contents.lines().enumerate() {
            let line = raw_line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                in_section = true;
                continue;
            }
            if !in_section || line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                failures.push(format!(
                    "{}:{}: malformed quirk entry",
                    path.display(),
                    line_number + 1
                ));
                continue;
            };
            if let Some(error) = validate_quirk_entry(key.trim(), value.trim()) {
                failures.push(format!("{}:{}: {error}", path.display(), line_number + 1));
            }
        }
    }
    (files.len(), failures)
}

fn parse_pressure_range(value: &str) -> Option<(i32, i32)> {
    let (down, up) = value.trim().split_once(':')?;
    let down = down.parse().ok()?;
    let up = up.parse().ok()?;
    (down >= up).then_some((down, up))
}

fn match_property(key: &str, value: &str, identity: &DeviceIdentity<'_>) -> bool {
    match key {
        "MatchName" => glob_matches(value, identity.name),
        "MatchBus" => match value.to_ascii_lowercase().as_str() {
            "usb" => identity.bus == 0x03,
            "bluetooth" => identity.bus == 0x05,
            "ps2" | "i8042" => identity.bus == 0x11,
            "i2c" => identity.bus == 0x18,
            "spi" => identity.bus == 0x1c,
            "rmi" => identity.bus == 0x1d,
            _ => false,
        },
        "MatchVendor" => parse_number(value).is_some_and(|number| number == identity.vendor),
        "MatchProduct" => parse_number(value).is_some_and(|number| number == identity.product),
        "MatchVersion" => parse_number(value).is_some_and(|number| number == identity.version),
        "MatchUdevType" => identity
            .udev_types
            .iter()
            .any(|udev_type| value.eq_ignore_ascii_case(udev_type)),
        "MatchDMIModalias" => glob_matches(value, identity.dmi_modalias),
        "MatchDeviceTree" => glob_matches(value, identity.device_tree),
        // A match constraint we cannot establish must not broaden a quirk.
        _ => false,
    }
}

fn parse_dimensions(value: &str) -> Option<(f64, f64)> {
    let (x, y) = value.trim().split_once('x')?;
    let x: f64 = x.parse().ok()?;
    let y: f64 = y.parse().ok()?;
    (x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0).then_some((x, y))
}

fn parse_number(value: &str) -> Option<u16> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

fn parse_key_code(name: &str) -> Option<u16> {
    if let Some(code) = name.strip_prefix("EV_KEY:") {
        return parse_number(code);
    }
    Some(match name {
        "BTN_LEFT" => 0x110,
        "BTN_RIGHT" => 0x111,
        "BTN_MIDDLE" => 0x112,
        "BTN_SIDE" => 0x113,
        "BTN_EXTRA" => 0x114,
        "BTN_FORWARD" => 0x115,
        "BTN_BACK" => 0x116,
        "BTN_TASK" => 0x117,
        "BTN_0" => 0x100,
        "BTN_TOOL_DOUBLETAP" => 0x14d,
        "BTN_TOOL_TRIPLETAP" => 0x14e,
        "BTN_TOOL_QUADTAP" => 0x14f,
        "BTN_TOOL_QUINTTAP" => 0x150,
        "KEY_F1" => 59,
        "KEY_F2" => 60,
        "KEY_F3" => 61,
        _ => return None,
    })
}

fn key_code_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0x110 => "BTN_LEFT",
        0x111 => "BTN_RIGHT",
        0x112 => "BTN_MIDDLE",
        0x113 => "BTN_SIDE",
        0x114 => "BTN_EXTRA",
        0x115 => "BTN_FORWARD",
        0x116 => "BTN_BACK",
        0x117 => "BTN_TASK",
        0x14d => "BTN_TOOL_DOUBLETAP",
        0x14e => "BTN_TOOL_TRIPLETAP",
        0x14f => "BTN_TOOL_QUADTAP",
        0x150 => "BTN_TOOL_QUINTTAP",
        59 => "KEY_F1",
        60 => "KEY_F2",
        61 => "KEY_F3",
        _ => return None,
    })
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v, mut star, mut retry) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(star_position) = star {
            p = star_position + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_supports_quirk_name_patterns() {
        assert!(glob_matches(
            "*Logitech*Marble*",
            "Logitech USB Marble Mouse"
        ));
        assert!(!glob_matches(
            "*Logitech*M575*",
            "Logitech USB Marble Mouse"
        ));
    }

    #[test]
    fn parses_numeric_and_named_key_codes() {
        assert_eq!(parse_key_code("EV_KEY:0x118"), Some(0x118));
        assert_eq!(parse_key_code("BTN_MIDDLE"), Some(0x112));
        assert_eq!(parse_key_code("EV_ABS:0x00"), None);
    }

    #[test]
    fn parses_positive_dimensions() {
        assert_eq!(parse_dimensions("100x55"), Some((100.0, 55.0)));
        assert_eq!(parse_dimensions("0x55"), None);
        assert_eq!(parse_dimensions("invalid"), None);
    }

    #[test]
    fn applies_apple_touchpad_onebutton_quirk_by_identity() {
        std::env::set_var(
            "LIBINPUT_QUIRKS_DIR",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quirks"),
        );
        let engine = QuirksEngine::load_default();
        let mut event_codes = Vec::new();
        let applied = engine.resolve(
            &QuirkProbe {
                name: "litest appletouch",
                bus: 0x03,
                vendor: 0x05ac,
                product: 0x021a,
                version: 0x00,
                udev_types: &["touchpad"],
            },
            &mut event_codes,
        );
        assert!(applied.model_apple_touchpad_onebutton);
        assert_eq!(applied.matched_sections, ["Apple Touchpad One Button"]);
        assert_eq!(applied.matched_files.len(), 1);

        let applied = engine.resolve(
            &QuirkProbe {
                name: "libinput-rs quirk override test",
                bus: 0x03,
                vendor: 0,
                product: 0,
                version: 0,
                udev_types: &["touchpad"],
            },
            &mut event_codes,
        );
        assert!(applied.ignore_device);
        assert_eq!(applied.class_hint, Some(DeviceClassHint::Touchpad));
        assert_eq!(applied.matched_sections, ["Resolved Class And Ignore"]);
    }

    #[test]
    fn quirk_database_fields_have_runtime_semantics() {
        let directory = std::env::var_os("LIBINPUT_RS_QUIRKS_CHECK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quirks")));
        let (file_count, failures) = validate_quirk_directory(&directory);
        assert!(
            file_count > 0,
            "no quirk files found in {}",
            directory.display()
        );
        assert!(
            failures.is_empty(),
            "quirk parity gate failed:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn malformed_recognized_values_fail_the_parity_gate() {
        assert!(validate_quirk_entry("AttrSizeHint", "0x40").is_some());
        assert!(validate_quirk_entry("AttrMscTimestamp", "ignore").is_some());
        assert!(validate_quirk_entry("ModelAppleTouchpad", "yes").is_some());
        assert!(validate_quirk_entry("AttrInputProp", "+INPUT_PROP_UNKNOWN").is_some());
        assert!(validate_quirk_entry("UnknownRequiredField", "1").is_some());
    }

    #[test]
    fn every_kernel_input_property_has_signed_runtime_semantics() {
        for property in [
            "INPUT_PROP_POINTER",
            "INPUT_PROP_DIRECT",
            "INPUT_PROP_BUTTONPAD",
            "INPUT_PROP_SEMI_MT",
            "INPUT_PROP_TOPBUTTONPAD",
            "INPUT_PROP_POINTING_STICK",
            "INPUT_PROP_ACCELEROMETER",
            "INPUT_PROP_PRESSUREPAD",
        ] {
            assert!(validate_quirk_entry("AttrInputProp", &format!("+{property}")).is_none());
            assert!(validate_quirk_entry("AttrInputProp", &format!("-{property}")).is_none());
        }
    }
}
