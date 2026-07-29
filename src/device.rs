use crate::evdev;
use crate::evdev::{AbsoluteAxisCode, Device, EventType, InputEvent, KeyCode, RelativeAxisCode};
use log::{info, warn};
use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const REFERENCE_TOUCHPAD_RESOLUTION: f32 = 40.0;
const FALLBACK_MOTION_SCALE: f32 = 0.45;
const FALLBACK_SCROLL_SCALE: f32 = 0.02;
const POINTER_UNITS_PER_MM: f32 = FALLBACK_MOTION_SCALE * REFERENCE_TOUCHPAD_RESOLUTION;
const SCROLL_TICKS_PER_MM: f32 = FALLBACK_SCROLL_SCALE * REFERENCE_TOUCHPAD_RESOLUTION;
const MIN_NORMALIZED_SCALE: f32 = 0.01;
const MAX_NORMALIZED_SCALE: f32 = 2.0;
const DWT_TIMEOUT: Duration = Duration::from_millis(500);

fn should_suppress_new_touch(
    disable_while_typing: bool,
    elapsed_since_typing: Option<Duration>,
) -> bool {
    disable_while_typing && elapsed_since_typing.is_some_and(|elapsed| elapsed < DWT_TIMEOUT)
}

fn normalized_scale(resolution: Option<i32>, units_per_mm: f32, fallback: f32) -> f32 {
    let Some(resolution) = resolution.filter(|value| *value > 0) else {
        return fallback;
    };

    let scale = units_per_mm / resolution as f32;
    if scale.is_finite() && (MIN_NORMALIZED_SCALE..=MAX_NORMALIZED_SCALE).contains(&scale) {
        scale
    } else {
        fallback
    }
}

fn device_axis_scales(device: &Device) -> (f32, f32, f32) {
    let mut x_resolution = None;
    let mut y_resolution = None;
    let mut mt_x_resolution = None;
    let mut mt_y_resolution = None;

    if let Ok(absinfo) = device.get_absinfo() {
        for (axis, info) in absinfo {
            if axis == AbsoluteAxisCode::ABS_X {
                x_resolution = Some(info.resolution());
            } else if axis == AbsoluteAxisCode::ABS_Y {
                y_resolution = Some(info.resolution());
            } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                mt_x_resolution = Some(info.resolution());
            } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                mt_y_resolution = Some(info.resolution());
            }
        }
    }

    let x_resolution = x_resolution
        .filter(|value| *value > 0)
        .or(mt_x_resolution.filter(|value| *value > 0));
    let y_resolution = y_resolution
        .filter(|value| *value > 0)
        .or(mt_y_resolution.filter(|value| *value > 0));

    (
        normalized_scale(x_resolution, POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
        normalized_scale(y_resolution, POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
        normalized_scale(y_resolution, SCROLL_TICKS_PER_MM, FALLBACK_SCROLL_SCALE),
    )
}

pub struct DeviceWrapper {
    pub device: Device,
    pub path: PathBuf,
    pub is_absolute: bool,
    pub is_keyboard: bool,

    // Pointer state
    pub touch_active: bool,
    pub touch_fingers: u32,
    pub last_x: Option<i32>,
    pub last_y: Option<i32>,
    pub current_dx: i32,
    pub current_dy: i32,
    pub remainder_x: f32,
    pub remainder_y: f32,
    motion_scale_x: f32,
    motion_scale_y: f32,
    scroll_scale_y: f32,

    // Tap-to-click state
    pub touch_start_time: Option<Instant>,
    pub tap_emitted: bool,

    // DWT state
    pub last_typing_time: Option<Instant>,
    pub last_movement_time: Option<Instant>,
    pub active_click_button: Option<u16>,
    pub touch_suppressed: bool,
}

impl Drop for DeviceWrapper {
    fn drop(&mut self) {
        // Ensure device is properly released to avoid resource leaks
        let _ = self.device.ungrab();
        info!("Released device: {:?}", self.path);
    }
}

impl DeviceWrapper {
    pub fn new(device: Device, path: PathBuf) -> Self {
        let is_absolute = device.supported_events().contains(EventType::ABSOLUTE);
        let is_keyboard = device.supported_events().contains(EventType::KEY)
            && device
                .supported_keys()
                .is_some_and(|keys| keys.contains(KeyCode::KEY_A));
        let (motion_scale_x, motion_scale_y, scroll_scale_y) = device_axis_scales(&device);

        if is_absolute {
            info!(
                "Using normalized touchpad scales x={motion_scale_x:.4}, y={motion_scale_y:.4}, scroll={scroll_scale_y:.4} for {:?}",
                path
            );
        }

        Self {
            device,
            path,
            is_absolute,
            is_keyboard,
            touch_active: false,
            touch_fingers: 0,
            last_x: None,
            last_y: None,
            current_dx: 0,
            current_dy: 0,
            remainder_x: 0.0,
            remainder_y: 0.0,
            motion_scale_x,
            motion_scale_y,
            scroll_scale_y,
            touch_start_time: None,
            tap_emitted: false,
            last_typing_time: None,
            last_movement_time: None,
            active_click_button: None,
            touch_suppressed: false,
        }
    }

    pub fn process_event(
        &mut self,
        ev: InputEvent,
        v_device: &mut crate::virtual_device::VirtualDevice,
        config: &crate::config::InputConfig,
        last_global_typing_time: Option<Instant>,
    ) -> Result<(), Box<dyn Error>> {
        if ev.event_type() == EventType::KEY {
            let value = ev.value();

            if self.is_keyboard && value != 0 {
                // value 1 is press, 2 is repeat
                self.last_typing_time = Some(Instant::now());
            }

            // Keyboards are observed only for disable-while-typing. They are
            // never grabbed and must never be forwarded into the pointer-only
            // uinput device.
            if self.is_keyboard {
                return Ok(());
            }
        }

        if !self.is_absolute {
            // For relative devices (like trackpoints), just forward the event directly.
            v_device.emit_raw(ev)?;
            return Ok(());
        }

        // Disable-While-Typing (DWT) check - moved before tap logic
        let dwt_active = should_suppress_new_touch(
            config.disable_while_typing,
            last_global_typing_time.map(|typing_time| typing_time.elapsed()),
        );

        // Typing during an existing gesture must not freeze pointer motion,
        // but it should still prevent that gesture from becoming a tap.
        if dwt_active && self.touch_active {
            self.tap_emitted = true;
        }

        // For absolute devices (touchpads), convert coordinate events into relative movements
        match ev.event_type() {
            EventType::KEY => {
                if ev.code() == KeyCode::BTN_TOUCH.0 {
                    self.touch_active = ev.value() != 0;
                    if self.touch_active {
                        self.touch_start_time = Some(Instant::now());
                        self.touch_suppressed = dwt_active;
                        self.tap_emitted = self.touch_suppressed;
                        self.touch_fingers = 1;
                        self.last_x = None;
                        self.last_y = None;
                    } else {
                        // Reset tracking state when the finger is lifted

                        // Tap-to-click logic
                        if config.tap_to_click && !self.tap_emitted && !self.touch_suppressed {
                            if let Some(start) = self.touch_start_time {
                                if start.elapsed() < Duration::from_millis(250) {
                                    // Emit click with proper error handling
                                    v_device.emit_raw(InputEvent::new(
                                        EventType::KEY.0,
                                        KeyCode::BTN_LEFT.0,
                                        1,
                                    ))?;
                                    v_device.emit_raw(InputEvent::new(
                                        EventType::SYNCHRONIZATION.0,
                                        0,
                                        0,
                                    ))?;
                                    v_device.emit_raw(InputEvent::new(
                                        EventType::KEY.0,
                                        KeyCode::BTN_LEFT.0,
                                        0,
                                    ))?;
                                }
                            }
                        }

                        self.last_x = None;
                        self.last_y = None;
                        self.current_dx = 0;
                        self.current_dy = 0;
                        self.touch_start_time = None;
                        self.touch_fingers = 0;
                        self.touch_suppressed = false;
                    }
                } else if ev.code() == KeyCode::BTN_TOOL_DOUBLETAP.0 {
                    if ev.value() != 0 {
                        self.touch_fingers = 2;
                    } else {
                        self.touch_fingers = 1;
                    }
                } else if ev.code() == KeyCode::BTN_TOOL_TRIPLETAP.0 {
                    if ev.value() != 0 {
                        self.touch_fingers = 3;
                    } else {
                        self.touch_fingers = 2;
                    }
                }

                // Only emit standard buttons (left, right, middle) directly
                let mut code = ev.code();
                if code == KeyCode::BTN_LEFT.0 {
                    if ev.value() != 0 {
                        // Press event - map physical clickpad clicks to right/middle click based on finger count
                        if self.touch_fingers == 2 {
                            code = KeyCode::BTN_RIGHT.0;
                        } else if self.touch_fingers == 3 {
                            code = KeyCode::BTN_MIDDLE.0;
                        }
                        self.active_click_button = Some(code);
                    } else {
                        // Release event - use the same button code that was pressed
                        if let Some(active_code) = self.active_click_button {
                            code = active_code;
                        }
                        self.active_click_button = None;
                    }
                }

                if code == KeyCode::BTN_LEFT.0
                    || code == KeyCode::BTN_RIGHT.0
                    || code == KeyCode::BTN_MIDDLE.0
                {
                    v_device.emit_raw(InputEvent::new(EventType::KEY.0, code, ev.value()))?;
                }
            }
            EventType::ABSOLUTE => {
                let code = ev.code();

                // Only update last_movement_time when actual movement coordinates are received
                if code == AbsoluteAxisCode::ABS_X.0 || code == AbsoluteAxisCode::ABS_Y.0 {
                    // Only reset tracking if movement has stalled for more than 50ms
                    if let Some(last_time) = self.last_movement_time {
                        if last_time.elapsed() > Duration::from_millis(50) {
                            self.last_x = None;
                            self.last_y = None;
                        }
                    }
                    self.last_movement_time = Some(Instant::now());
                }

                if code == AbsoluteAxisCode::ABS_X.0 {
                    let val = ev.value();
                    if let Some(prev_x) = self.last_x {
                        self.current_dx += val - prev_x;
                    }
                    self.last_x = Some(val);
                } else if code == AbsoluteAxisCode::ABS_Y.0 {
                    let val = ev.value();
                    if let Some(prev_y) = self.last_y {
                        self.current_dy += val - prev_y;
                    }
                    self.last_y = Some(val);
                }
            }
            EventType::SYNCHRONIZATION => {
                if ev.code() == 0 {
                    // SYN_REPORT code is 0
                    let has_movement = self.current_dx != 0 || self.current_dy != 0;

                    if self.touch_suppressed {
                        // Throw away movement completely
                        self.current_dx = 0;
                        self.current_dy = 0;
                        self.remainder_x = 0.0;
                        self.remainder_y = 0.0;
                        self.tap_emitted = true; // prevent taps from happening
                    } else if has_movement {
                        self.tap_emitted = true; // Moved enough to cancel tap

                        if self.touch_fingers <= 1 {
                            // Normalize high-resolution absolute coordinates to physical motion.
                            // pointer_acceleration remains a user multiplier around neutral 1.0.
                            let total_x = (self.current_dx as f32 * self.motion_scale_x)
                                * config.pointer_acceleration
                                + self.remainder_x;
                            let total_y = (self.current_dy as f32 * self.motion_scale_y)
                                * config.pointer_acceleration
                                + self.remainder_y;

                            let emit_x = total_x.round() as i32;
                            let emit_y = total_y.round() as i32;

                            self.remainder_x = total_x - emit_x as f32;
                            self.remainder_y = total_y - emit_y as f32;

                            if emit_x != 0 {
                                v_device.emit_raw(InputEvent::new(
                                    EventType::RELATIVE.0,
                                    RelativeAxisCode::REL_X.0,
                                    emit_x,
                                ))?;
                            }
                            if emit_y != 0 {
                                v_device.emit_raw(InputEvent::new(
                                    EventType::RELATIVE.0,
                                    RelativeAxisCode::REL_Y.0,
                                    emit_y,
                                ))?;
                            }
                        } else if self.touch_fingers == 2 {
                            let total_y =
                                (self.current_dy as f32 * self.scroll_scale_y) + self.remainder_y;
                            let emit_wheel = total_y.round() as i32;
                            self.remainder_y = total_y - emit_wheel as f32;

                            if emit_wheel != 0 {
                                let mut final_wheel = emit_wheel;
                                // REL_WHEEL typically uses 1 for up, -1 for down.
                                // Moving fingers down the touchpad increases Y.
                                if config.natural_scrolling {
                                    final_wheel = -final_wheel;
                                }
                                v_device.emit_raw(InputEvent::new(
                                    EventType::RELATIVE.0,
                                    RelativeAxisCode::REL_WHEEL.0,
                                    final_wheel,
                                ))?;
                            }
                            // Also clear dx so it doesn't build up during scroll
                            self.remainder_x = 0.0;
                        }

                        self.current_dx = 0;
                        self.current_dy = 0;
                    }

                    // Keep a complete frame for accepted touches and idle
                    // state changes. Suppressed contacts intentionally emit
                    // no pointer frame.
                    if has_movement || !self.touch_suppressed {
                        v_device.emit_raw(ev)?;
                    }
                } else {
                    v_device.emit_raw(ev)?;
                }
            }
            _ => {
                // Do not emit unknown events from absolute devices to the relative virtual device
            }
        }

        Ok(())
    }
}

pub fn try_open_device(path: &std::path::Path) -> Option<DeviceWrapper> {
    if let Ok(mut device) = evdev::Device::open(path) {
        let name = device.name().unwrap_or("Unknown").to_string();
        info!("Checking device at {:?}: {}", path, name);
        // Never consume events emitted by our own uinput device. Doing so
        // creates a feedback loop: each forwarded event becomes readable
        // again and is forwarded forever. Match the legacy name as well as
        // the stable virtual input ID so upgrades remain safe.
        if crate::virtual_device::is_companion_device(device.name(), &device.input_id()) {
            info!("Ignoring companion output device at {:?}", path);
            return None;
        }

        let properties = device.properties();
        let is_touchpad = device.supported_events().contains(EventType::ABSOLUTE)
            && device
                .supported_keys()
                .is_some_and(|keys| keys.contains(KeyCode::BTN_TOUCH))
            && (properties.contains(evdev::PropType::POINTER)
                || properties.contains(evdev::PropType::BUTTONPAD));
        let is_keyboard = device.supported_events().contains(EventType::KEY)
            && device
                .supported_keys()
                .is_some_and(|keys| keys.contains(KeyCode::KEY_A));

        if is_touchpad {
            info!("Found touchpad hardware: {} at {:?}", name, path);
            if device.grab().is_ok() {
                return Some(DeviceWrapper::new(device, path.to_path_buf()));
            } else {
                warn!("Failed to grab touchpad device: {:?}", path);
            }
        } else if is_keyboard {
            info!("Found keyboard for DWT monitoring: {} at {:?}", name, path);
            return Some(DeviceWrapper::new(device, path.to_path_buf()));
        }
    } else {
        warn!("Failed to open device {:?}", path);
    }
    None
}

pub fn scan_input_devices() -> Result<Vec<DeviceWrapper>, Box<dyn Error>> {
    let mut tracked = Vec::new();

    for (path, _) in evdev::enumerate() {
        if let Some(wrapper) = try_open_device(&path) {
            tracked.push(wrapper);
        }
    }

    Ok(tracked)
}

#[cfg(test)]
mod tests {
    use super::{
        normalized_scale, should_suppress_new_touch, DWT_TIMEOUT, FALLBACK_MOTION_SCALE,
        FALLBACK_SCROLL_SCALE, POINTER_UNITS_PER_MM, SCROLL_TICKS_PER_MM,
    };

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.000_001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn reference_resolution_matches_live_calibration() {
        assert_close(
            normalized_scale(Some(40), POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
            0.45,
        );
        assert_close(
            normalized_scale(Some(40), SCROLL_TICKS_PER_MM, FALLBACK_SCROLL_SCALE),
            0.02,
        );
    }

    #[test]
    fn higher_resolution_produces_the_same_physical_motion() {
        assert_close(
            normalized_scale(Some(80), POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
            0.225,
        );
        assert_close(
            normalized_scale(Some(80), SCROLL_TICKS_PER_MM, FALLBACK_SCROLL_SCALE),
            0.01,
        );
    }

    #[test]
    fn missing_or_implausible_resolution_uses_calibrated_fallback() {
        assert_close(
            normalized_scale(None, POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
            FALLBACK_MOTION_SCALE,
        );
        assert_close(
            normalized_scale(Some(0), POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
            FALLBACK_MOTION_SCALE,
        );
        assert_close(
            normalized_scale(Some(100_000), POINTER_UNITS_PER_MM, FALLBACK_MOTION_SCALE),
            FALLBACK_MOTION_SCALE,
        );
    }

    #[test]
    fn dwt_only_suppresses_touches_that_begin_inside_the_timeout() {
        assert!(should_suppress_new_touch(
            true,
            Some(DWT_TIMEOUT - std::time::Duration::from_millis(1))
        ));
        assert!(!should_suppress_new_touch(true, Some(DWT_TIMEOUT)));
        assert!(!should_suppress_new_touch(true, None));
        assert!(!should_suppress_new_touch(
            false,
            Some(std::time::Duration::ZERO)
        ));
    }
}
