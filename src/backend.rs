//! Evdev backend for the libinput-rs shared library.
//!
//! BackendState owns every open DeviceWrapper and provides a single
//! drain() call that reads all pending kernel events, applies
//! motion/scroll/DWT/tap/pinch/keyboard logic, and appends finished
//! LibinputEvents to a caller-supplied queue.

use std::collections::{HashMap, VecDeque};
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use evdev::{AbsoluteAxisCode, Device, EventType, InputEvent, KeyCode, RelativeAxisCode};
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

use crate::ffi_types::{
    BackendKind, EventPayload, GestureEvent, KeyboardKeyEvent, LibinputContext, LibinputDevice,
    LibinputEvent, LibinputEventType, LibinputTabletTool, PointerAxisEvent, PointerButtonEvent,
    PointerMotionAbsoluteEvent, PointerMotionEvent, TabletToolEvent, TouchEvent,
};

#[link(name = "wacom")]
extern "C" {
    fn libwacom_database_new() -> *mut libc::c_void;
    fn libwacom_database_destroy(database: *mut libc::c_void);
    fn libwacom_stylus_get_for_id(
        database: *const libc::c_void,
        tool_id: libc::c_int,
    ) -> *const libc::c_void;
    fn libwacom_stylus_get_name(stylus: *const libc::c_void) -> *const libc::c_char;
    fn libwacom_stylus_is_generic(stylus: *const libc::c_void) -> libc::c_int;
}

struct RestrictedFdGuard {
    fd: RawFd,
    close: Option<unsafe extern "C" fn(RawFd, *mut libc::c_void)>,
    user_data: *mut libc::c_void,
    armed: bool,
}

impl RestrictedFdGuard {
    fn new(
        fd: RawFd,
        close: Option<unsafe extern "C" fn(RawFd, *mut libc::c_void)>,
        user_data: *mut libc::c_void,
    ) -> Self {
        Self {
            fd,
            close,
            user_data,
            armed: true,
        }
    }

    fn disarm(mut self) -> RawFd {
        self.armed = false;
        self.fd
    }
}

impl Drop for RestrictedFdGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Some(close) = self.close {
                unsafe { close(self.fd, self.user_data) };
            } else {
                unsafe { libc::close(self.fd) };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-touch slot
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct MtSlot {
    active: bool,
    reported: bool,
    dirty: bool,
    palm_suppressed: bool,
    cancel_pending: bool,
    seat_slot: Option<i32>,
    tracking_id: i32,
    tool_type: i32,
    x: f64,
    y: f64,
    distance: f64, // ABS_MT_DISTANCE (for stylus / palm)
}

// ---------------------------------------------------------------------------
// Key-repeat tracking
// ---------------------------------------------------------------------------

const REPEAT_DELAY_MS: u64 = 200;
const REPEAT_INTERVAL_MS: u64 = 25;

#[derive(Clone)]
struct HeldKey {
    code: u16,
    ts_usec: u64,
    last_fire: Instant,
    initial_fired: bool,
}

// ---------------------------------------------------------------------------
// Per-device tracking state
// ---------------------------------------------------------------------------

struct TrackedDevice {
    device: Device,
    restricted_fd: Option<RawFd>,
    path: PathBuf,
    is_absolute: bool,
    is_absolute_pointer: bool,
    has_mt: bool,
    protocol_a: bool,
    mt_x_fuzz: i32,
    mt_y_fuzz: i32,
    is_keyboard: bool,
    is_pointer: bool,
    is_topbuttonpad: bool,
    is_pointing_stick: bool,
    supports_hi_res_vertical: bool,
    supports_hi_res_horizontal: bool,
    warned_missing_hi_res_vertical: bool,
    warned_missing_hi_res_horizontal: bool,
    wheel_is_virtual: bool,
    is_lenovo_scrollpoint: bool,
    wheel_state: u8,
    wheel_min_movement: i64,
    wheel_accum_vertical: i64,
    wheel_accum_horizontal: i64,
    wheel_direction: i8,
    wheel_last_emit: Option<Instant>,
    scroll_button_down: bool,
    scroll_lock_active: bool,
    scroll_button_lock_press: bool,
    scroll_button_press_time: Option<Instant>,
    scroll_button_moved: bool,
    scroll_button_axes: u8,
    middle_left_down: bool,
    middle_right_down: bool,
    middle_chord_active: bool,
    middle_pending_button: Option<u16>,
    middle_pending_since: Option<Instant>,
    middle_suppressed_left: bool,
    middle_suppressed_right: bool,
    middle_real_down: bool,
    left_handed_applied: bool,
    debounce_buttons: HashMap<u16, DebounceButton>,
    debounce_spurious: bool,
    debounce_bypass: bool,
    scroll_button_accum_x: i64,
    scroll_button_accum_y: i64,

    // --- relative / button ---
    pending_rel_x: i64,
    pending_rel_y: i64,
    pending_wheel_vertical: i64,
    pending_wheel_horizontal: i64,
    pending_wheel_hi_res_vertical: i64,
    pending_wheel_hi_res_horizontal: i64,
    current_abs_x: Option<i32>,
    current_abs_y: Option<i32>,
    absolute_changed: bool,
    remainder_x: f32,
    remainder_y: f32,

    // --- absolute / touchpad ---
    touch_active: bool,
    touch_fingers: u32,
    last_x: Option<i32>,
    last_y: Option<i32>,
    current_dx: i32,
    current_dy: i32,
    abs_x_range: Option<(i32, i32)>,
    abs_y_range: Option<(i32, i32)>,
    axis_range_warning_at: Option<Instant>,
    tap_emitted: bool,
    touch_start_time: Option<Instant>,
    last_movement_time: Option<Instant>,
    active_click_button: Option<u16>,
    active_click_device: Option<*mut LibinputDevice>,

    // --- tablet tool ---
    tablet_serial: u64,
    tablet_tool_id: u64,
    tablet_tool_type: u32,
    tablet_proximity_pending: Option<bool>,
    tablet_tool: *mut LibinputTabletTool,
    tablet_has_pressure: bool,
    tablet_has_distance: bool,
    tablet_has_tilt: bool,
    tablet_has_rotation: bool,
    tablet_has_slider: bool,
    tablet_has_wheel: bool,
    tablet_has_size: bool,
    tablet_buttons: Vec<u32>,
    tablet_x: f64,
    tablet_y: f64,
    tablet_x_changed: bool,
    tablet_y_changed: bool,
    tablet_axes_changed: bool,
    tablet_pressure: f64,
    tablet_pressure_range: Option<(i32, i32)>,
    tablet_pressure_changed: bool,
    tablet_distance: f64,
    tablet_distance_range: Option<(i32, i32)>,
    tablet_distance_changed: bool,
    tablet_tilt_x: f64,
    tablet_tilt_y: f64,
    tablet_tilt_x_info: Option<(i32, i32, i32)>,
    tablet_tilt_y_info: Option<(i32, i32, i32)>,
    tablet_tilt_x_changed: bool,
    tablet_tilt_y_changed: bool,
    tablet_rotation: f64,
    tablet_rotation_info: Option<(i32, i32)>,
    tablet_rotation_changed: bool,
    tablet_slider: f64,
    tablet_slider_range: Option<(i32, i32)>,
    tablet_slider_changed: bool,
    tablet_wheel_delta: f64,
    tablet_wheel_discrete: i32,
    tablet_wheel_changed: bool,
    tablet_tip_down: bool,
    tablet_tip_pending: Option<bool>,
    tablet_zero_pressure_since: Option<Instant>,
    tablet_proximity_timer_enabled: bool,
    tablet_buttons_down: u32,
    tablet_held_buttons: Vec<u32>,

    // --- multi-touch slots (for pinch) ---
    mt_slots: Vec<MtSlot>,
    current_slot: usize,
    protocol_a_tracking_id: Option<i32>,
    protocol_a_x: Option<f64>,
    protocol_a_y: Option<f64>,
    protocol_a_contacts: Vec<(i32, f64, f64)>,
    pinch_active: bool,
    pinch_base_dist: f64,
    pinch_base_angle: f64,
    pinch_fingers: i32,

    // --- keyboard repeat ---
    held_keys: Vec<HeldKey>,
    held_buttons: Vec<u16>,

    // --- DWT modifier state ---
    last_typing_time: Option<Instant>,

    // libinput device pointer (context owns the allocation)
    lib_device: *mut LibinputDevice,
}

unsafe impl Send for TrackedDevice {}

#[derive(Default)]
struct DebounceButton {
    delivered_down: bool,
    window_since: Option<Instant>,
    pending_down: Option<bool>,
    pending_since: Option<Instant>,
}

impl TrackedDevice {
    #[allow(clippy::too_many_arguments)]
    fn new(
        device: Device,
        restricted_fd: Option<RawFd>,
        path: PathBuf,
        lib_device: *mut LibinputDevice,
        uses_absolute_events: bool,
        is_absolute_pointer: bool,
        is_keyboard: bool,
        is_pointer: bool,
        is_topbuttonpad: bool,
        is_pointing_stick: bool,
    ) -> Self {
        let is_absolute = uses_absolute_events;
        let relative_axes = device.supported_relative_axes();
        let absolute_axes = device.supported_absolute_axes();
        let tablet_has_pressure =
            absolute_axes.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_PRESSURE));
        let tablet_has_distance =
            absolute_axes.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_DISTANCE));
        let tablet_has_tilt = absolute_axes.is_some_and(|axes| {
            axes.contains(AbsoluteAxisCode::ABS_TILT_X)
                && axes.contains(AbsoluteAxisCode::ABS_TILT_Y)
        });
        let tablet_has_rotation = absolute_axes.is_some_and(|axes| {
            axes.contains(AbsoluteAxisCode::ABS_Z)
                || axes.contains(AbsoluteAxisCode::ABS_MT_ORIENTATION)
        });
        let tablet_has_slider =
            absolute_axes.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_WHEEL));
        let tablet_has_wheel =
            relative_axes.is_some_and(|axes| axes.contains(RelativeAxisCode::REL_WHEEL));
        let tablet_has_size = absolute_axes.is_some_and(|axes| {
            axes.contains(AbsoluteAxisCode::ABS_MT_TOUCH_MAJOR)
                && axes.contains(AbsoluteAxisCode::ABS_MT_TOUCH_MINOR)
        });
        let tablet_buttons = device
            .supported_keys()
            .map(|keys| {
                [KeyCode::BTN_STYLUS.0, KeyCode::BTN_STYLUS2.0, 0x149, 0x100]
                    .into_iter()
                    .chain(0x110..=0x117)
                    .filter(|code| keys.contains(KeyCode(*code)))
                    .map(u32::from)
                    .collect()
            })
            .unwrap_or_default();
        let initial_tablet_tool_type = {
            use std::os::fd::AsRawFd;
            let mut key_bits = [0_u8; 96];
            let request =
                ((2_u64 << 30) | ((key_bits.len() as u64) << 16) | ((b'E' as u64) << 8) | 0x18)
                    as libc::c_ulong;
            if unsafe { libc::ioctl(device.as_raw_fd(), request, key_bits.as_mut_ptr()) } >= 0 {
                let is_down = |code: u16| {
                    key_bits
                        .get(usize::from(code / 8))
                        .is_some_and(|byte| byte & (1 << (code % 8)) != 0)
                };
                [
                    (KeyCode::BTN_TOOL_RUBBER.0, 2),
                    (KeyCode::BTN_TOOL_BRUSH.0, 3),
                    (KeyCode::BTN_TOOL_PENCIL.0, 4),
                    (KeyCode::BTN_TOOL_AIRBRUSH.0, 5),
                    (KeyCode::BTN_TOOL_MOUSE.0, 6),
                    (KeyCode::BTN_TOOL_LENS.0, 7),
                    (KeyCode::BTN_TOOL_PEN.0, 1),
                ]
                .into_iter()
                .find_map(|(code, tool_type)| is_down(code).then_some(tool_type))
            } else {
                None
            }
        };
        let supports_hi_res_vertical =
            relative_axes.is_some_and(|axes| axes.contains(RelativeAxisCode::REL_WHEEL_HI_RES));
        let supports_hi_res_horizontal =
            relative_axes.is_some_and(|axes| axes.contains(RelativeAxisCode::REL_HWHEEL_HI_RES));
        let mut abs_x_range = None;
        let mut abs_y_range = None;
        let mut current_abs_x = None;
        let mut current_abs_y = None;
        let mut tablet_pressure = 0.0;
        let mut tablet_pressure_range = None;
        let mut tablet_distance = 0.0;
        let mut tablet_distance_range = None;
        let mut tablet_tilt_x = 0.0;
        let mut tablet_tilt_y = 0.0;
        let mut tablet_tilt_x_info = None;
        let mut tablet_tilt_y_info = None;
        let mut tablet_rotation = 0.0;
        let mut tablet_rotation_info = None;
        let mut tablet_slider = 0.0;
        let mut tablet_slider_range = None;
        let mut has_mt = false;
        let mut has_mt_slot = false;
        let mut mt_slot_count = 10_usize;
        let mut mt_x_fuzz = 0;
        let mut mt_y_fuzz = 0;
        if let Ok(absinfo) = device.get_absinfo() {
            for (axis, info) in absinfo {
                if axis == AbsoluteAxisCode::ABS_X {
                    abs_x_range = Some((info.minimum(), info.maximum()));
                    current_abs_x = Some(info.value());
                } else if axis == AbsoluteAxisCode::ABS_Y {
                    abs_y_range = Some((info.minimum(), info.maximum()));
                    current_abs_y = Some(info.value());
                } else if axis == AbsoluteAxisCode::ABS_PRESSURE {
                    tablet_pressure = f64::from(info.value());
                    tablet_pressure_range = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_DISTANCE {
                    tablet_distance = f64::from(info.value());
                    tablet_distance_range = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_TILT_X {
                    tablet_tilt_x = f64::from(info.value());
                    tablet_tilt_x_info =
                        Some((info.minimum(), info.maximum(), info.resolution()));
                } else if axis == AbsoluteAxisCode::ABS_TILT_Y {
                    tablet_tilt_y = f64::from(info.value());
                    tablet_tilt_y_info =
                        Some((info.minimum(), info.maximum(), info.resolution()));
                } else if axis == AbsoluteAxisCode::ABS_Z
                    || axis == AbsoluteAxisCode::ABS_MT_ORIENTATION
                {
                    tablet_rotation = f64::from(info.value());
                    tablet_rotation_info = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_WHEEL {
                    tablet_slider = f64::from(info.value());
                    tablet_slider_range = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_MT_TRACKING_ID {
                    has_mt = true;
                } else if axis == AbsoluteAxisCode::ABS_MT_SLOT {
                    has_mt_slot = true;
                    mt_slot_count = (info.maximum() - info.minimum() + 1).clamp(1, 256) as usize;
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    mt_x_fuzz = info.fuzz();
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                    mt_y_fuzz = info.fuzz();
                }
            }
        }

        let mut mt_slots = vec![MtSlot::default(); mt_slot_count];
        mt_slots[0].x = current_abs_x.unwrap_or_default() as f64;
        mt_slots[0].y = current_abs_y.unwrap_or_default() as f64;
        if has_mt_slot {
            use std::os::fd::AsRawFd;
            let query_slots = |axis: AbsoluteAxisCode| -> Option<Vec<i32>> {
                let mut values = vec![0_i32; mt_slot_count + 1];
                values[0] = i32::from(axis.0);
                let size = values.len() * std::mem::size_of::<i32>();
                let request = ((2_u64 << 30) | ((size as u64) << 16) | ((b'E' as u64) << 8) | 0x0a)
                    as libc::c_ulong;
                (unsafe { libc::ioctl(device.as_raw_fd(), request, values.as_mut_ptr()) } >= 0)
                    .then(|| values.split_off(1))
            };
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_POSITION_X) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.x = value as f64;
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_POSITION_Y) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.y = value as f64;
                }
            }
        }

        Self {
            device,
            restricted_fd,
            path,
            is_absolute,
            is_absolute_pointer,
            has_mt,
            protocol_a: has_mt && !has_mt_slot,
            mt_x_fuzz,
            mt_y_fuzz,
            is_keyboard,
            is_pointer,
            is_topbuttonpad,
            is_pointing_stick,
            supports_hi_res_vertical,
            supports_hi_res_horizontal,
            warned_missing_hi_res_vertical: false,
            warned_missing_hi_res_horizontal: false,
            wheel_is_virtual: false,
            is_lenovo_scrollpoint: false,
            wheel_state: 0,
            wheel_min_movement: 47,
            wheel_accum_vertical: 0,
            wheel_accum_horizontal: 0,
            wheel_direction: 0,
            wheel_last_emit: None,
            scroll_button_down: false,
            scroll_lock_active: false,
            scroll_button_lock_press: false,
            scroll_button_press_time: None,
            scroll_button_moved: false,
            scroll_button_axes: 0,
            middle_left_down: false,
            middle_right_down: false,
            middle_chord_active: false,
            middle_pending_button: None,
            middle_pending_since: None,
            middle_suppressed_left: false,
            middle_suppressed_right: false,
            middle_real_down: false,
            left_handed_applied: false,
            debounce_buttons: HashMap::new(),
            debounce_spurious: false,
            debounce_bypass: false,
            scroll_button_accum_x: 0,
            scroll_button_accum_y: 0,
            pending_rel_x: 0,
            pending_rel_y: 0,
            pending_wheel_vertical: 0,
            pending_wheel_horizontal: 0,
            pending_wheel_hi_res_vertical: 0,
            pending_wheel_hi_res_horizontal: 0,
            current_abs_x,
            current_abs_y,
            absolute_changed: false,
            remainder_x: 0.0,
            remainder_y: 0.0,
            touch_active: false,
            touch_fingers: 0,
            last_x: None,
            last_y: None,
            current_dx: 0,
            current_dy: 0,
            abs_x_range,
            abs_y_range,
            axis_range_warning_at: None,
            tap_emitted: false,
            touch_start_time: None,
            last_movement_time: None,
            active_click_button: None,
            active_click_device: None,
            tablet_serial: 0,
            tablet_tool_id: 0,
            tablet_tool_type: initial_tablet_tool_type.unwrap_or(1),
            tablet_proximity_pending: initial_tablet_tool_type.map(|_| true),
            tablet_tool: std::ptr::null_mut(),
            tablet_has_pressure,
            tablet_has_distance,
            tablet_has_tilt,
            tablet_has_rotation,
            tablet_has_slider,
            tablet_has_wheel,
            tablet_has_size,
            tablet_buttons,
            tablet_x: current_abs_x.unwrap_or_default() as f64,
            tablet_y: current_abs_y.unwrap_or_default() as f64,
            tablet_x_changed: false,
            tablet_y_changed: false,
            tablet_axes_changed: false,
            tablet_pressure,
            tablet_pressure_range,
            tablet_pressure_changed: false,
            tablet_distance,
            tablet_distance_range,
            tablet_distance_changed: false,
            tablet_tilt_x,
            tablet_tilt_y,
            tablet_tilt_x_info,
            tablet_tilt_y_info,
            tablet_tilt_x_changed: false,
            tablet_tilt_y_changed: false,
            tablet_rotation,
            tablet_rotation_info,
            tablet_rotation_changed: false,
            tablet_slider,
            tablet_slider_range,
            tablet_slider_changed: false,
            tablet_wheel_delta: 0.0,
            tablet_wheel_discrete: 0,
            tablet_wheel_changed: false,
            tablet_tip_down: false,
            tablet_tip_pending: None,
            tablet_zero_pressure_since: None,
            tablet_proximity_timer_enabled: true,
            tablet_buttons_down: 0,
            tablet_held_buttons: Vec::new(),
            mt_slots,
            current_slot: 0,
            protocol_a_tracking_id: None,
            protocol_a_x: None,
            protocol_a_y: None,
            protocol_a_contacts: Vec::new(),
            pinch_active: false,
            pinch_base_dist: 0.0,
            pinch_base_angle: 0.0,
            pinch_fingers: 0,
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
            last_typing_time: None,
            lib_device,
        }
    }

    unsafe fn close(
        self,
        interface: *const crate::ffi_types::LibinputInterface,
        user_data: *mut libc::c_void,
    ) -> *mut LibinputDevice {
        let TrackedDevice {
            device,
            restricted_fd,
            lib_device,
            ..
        } = self;
        drop(device);
        if let Some(fd) = restricted_fd {
            if !interface.is_null() {
                if let Some(close_fn) = (*interface).close_restricted {
                    close_fn(fd, user_data);
                }
            }
        }
        lib_device
    }

    /// Count currently active MT slots.
    fn active_slot_count(&self) -> usize {
        self.mt_slots.iter().filter(|s| s.active).count()
    }

    /// Euclidean distance between the two primary active slots.
    fn primary_slot_distance(&self) -> Option<f64> {
        let active: Vec<&MtSlot> = self.mt_slots.iter().filter(|s| s.active).collect();
        if active.len() < 2 {
            return None;
        }
        let dx = active[0].x - active[1].x;
        let dy = active[0].y - active[1].y;
        Some((dx * dx + dy * dy).sqrt())
    }

    /// Angle (degrees) of the vector between the two primary active slots.
    fn primary_slot_angle(&self) -> f64 {
        let active: Vec<&MtSlot> = self.mt_slots.iter().filter(|s| s.active).collect();
        if active.len() < 2 {
            return 0.0;
        }
        let dx = active[1].x - active[0].x;
        let dy = active[1].y - active[0].y;
        dy.atan2(dx).to_degrees()
    }

    fn filter_hi_res_wheel(&mut self, axis: u32, value: i64) -> Option<i64> {
        if value == 0 || self.wheel_is_virtual {
            return (value != 0).then_some(value);
        }

        if self
            .wheel_last_emit
            .is_some_and(|last| last.elapsed() >= Duration::from_millis(500))
        {
            self.wheel_state = 0;
            self.wheel_accum_vertical = 0;
            self.wheel_accum_horizontal = 0;
            self.wheel_last_emit = None;
        }

        self.wheel_min_movement = self.wheel_min_movement.min(value.abs());
        let accumulate = self.wheel_min_movement < 30;
        let direction = match (axis, value.is_positive()) {
            (0, true) => 1,
            (0, false) => -1,
            (1, true) => 2,
            (1, false) => -2,
            _ => 0,
        };
        if direction != self.wheel_direction {
            self.wheel_direction = direction;
            self.wheel_state = 0;
            self.wheel_accum_vertical = 0;
            self.wheel_accum_horizontal = 0;
        }

        if !accumulate {
            self.wheel_state = 2;
            self.wheel_last_emit = Some(Instant::now());
            return Some(value);
        }

        if self.wheel_state == 2 {
            self.wheel_last_emit = Some(Instant::now());
            return Some(value);
        }

        self.wheel_state = 1;
        let accumulator = if axis == 0 {
            &mut self.wheel_accum_vertical
        } else {
            &mut self.wheel_accum_horizontal
        };
        *accumulator += value;
        if accumulator.abs() > self.wheel_min_movement {
            let accumulated = *accumulator;
            *accumulator = 0;
            self.wheel_state = 2;
            self.wheel_last_emit = Some(Instant::now());
            Some(accumulated)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: convert a SystemTime to microseconds since UNIX epoch
// ---------------------------------------------------------------------------

fn systime_to_usec(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_micros() as u64
}

unsafe fn press_seat_button(device: *mut LibinputDevice) -> u32 {
    (*(*device).seat)
        .button_count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1
}

unsafe fn release_seat_button(device: *mut LibinputDevice) -> u32 {
    let count = &(*(*device).seat).button_count;
    let previous = count
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |value| Some(value.saturating_sub(1)),
        )
        .unwrap_or(0);
    previous.saturating_sub(1)
}

unsafe fn allocate_touch_seat_slot(ctx: *mut LibinputContext) -> i32 {
    let slots = &mut (*ctx).touch_seat_slots;
    if let Some(index) = slots.iter().position(|used| !*used) {
        slots[index] = true;
        index as i32
    } else {
        slots.push(true);
        (slots.len() - 1) as i32
    }
}

unsafe fn release_touch_seat_slot(ctx: *mut LibinputContext, seat_slot: i32) {
    let slots = &mut (*ctx).touch_seat_slots;
    if let Some(used) = slots.get_mut(seat_slot as usize) {
        *used = false;
    }
}

unsafe fn tablet_tool_for(
    ctx: *mut LibinputContext,
    device: *mut LibinputDevice,
    serial: u64,
    tool_id: u64,
    tool_type: u32,
    capabilities: [bool; 7],
    buttons: &[u32],
) -> *mut LibinputTabletTool {
    if let Some(tool) = (*ctx).tablet_tools.iter().copied().find(|tool| {
        !tool.is_null()
            && (**tool).tool_type == tool_type
            && if serial == 0 {
                (**tool).serial == 0 && (**tool).device == device
            } else {
                (**tool).serial == serial
            }
    }) {
        (*tool).device = device;
        return tool;
    }
    let name = i32::try_from(tool_id).ok().and_then(|tool_id| {
        let database = libwacom_database_new();
        if database.is_null() {
            return None;
        }
        let stylus = libwacom_stylus_get_for_id(database, tool_id);
        let name = if stylus.is_null() || libwacom_stylus_is_generic(stylus) != 0 {
            None
        } else {
            let value = libwacom_stylus_get_name(stylus);
            (!value.is_null()).then(|| std::ffi::CStr::from_ptr(value).to_owned())
        };
        libwacom_database_destroy(database);
        name
    });
    let tool = Box::into_raw(Box::new(LibinputTabletTool {
        refcount: std::sync::atomic::AtomicI32::new(1),
        user_data: std::ptr::null_mut(),
        serial,
        tool_id,
        name,
        tool_type,
        device,
        has_pressure: capabilities[0],
        has_distance: capabilities[1],
        has_tilt: capabilities[2] && !matches!(tool_type, 6 | 7),
        has_rotation: capabilities[3] || (capabilities[2] && matches!(tool_type, 6 | 7)),
        has_slider: capabilities[4],
        has_wheel: capabilities[5],
        has_size: capabilities[6],
        buttons: buttons
            .iter()
            .copied()
            .filter(|button| match tool_type {
                6 | 7 => (0x110..=0x117).contains(button),
                8 => *button == 0x100,
                _ => matches!(*button, 0x149 | 0x14b | 0x14c),
            })
            .collect(),
    }));
    (*ctx).tablet_tools.push(tool);
    tool
}

unsafe fn tablet_tool_payload(
    td: &TrackedDevice,
    lib_dev: *mut LibinputDevice,
    time_usec: u64,
    tool: *mut LibinputTabletTool,
    proximity_state: u32,
) -> TabletToolEvent {
    let (x_min, x_max) = (*lib_dev).abs_x_range.unwrap_or((0, 0));
    let (y_min, y_max) = (*lib_dev).abs_y_range.unwrap_or((0, 0));
    let (pressure_min, pressure_max) = td.tablet_pressure_range.unwrap_or((0, 0));
    let pressure_lower = f64::from(pressure_min)
        + (f64::from(pressure_max - pressure_min) * 0.01).trunc();
    let pressure_is_active = td.tablet_pressure > pressure_lower;
    let distance = td.tablet_distance_range.map_or(0.0, |(minimum, maximum)| {
        let range = f64::from(maximum - minimum);
        if pressure_is_active || range <= 0.0 {
            0.0
        } else {
            ((td.tablet_distance - f64::from(minimum)) / range).clamp(0.0, 1.0)
        }
    });
    let x_range = f64::from(x_max - x_min);
    let y_range = f64::from(y_max - y_min);
    let normalized_x = if x_range > 0.0 {
        (td.tablet_x - f64::from(x_min)) / x_range
    } else {
        0.0
    };
    let normalized_y = if y_range > 0.0 {
        (td.tablet_y - f64::from(y_min)) / y_range
    } else {
        0.0
    };
    let matrix = (*lib_dev).calibration.map(f64::from);
    let transformed_x = matrix[0] * normalized_x + matrix[1] * normalized_y + matrix[2];
    let transformed_y = matrix[3] * normalized_x + matrix[4] * normalized_y + matrix[5];
    let normalize_tilt = |value: f64, info: Option<(i32, i32, i32)>| {
        let Some((minimum, maximum, resolution)) = info else {
            return 0.0;
        };
        if resolution != 0 && minimum < 0 && maximum > 0 {
            (value / f64::from(resolution)).to_degrees()
        } else {
            let adjusted_maximum = if (maximum - minimum + 1) % 2 == 0 {
                maximum.saturating_add(1)
            } else {
                maximum
            };
            let range = f64::from(adjusted_maximum - minimum);
            if range > 0.0 {
                ((value - f64::from(minimum)) / range * 2.0 - 1.0) * 64.0
            } else {
                0.0
            }
        }
    };
    let tilt_x = normalize_tilt(td.tablet_tilt_x, td.tablet_tilt_x_info);
    let tilt_y = normalize_tilt(td.tablet_tilt_y, td.tablet_tilt_y_info);
    let tool_type = (*tool).tool_type;
    let rotation = if matches!(tool_type, 6 | 7) {
        ((-tilt_x).atan2(tilt_y).to_degrees() - 5.0).rem_euclid(360.0)
    } else if tool_type == 8 {
        (360.0 - td.tablet_rotation).rem_euclid(360.0)
    } else if let Some((minimum, maximum)) = td.tablet_rotation_info {
        let range = f64::from(maximum - minimum + 1);
        if range > 0.0 {
            (((td.tablet_rotation - f64::from(minimum)) / range) * 360.0 + 90.0)
                .rem_euclid(360.0)
        } else {
            0.0
        }
    } else {
        0.0
    };
    let slider = td.tablet_slider_range.map_or(0.0, |(minimum, maximum)| {
        let range = f64::from(maximum - minimum);
        if range > 0.0 {
            ((td.tablet_slider - f64::from(minimum)) / range) * 2.0 - 1.0
        } else {
            0.0
        }
    });
    TabletToolEvent {
        time_usec,
        tool,
        proximity_state,
        x: transformed_x * x_range + f64::from(x_min),
        y: transformed_y * y_range + f64::from(y_min),
        x_min: f64::from(x_min),
        x_max: f64::from(x_max),
        y_min: f64::from(y_min),
        y_max: f64::from(y_max),
        x_resolution: f64::from((*lib_dev).abs_x_resolution.unwrap_or(0)),
        y_resolution: f64::from((*lib_dev).abs_y_resolution.unwrap_or(0)),
        x_changed: (matrix[0] != 0.0 && td.tablet_x_changed)
            || (matrix[1] != 0.0 && td.tablet_y_changed),
        y_changed: (matrix[3] != 0.0 && td.tablet_x_changed)
            || (matrix[4] != 0.0 && td.tablet_y_changed),
        pressure: td.tablet_pressure,
        pressure_min: pressure_lower,
        pressure_max: f64::from(pressure_max),
        pressure_changed: td.tablet_pressure_changed && pressure_is_active,
        distance,
        distance_changed: td.tablet_distance_changed && !pressure_is_active,
        tilt_x,
        tilt_y,
        tilt_x_changed: td.tablet_tilt_x_changed,
        tilt_y_changed: td.tablet_tilt_y_changed,
        rotation,
        rotation_changed: if matches!(tool_type, 6 | 7) {
            td.tablet_tilt_x_changed || td.tablet_tilt_y_changed
        } else {
            td.tablet_rotation_changed
        },
        slider,
        slider_changed: td.tablet_slider_changed,
        wheel_delta: td.tablet_wheel_delta,
        wheel_discrete: td.tablet_wheel_discrete,
        wheel_changed: td.tablet_wheel_changed,
        tip_state: u32::from(td.tablet_tip_down),
        button: 0,
        button_state: 0,
        seat_button_count: 0,
    }
}

// ---------------------------------------------------------------------------
// BackendState
// ---------------------------------------------------------------------------

pub struct BackendState {
    devices: HashMap<RawFd, TrackedDevice>,
    requested_paths: Vec<PathBuf>,
    inotify: Option<Inotify>,
    pub global_typing_time: Option<Instant>,
    suspended: bool,
}

unsafe impl Send for BackendState {}

impl BackendState {
    unsafe fn release_context_device(ctx: *mut LibinputContext, device: *mut LibinputDevice) {
        (*ctx).devices.retain(|candidate| *candidate != device);
        if (*device)
            .refcount
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            == 1
        {
            drop(Box::from_raw(device));
        }
    }

    pub fn new() -> Self {
        let inotify = Inotify::init(InitFlags::IN_NONBLOCK).ok().and_then(|ino| {
            ino.add_watch(
                "/dev/input",
                AddWatchFlags::IN_CREATE
                    | AddWatchFlags::IN_ATTRIB
                    | AddWatchFlags::IN_DELETE
                    | AddWatchFlags::IN_MOVED_FROM
                    | AddWatchFlags::IN_MOVED_TO,
            )
            .ok()?;
            Some(ino)
        });
        Self {
            devices: HashMap::new(),
            requested_paths: Vec::new(),
            inotify,
            global_typing_time: None,
            suspended: false,
        }
    }

    pub fn inotify_fd(&self) -> Option<RawFd> {
        use std::os::fd::{AsFd, AsRawFd};
        self.inotify.as_ref().map(|i| i.as_fd().as_raw_fd())
    }

    // -----------------------------------------------------------------------
    // Device discovery
    // -----------------------------------------------------------------------

    pub unsafe fn scan_and_open(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut Vec<LibinputEvent>,
    ) {
        if self.suspended {
            return;
        }
        for (path, _) in evdev::enumerate() {
            self.try_open(ctx, &path, out);
        }
    }

    pub unsafe fn try_open(
        &mut self,
        ctx: *mut LibinputContext,
        path: &std::path::Path,
        out: &mut Vec<LibinputEvent>,
    ) {
        if (*ctx).backend_kind == BackendKind::Udev
            && self.devices.values().any(|tracked| tracked.path == path)
        {
            return;
        }
        let interface = &*(*ctx).interface;
        let (device, restricted_guard) = if let Some(open_fn) = interface.open_restricted {
            let c_path = match std::ffi::CString::new(path.to_str().unwrap_or("")) {
                Ok(c) => c,
                Err(_) => return,
            };
            let raw_fd = open_fn(
                c_path.as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK,
                (*ctx).user_data,
            );
            if raw_fd < 0 {
                return;
            }
            let guard =
                RestrictedFdGuard::new(raw_fd, interface.close_restricted, (*ctx).user_data);
            let device_fd = libc::fcntl(raw_fd, libc::F_DUPFD_CLOEXEC, 0);
            if device_fd < 0 {
                return;
            }
            let owned_fd: std::os::fd::OwnedFd =
                unsafe { std::os::fd::FromRawFd::from_raw_fd(device_fd) };
            match Device::from_fd(owned_fd) {
                Ok(d) => (d, Some(guard)),
                Err(_) => return,
            }
        } else {
            let Ok(d) = Device::open(path) else {
                return;
            };
            (d, None)
        };

        let name = device.name().unwrap_or("Unknown").to_string();

        // Skip our own virtual device
        if name.contains("virtual pointer") || name.contains("libinput-rs") {
            return;
        }

        let mut has_abs_x = false;
        let mut has_abs_y = false;
        let mut has_mt_x = false;
        let mut has_mt_y = false;
        let mut has_mt_slot = false;
        let mut invalid_coordinate_range = false;
        let mut invalid_other_range = false;
        let mut invalid_negative_resolution = false;
        let mut abs_x_resolution = None;
        let mut abs_y_resolution = None;
        let mut abs_x_range_raw = None;
        let mut abs_y_range_raw = None;
        let mut mt_x_range_raw = None;
        let mut mt_y_range_raw = None;
        let mut mt_x_resolution = None;
        let mut mt_y_resolution = None;
        if let Ok(absinfo) = device.get_absinfo() {
            for (axis, info) in absinfo {
                has_abs_x |= axis == AbsoluteAxisCode::ABS_X;
                has_abs_y |= axis == AbsoluteAxisCode::ABS_Y;
                has_mt_x |= axis == AbsoluteAxisCode::ABS_MT_POSITION_X;
                has_mt_y |= axis == AbsoluteAxisCode::ABS_MT_POSITION_Y;
                has_mt_slot |= axis == AbsoluteAxisCode::ABS_MT_SLOT;

                if axis == AbsoluteAxisCode::ABS_X {
                    abs_x_resolution = Some(info.resolution());
                    abs_x_range_raw = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_Y {
                    abs_y_resolution = Some(info.resolution());
                    abs_y_range_raw = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    mt_x_resolution = Some(info.resolution());
                    mt_x_range_raw = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                    mt_y_resolution = Some(info.resolution());
                    mt_y_range_raw = Some((info.minimum(), info.maximum()));
                }
                invalid_negative_resolution |= info.resolution() < 0;

                let range = i64::from(info.maximum()) - i64::from(info.minimum());
                let range_check_is_skipped = axis == AbsoluteAxisCode::ABS_MISC
                    || axis == AbsoluteAxisCode::ABS_MT_SLOT
                    || axis == AbsoluteAxisCode::ABS_MT_TOOL_TYPE;
                let zero_range_vendor_axis = info.minimum() == 0
                    && info.maximum() == 0
                    && axis.0 >= AbsoluteAxisCode::ABS_MISC.0
                    && axis.0 < AbsoluteAxisCode::ABS_MT_SLOT.0;
                let is_abs_coordinate =
                    axis == AbsoluteAxisCode::ABS_X || axis == AbsoluteAxisCode::ABS_Y;
                let is_mt_coordinate = axis == AbsoluteAxisCode::ABS_MT_POSITION_X
                    || axis == AbsoluteAxisCode::ABS_MT_POSITION_Y;
                if !range_check_is_skipped
                    && !zero_range_vendor_axis
                    && (range <= 0 || range > i64::from(i32::MAX) / 2)
                {
                    if is_abs_coordinate || is_mt_coordinate {
                        invalid_coordinate_range = true;
                    } else {
                        invalid_other_range = true;
                    }
                }
            }
        }
        let abs_resolution_mismatch = matches!(
            (abs_x_resolution, abs_y_resolution),
            (Some(x), Some(y)) if (x == 0) != (y == 0)
        );
        let mt_resolution_mismatch = matches!(
            (mt_x_resolution, mt_y_resolution),
            (Some(x), Some(y)) if (x == 0) != (y == 0)
        );
        if has_abs_x != has_abs_y
            || ((has_abs_x && has_abs_y)
                && (has_mt_slot || has_mt_x || has_mt_y)
                && has_mt_x != has_mt_y)
            || invalid_coordinate_range
            || invalid_other_range
            || invalid_negative_resolution
            || abs_resolution_mismatch
            || mt_resolution_mismatch
        {
            return;
        }
        if abs_x_range_raw.is_none() {
            abs_x_range_raw = mt_x_range_raw;
            abs_x_resolution = mt_x_resolution;
        }
        if abs_y_range_raw.is_none() {
            abs_y_range_raw = mt_y_range_raw;
            abs_y_resolution = mt_y_resolution;
        }

        let props = device.properties();
        let is_topbuttonpad = props.contains(evdev::PropType::TOPBUTTONPAD);
        let keys = device.supported_keys();
        let rel = device.supported_relative_axes();
        let abs = device.supported_absolute_axes();
        let udev_device = crate::udev::UdevDevice::from_path(path);
        let udev_pointer = udev_device.as_ptr();
        if crate::udev::property_value(udev_pointer, "LIBINPUT_IGNORE_DEVICE")
            .is_some_and(|value| value != "0")
        {
            return;
        }
        if (*ctx).backend_kind == crate::ffi_types::BackendKind::Udev {
            let assigned_seat = (*(*ctx).seat).physical_name.to_string_lossy();
            let device_seat = crate::udev::property_value(udev_pointer, "ID_SEAT")
                .unwrap_or_else(|| "seat0".to_string());
            if device_seat != assigned_seat {
                return;
            }
        }
        let has_tag = |tag| crate::udev::property_equals(udev_pointer, tag, "1");
        let tag_keyboard = has_tag("ID_INPUT_KEYBOARD");
        let tag_touchpad = has_tag("ID_INPUT_TOUCHPAD");
        let tag_touchscreen = has_tag("ID_INPUT_TOUCHSCREEN");
        let tag_tablet = has_tag("ID_INPUT_TABLET");
        let tag_tablet_pad = has_tag("ID_INPUT_TABLET_PAD");
        let tag_switch = has_tag("ID_INPUT_SWITCH");
        let tag_mouse = has_tag("ID_INPUT_MOUSE");
        let tag_pointing_stick = has_tag("ID_INPUT_POINTINGSTICK");
        let has_class_tag = tag_keyboard
            || tag_touchpad
            || tag_touchscreen
            || tag_tablet
            || tag_tablet_pad
            || tag_switch
            || tag_mouse
            || tag_pointing_stick;
        let use_evdev_fallback = !has_class_tag;
        let is_keyboard = tag_keyboard
            || (use_evdev_fallback && keys.is_some_and(|k| k.iter().any(|key| key.0 < 0x100)));
        let has_pen = keys.is_some_and(|k| {
            k.contains(KeyCode::BTN_TOOL_PEN)
                || k.contains(KeyCode::BTN_TOOL_RUBBER)
                || k.contains(KeyCode::BTN_TOOL_BRUSH)
                || k.contains(KeyCode::BTN_TOOL_PENCIL)
                || k.contains(KeyCode::BTN_TOOL_AIRBRUSH)
        });
        let has_finger = keys.is_some_and(|k| k.contains(KeyCode::BTN_TOOL_FINGER));
        let has_touch_button = keys.is_some_and(|k| k.contains(KeyCode::BTN_TOUCH));
        let has_abs_xy = abs.is_some_and(|a| {
            a.contains(AbsoluteAxisCode::ABS_X) && a.contains(AbsoluteAxisCode::ABS_Y)
        });
        let has_mt_xy = abs.is_some_and(|a| {
            a.contains(AbsoluteAxisCode::ABS_MT_POSITION_X)
                && a.contains(AbsoluteAxisCode::ABS_MT_POSITION_Y)
        });
        let is_touchpad = tag_touchpad
            || (use_evdev_fallback
                && !has_pen
                && (props.contains(evdev::PropType::POINTER)
                    || props.contains(evdev::PropType::BUTTONPAD)
                    || has_finger)
                && (has_abs_xy || has_mt_xy));
        let mut has_tablet = tag_tablet || (use_evdev_fallback && has_pen);
        let mut has_touch = tag_touchscreen
            || (use_evdev_fallback
                && !has_pen
                && !is_touchpad
                && has_touch_button
                && (has_abs_xy || has_mt_xy));
        let input_id = device.input_id();
        let has_tablet_pad = tag_tablet_pad
            || (use_evdev_fallback
                && !has_pen
                && !has_finger
                && !has_touch_button
                && device.supported_events().contains(EventType::ABSOLUTE)
                && keys.is_some_and(|k| {
                    k.contains(KeyCode::BTN_0)
                        || (input_id.vendor() == 0x056a && k.iter().any(|key| key.0 >= 0x100))
                }));
        let has_supported_switch = device
            .supported_switches()
            .is_some_and(|switches| switches.iter().any(|switch| matches!(switch.0, 0 | 1 | 10)));
        let has_switch = has_supported_switch && (tag_switch || use_evdev_fallback);
        let has_relative_pointer = rel.is_some_and(|r| {
            r.contains(RelativeAxisCode::REL_X)
                || r.contains(RelativeAxisCode::REL_Y)
                || r.contains(RelativeAxisCode::REL_WHEEL)
                || r.contains(RelativeAxisCode::REL_HWHEEL)
                || r.contains(RelativeAxisCode::REL_WHEEL_HI_RES)
                || r.contains(RelativeAxisCode::REL_HWHEEL_HI_RES)
        });
        let has_relative_motion = rel.is_some_and(|r| {
            r.contains(RelativeAxisCode::REL_X) || r.contains(RelativeAxisCode::REL_Y)
        });
        let has_wheel = rel.is_some_and(|r| {
            r.contains(RelativeAxisCode::REL_WHEEL)
                || r.contains(RelativeAxisCode::REL_HWHEEL)
                || r.contains(RelativeAxisCode::REL_WHEEL_HI_RES)
                || r.contains(RelativeAxisCode::REL_HWHEEL_HI_RES)
        });
        let is_pointer = is_touchpad || tag_mouse || tag_pointing_stick || has_relative_pointer;
        if !is_pointer
            && !is_keyboard
            && !has_touch
            && !has_tablet
            && !has_tablet_pad
            && !has_switch
        {
            return;
        }

        let lib_dev = Box::into_raw(Box::new(LibinputDevice::new(
            &name,
            path.to_str().unwrap_or(""),
            (*ctx).seat,
            ctx,
        )));
        (*lib_dev).has_pointer = is_pointer;
        (*lib_dev).has_keyboard = is_keyboard;
        (*lib_dev).has_touch = has_touch;
        (*lib_dev).has_gesture = is_touchpad;
        (*lib_dev).has_switch = has_switch;
        (*lib_dev).has_tablet = has_tablet;
        (*lib_dev).has_tablet_pad = has_tablet_pad;
        (*lib_dev).abs_x_range = abs_x_range_raw;
        (*lib_dev).abs_y_range = abs_y_range_raw;
        (*lib_dev).abs_x_resolution = abs_x_resolution.filter(|resolution| *resolution > 0);
        (*lib_dev).abs_y_resolution = abs_y_resolution.filter(|resolution| *resolution > 0);
        (*lib_dev).calibration_available =
            has_abs_xy && !is_touchpad && (!has_tablet || props.contains(evdev::PropType::DIRECT));
        (*lib_dev).accel_available = is_touchpad || has_relative_motion;
        (*lib_dev).supports_button_scroll = is_pointer
            && !has_tablet
            && !has_tablet_pad
            && has_relative_motion
            && keys.is_some_and(|codes| {
                codes.iter().any(|code| {
                    (KeyCode::BTN_0.0..=KeyCode::BTN_9.0).contains(&code.0)
                        || (KeyCode::BTN_LEFT.0..=KeyCode::BTN_TASK.0).contains(&code.0)
                })
            });
        let mut event_codes: Vec<u16> = keys
            .map(|codes| codes.iter().map(|code| code.0).collect())
            .unwrap_or_default();
        let udev_type = if tag_touchpad {
            "touchpad"
        } else if tag_touchscreen {
            "touchscreen"
        } else if tag_tablet {
            "tablet"
        } else if tag_tablet_pad {
            "tablet-pad"
        } else if tag_pointing_stick {
            "pointingstick"
        } else if tag_mouse || has_relative_pointer {
            "mouse"
        } else if tag_keyboard {
            "keyboard"
        } else if tag_switch {
            "switch"
        } else {
            "unknown"
        };
        let applied_quirks = crate::quirks::apply_quirks(
            &name,
            input_id.bus_type().0,
            input_id.vendor(),
            input_id.product(),
            input_id.version(),
            udev_type,
            &mut event_codes,
        );
        for message in &applied_quirks.messages {
            crate::emit_debug_log(ctx, message);
        }
        if applied_quirks.model_dell_canvas_totem {
            has_touch = false;
            has_tablet = true;
            (*lib_dev).has_touch = false;
            (*lib_dev).has_tablet = true;
        }
        (*lib_dev).event_codes = event_codes;
        let has_left_button = (*lib_dev).event_codes.contains(&KeyCode::BTN_LEFT.0);
        let has_right_button = (*lib_dev).event_codes.contains(&KeyCode::BTN_RIGHT.0);
        let has_middle_button = (*lib_dev).event_codes.contains(&KeyCode::BTN_MIDDLE.0);
        let is_clickpad = props.contains(evdev::PropType::BUTTONPAD);
        let (middle_emulation_available, middle_emulation_default) = if is_touchpad {
            if is_clickpad {
                (true, false)
            } else if has_left_button && has_right_button {
                if has_middle_button && applied_quirks.model_alps_serial_touchpad {
                    (true, true)
                } else {
                    (false, !has_middle_button)
                }
            } else {
                (false, false)
            }
        } else if is_pointer
            && !has_tablet
            && !has_tablet_pad
            && has_left_button
            && has_right_button
        {
            (has_middle_button, !has_middle_button)
        } else {
            (false, false)
        };
        (*lib_dev).middle_emulation_available = middle_emulation_available;
        (*lib_dev).middle_emulation = middle_emulation_default;
        (*lib_dev).middle_emulation_default = middle_emulation_default;
        let default_scroll_button = if !(*lib_dev).supports_button_scroll {
            0
        } else if has_middle_button {
            u32::from(KeyCode::BTN_MIDDLE.0)
        } else if (*lib_dev).event_codes.contains(&KeyCode::BTN_SIDE.0) {
            u32::from(KeyCode::BTN_SIDE.0)
        } else {
            0
        };
        (*lib_dev).scroll_button = default_scroll_button;
        (*lib_dev).scroll_default_button = default_scroll_button;
        if !is_touchpad {
            let default_scroll_method = if (*lib_dev).supports_button_scroll
                && !has_wheel
                && has_middle_button
                && !applied_quirks.model_lenovo_scrollpoint
            {
                4
            } else {
                0
            };
            (*lib_dev).scroll_method = default_scroll_method;
            (*lib_dev).scroll_default_method = default_scroll_method;
        }
        (*lib_dev).vendor_id = input_id.vendor() as u32;
        (*lib_dev).product_id = input_id.product() as u32;
        (*lib_dev).bus_type = input_id.bus_type().0 as u32;
        (*lib_dev).udev_device = udev_device.into_raw();
        let raw_udev_device = (*lib_dev).udev_device;
        if let Some(value) =
            crate::udev::property_value(raw_udev_device, "LIBINPUT_CALIBRATION_MATRIX")
        {
            let values: Vec<f32> = value
                .split_ascii_whitespace()
                .filter_map(|component| component.parse::<f32>().ok())
                .collect();
            if values.len() == 6 && values.iter().all(|component| component.is_finite()) {
                let mut matrix = [0.0_f32; 6];
                matrix.copy_from_slice(&values);
                (*lib_dev).calibration = matrix;
                (*lib_dev).default_calibration = matrix;
            }
        }
        let wheel_angle = |angle_property: &str, count_property: &str| {
            crate::udev::property_value(raw_udev_device, count_property)
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value != 0.0)
                .map(|count| 360.0 / count)
                .or_else(|| {
                    crate::udev::property_value(raw_udev_device, angle_property)
                        .and_then(|value| value.parse::<f64>().ok())
                        .filter(|value| value.is_finite() && *value != 0.0)
                })
                .unwrap_or(15.0)
        };
        (*lib_dev).wheel_click_angle_vertical =
            wheel_angle("MOUSE_WHEEL_CLICK_ANGLE", "MOUSE_WHEEL_CLICK_COUNT");
        (*lib_dev).wheel_click_angle_horizontal = wheel_angle(
            "MOUSE_WHEEL_CLICK_ANGLE_HORIZONTAL",
            "MOUSE_WHEEL_CLICK_COUNT_HORIZONTAL",
        );
        (*lib_dev).output_name = crate::udev::property_value((*lib_dev).udev_device, "WL_OUTPUT")
            .and_then(|name| std::ffi::CString::new(name).ok());
        (*lib_dev).sysname =
            std::ffi::CString::new(path.file_name().and_then(|s| s.to_str()).unwrap_or(""))
                .unwrap_or_else(|_| std::ffi::CString::new("").unwrap());
        let external_touchpad = crate::udev::property_equals(
            (*lib_dev).udev_device,
            "ID_INPUT_TOUCHPAD_INTEGRATION",
            "external",
        );
        (*lib_dev).send_events_modes =
            if is_touchpad && !external_touchpad && input_id.vendor() != 0x056a {
                0b11
            } else {
                0b01
            };

        if let Ok(absinfo) = device.get_absinfo() {
            let mut x = None;
            let mut y = None;
            let mut x_range = None;
            let mut y_range = None;
            let mut size_axes_are_sentinel = false;
            let mut mt_x_info = None;
            let mut mt_y_info = None;
            let mut slots = None;
            for (axis, info) in absinfo {
                if axis == AbsoluteAxisCode::ABS_X {
                    x_range = Some((info.maximum() - info.minimum()).unsigned_abs() as f64);
                    size_axes_are_sentinel |= info.minimum() == 0 && info.maximum() == 1;
                    if info.resolution() > 0 {
                        x = Some(x_range.unwrap_or_default() / info.resolution() as f64);
                    }
                } else if axis == AbsoluteAxisCode::ABS_Y {
                    y_range = Some((info.maximum() - info.minimum()).unsigned_abs() as f64);
                    size_axes_are_sentinel |= info.minimum() == 0 && info.maximum() == 1;
                    if info.resolution() > 0 {
                        y = Some(y_range.unwrap_or_default() / info.resolution() as f64);
                    }
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    mt_x_info = Some((
                        (info.maximum() - info.minimum()).unsigned_abs() as f64,
                        info.resolution(),
                        info.minimum() == 0 && info.maximum() == 1,
                    ));
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                    mt_y_info = Some((
                        (info.maximum() - info.minimum()).unsigned_abs() as f64,
                        info.resolution(),
                        info.minimum() == 0 && info.maximum() == 1,
                    ));
                } else if axis == AbsoluteAxisCode::ABS_MT_SLOT {
                    slots = Some(info.maximum() - info.minimum() + 1);
                }
            }
            if (is_touchpad || has_touch || has_tablet) && (x_range.is_none() || y_range.is_none())
            {
                if let (Some((xr, xres, xsentinel)), Some((yr, yres, ysentinel))) =
                    (mt_x_info, mt_y_info)
                {
                    x_range = Some(xr);
                    y_range = Some(yr);
                    size_axes_are_sentinel = xsentinel || ysentinel;
                    x = (xres > 0).then_some(xr / xres as f64);
                    y = (yres > 0).then_some(yr / yres as f64);
                }
            }
            if size_axes_are_sentinel || x_range.is_none() || y_range.is_none() {
                x = None;
                y = None;
            } else if let Some((width, height)) = applied_quirks.size_hint {
                x = Some(width);
                y = Some(height);
            } else if let (Some((x_resolution, y_resolution)), Some(xr), Some(yr)) =
                (applied_quirks.resolution_hint, x_range, y_range)
            {
                x = Some(xr / x_resolution);
                y = Some(yr / y_resolution);
            } else if is_touchpad && x.is_none() && y.is_none() {
                // Match upstream's conservative fallback for old touchpads
                // whose kernels provide neither resolution nor a size hint.
                x = Some(69.0);
                y = Some(50.0);
            }
            (*lib_dev).width_mm = x;
            (*lib_dev).height_mm = y;
            (*lib_dev).touch_count = if has_touch || is_touchpad {
                if has_touch && has_mt_xy && !has_mt_slot {
                    10
                } else {
                    slots.unwrap_or(1)
                }
            } else {
                0
            };
            if is_touchpad {
                let default_scroll_method = if (*lib_dev).touch_count >= 2 { 1 } else { 2 };
                (*lib_dev).scroll_method = default_scroll_method;
                (*lib_dev).scroll_default_method = default_scroll_method;
            }
        }
        (*ctx).devices.push(lib_dev);

        let fd = {
            use std::os::unix::io::AsRawFd;
            device.as_raw_fd()
        };
        if self.devices.contains_key(&fd) {
            return;
        }

        (*ctx).register_fd(fd);

        let restricted_fd = restricted_guard.map(RestrictedFdGuard::disarm);
        let mut td = TrackedDevice::new(
            device,
            restricted_fd,
            path.to_path_buf(),
            lib_dev,
            is_touchpad || has_touch || has_tablet || (is_pointer && has_abs_xy),
            is_pointer && has_abs_xy && !is_touchpad,
            is_keyboard,
            is_pointer,
            is_topbuttonpad,
            tag_pointing_stick,
        );
        let udev_fuzz = |primary: &str, fallback: &str| {
            crate::udev::property_value(raw_udev_device, primary)
                .or_else(|| crate::udev::property_value(raw_udev_device, fallback))
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|value| *value >= 0)
        };
        if let Some(fuzz) = udev_fuzz("LIBINPUT_FUZZ_35", "LIBINPUT_FUZZ_00") {
            td.mt_x_fuzz = fuzz;
        }
        if let Some(fuzz) = udev_fuzz("LIBINPUT_FUZZ_36", "LIBINPUT_FUZZ_01") {
            td.mt_y_fuzz = fuzz;
        }
        if applied_quirks.disable_hi_res_wheel_vertical {
            td.supports_hi_res_vertical = false;
        }
        if applied_quirks.disable_hi_res_wheel_horizontal {
            td.supports_hi_res_horizontal = false;
        }
        if applied_quirks.disable_tablet_tilt_x || applied_quirks.disable_tablet_tilt_y {
            td.tablet_has_tilt = false;
        }
        td.wheel_is_virtual = applied_quirks.is_virtual;
        td.is_lenovo_scrollpoint = applied_quirks.model_lenovo_scrollpoint;
        self.devices.insert(fd, td);

        out.push(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_ADDED,
            payload: EventPayload::DeviceAdded,
            context: ctx,
            device: lib_dev,
        });
    }

    pub unsafe fn release_active_inputs(
        &mut self,
        ctx: *mut LibinputContext,
        device: *mut LibinputDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let Some(tracked) = self
            .devices
            .values_mut()
            .find(|tracked| tracked.lib_device == device)
        else {
            return;
        };
        let time_usec = systime_to_usec(SystemTime::now());
        for key in tracked.held_keys.drain(..) {
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_KEYBOARD_KEY,
                payload: EventPayload::KeyboardKey(KeyboardKeyEvent {
                    time_usec,
                    key: key.code as u32,
                    state: 0,
                }),
                context: ctx,
                device,
            });
        }
        for button in tracked.held_buttons.drain(..) {
            let seat_button_count = release_seat_button(device);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec,
                    button: button as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device,
            });
        }
        if let Some(button) = tracked.active_click_button.take() {
            let event_device = tracked.active_click_device.take().unwrap_or(device);
            let seat_button_count = release_seat_button(event_device);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec,
                    button: button as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device: event_device,
            });
        }
        if !tracked.tablet_tool.is_null() {
            let tool = tracked.tablet_tool;
            for button in std::mem::take(&mut tracked.tablet_held_buttons) {
                let seat_button_count = release_seat_button(device);
                (*tool)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut payload = tablet_tool_payload(tracked, device, time_usec, tool, 1);
                payload.button = button;
                payload.button_state = 0;
                payload.seat_button_count = seat_button_count;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_BUTTON,
                    payload: EventPayload::TabletTool(payload),
                    context: ctx,
                    device,
                });
            }
            (*tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY,
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    tracked, device, time_usec, tool, 0,
                )),
                context: ctx,
                device,
            });
            tracked.tablet_tool = std::ptr::null_mut();
            tracked.tablet_zero_pressure_since = None;
            tracked.tablet_buttons_down = 0;
        }
        if tracked.touch_active && (*device).has_touch {
            tracked.touch_active = false;
            tracked.touch_fingers = 0;
            tracked.touch_start_time = None;
            for slot in &mut tracked.mt_slots {
                if let Some(seat_slot) = slot.seat_slot.take() {
                    release_touch_seat_slot(ctx, seat_slot);
                }
                slot.active = false;
                slot.reported = false;
                slot.dirty = false;
                slot.tracking_id = -1;
            }
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TOUCH_CANCEL,
                payload: EventPayload::TouchCancel(TouchEvent {
                    time_usec,
                    slot: -1,
                    seat_slot: -1,
                    x: 0.0,
                    y: 0.0,
                }),
                context: ctx,
                device,
            });
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TOUCH_FRAME,
                payload: EventPayload::TouchFrame { time_usec },
                context: ctx,
                device,
            });
        }
    }

    // -----------------------------------------------------------------------
    // Main drain loop
    // -----------------------------------------------------------------------

    unsafe fn emit_forced_tablet_proximity_out(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        for td in self.devices.values_mut() {
            if !td.tablet_proximity_timer_enabled || td.tablet_buttons_down != 0 {
                continue;
            }
            let Some(since) = td.tablet_zero_pressure_since else {
                continue;
            };
            if since.elapsed() < Duration::from_millis(150) || td.tablet_tool.is_null() {
                continue;
            }
            let tool = td.tablet_tool;
            (*tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY,
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    td,
                    td.lib_device,
                    systime_to_usec(SystemTime::now()),
                    tool,
                    0,
                )),
                context: ctx,
                device: td.lib_device,
            });
            td.tablet_tool = std::ptr::null_mut();
            td.tablet_zero_pressure_since = None;
        }
    }

    pub unsafe fn drain_into_queue(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        // --- hotplug ---
        self.handle_hotplug(ctx, out);

        // --- synthetic key-repeat before reading new events ---
        self.emit_key_repeats(ctx, out);
        self.emit_forced_tablet_proximity_out(ctx, out);

        // A lone left/right press is held briefly while middle-button
        // emulation waits for a possible chord. Once the chord window has
        // elapsed, deliver the original press before processing newer input.
        let middle_timeout = Duration::from_millis(50);
        let debounce_timeout = Duration::from_millis(25);
        let timed_out: Vec<RawFd> = self
            .devices
            .iter()
            .filter_map(|(fd, tracked)| {
                tracked
                    .middle_pending_since
                    .is_some_and(|since| since.elapsed() >= middle_timeout)
                    .then_some(*fd)
            })
            .collect();
        for fd in timed_out {
            let Some(tracked) = self.devices.get_mut(&fd) else {
                continue;
            };
            let Some(button) = tracked.middle_pending_button.take() else {
                continue;
            };
            tracked.middle_pending_since = None;
            Self::emit_pointer_button(
                systime_to_usec(SystemTime::now()),
                tracked.lib_device,
                ctx,
                tracked,
                button,
                true,
                out,
            );
        }

        let debounce_timed_out: Vec<(RawFd, u16, bool)> = self
            .devices
            .iter()
            .flat_map(|(fd, tracked)| {
                tracked
                    .debounce_buttons
                    .iter()
                    .filter_map(move |(code, state)| {
                        state
                            .pending_since
                            .is_some_and(|since| since.elapsed() >= debounce_timeout)
                            .then_some((*fd, *code, state.pending_down?))
                    })
            })
            .collect();
        for (fd, code, down) in debounce_timed_out {
            let Some(tracked) = self.devices.get_mut(&fd) else {
                continue;
            };
            if let Some(state) = tracked.debounce_buttons.get_mut(&code) {
                state.delivered_down = down;
                state.pending_down = None;
                state.pending_since = None;
                state.window_since = None;
            }
            tracked.debounce_spurious = true;
            Self::emit_pointer_button(
                systime_to_usec(SystemTime::now()),
                tracked.lib_device,
                ctx,
                tracked,
                code,
                down,
                out,
            );
        }

        // --- read events from every open device ---
        let pointing_stick_device = self
            .devices
            .values()
            .find(|tracked| tracked.is_pointing_stick)
            .map(|tracked| tracked.lib_device);
        let fds: Vec<RawFd> = self.devices.keys().copied().collect();
        let mut dead_fds: Vec<RawFd> = Vec::new();

        for fd in fds {
            let td = match self.devices.get_mut(&fd) {
                Some(d) => d,
                None => continue,
            };

            let batch: Vec<InputEvent> = match td.device.fetch_events() {
                Ok(b) => b.collect(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e)
                    if e.raw_os_error() == Some(nix::libc::ENODEV)
                        || e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    dead_fds.push(fd);
                    continue;
                }
                Err(_) => continue,
            };

            let lib_dev = td.lib_device;
            let is_abs = td.is_absolute;
            let is_kbd = td.is_keyboard;
            let is_ptr = td.is_pointer;
            let is_tablet_tool = unsafe { &*lib_dev }.has_tablet
                && !unsafe { &*lib_dev }.has_touch
                && !unsafe { &*lib_dev }.has_tablet_pad;
            let cfg_tap = unsafe { &*lib_dev }.tap_enabled;
            let cfg_nat = unsafe { &*lib_dev }.natural_scroll;
            let cfg_dwt = unsafe { &*lib_dev }.dwt_enabled;
            let cfg_accel = unsafe { &*lib_dev }.accel_speed as f32 + 1.0;

            let send_events_disabled = unsafe { &*lib_dev }.send_events_mode == 1;
            if send_events_disabled {
                if td.is_topbuttonpad {
                    for ev in &batch {
                        Self::process_disabled_topbutton_event(
                            ev,
                            systime_to_usec(ev.timestamp()),
                            pointing_stick_device,
                            ctx,
                            td,
                            out,
                        );
                    }
                }
                continue;
            }

            let global_typing = self.global_typing_time;

            for ev in &batch {
                let ts_usec = systime_to_usec(ev.timestamp());

                if is_tablet_tool {
                    Self::process_tablet_tool_event(ev, ts_usec, lib_dev, ctx, td, out);
                    continue;
                }

                if td.is_absolute_pointer {
                    Self::process_absolute_pointer_event(ev, ts_usec, lib_dev, ctx, td, out);
                    continue;
                }

                // ---- Keyboard device ----
                if is_kbd && !is_abs && !is_ptr {
                    Self::process_keyboard_event(
                        ev,
                        ts_usec,
                        lib_dev,
                        ctx,
                        td,
                        out,
                        &mut self.global_typing_time,
                    );
                    continue;
                }

                // ---- Relative device (mouse / trackpoint) ----
                if !is_abs && is_ptr {
                    Self::process_relative_event(ev, ts_usec, lib_dev, ctx, td, out);
                    continue;
                }

                // ---- Absolute device (touchpad) ----
                if is_abs {
                    let dwt_active = cfg_dwt
                        && global_typing
                            .map(|t| t.elapsed() < Duration::from_millis(500))
                            .unwrap_or(false);
                    Self::process_absolute_event(
                        ev,
                        ts_usec,
                        lib_dev,
                        pointing_stick_device,
                        ctx,
                        td,
                        out,
                        cfg_tap,
                        cfg_nat,
                        cfg_accel,
                        dwt_active,
                    );
                }
            }
        }

        // Remove dead devices
        for fd in dead_fds {
            if let Some(device) = self.devices.get(&fd).map(|tracked| tracked.lib_device) {
                self.release_active_inputs(ctx, device, out);
            }
            if let Some(td) = self.devices.remove(&fd) {
                (*ctx).unregister_fd(fd);
                let lib_device = td.close((*ctx).interface, (*ctx).user_data);
                (*lib_device)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
                    payload: EventPayload::DeviceRemoved,
                    context: ctx,
                    device: lib_device,
                });
                Self::release_context_device(ctx, lib_device);
            }
        }

        let next_middle_timeout = self
            .devices
            .values()
            .filter_map(|tracked| tracked.middle_pending_since)
            .map(|since| middle_timeout.saturating_sub(since.elapsed()))
            .min();
        let next_debounce_timeout = self
            .devices
            .values()
            .flat_map(|tracked| tracked.debounce_buttons.values())
            .filter_map(|state| state.pending_since)
            .map(|since| debounce_timeout.saturating_sub(since.elapsed()))
            .min();
        (*ctx).arm_timer(match (next_middle_timeout, next_debounce_timeout) {
            (Some(middle), Some(debounce)) => Some(middle.min(debounce)),
            (middle, debounce) => middle.or(debounce),
        });
    }

    // -----------------------------------------------------------------------
    // Hotplug
    // -----------------------------------------------------------------------

    unsafe fn handle_hotplug(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        if self.suspended {
            return;
        }
        let saw_event = {
            let Some(ref ino) = self.inotify else { return };
            let Ok(ievents) = ino.read_events() else {
                return;
            };
            !ievents.is_empty()
        };
        if !saw_event {
            return;
        }

        let disappeared: Vec<*mut LibinputDevice> = self
            .devices
            .values()
            .filter(|tracked| !tracked.path.exists())
            .map(|tracked| tracked.lib_device)
            .collect();
        for device in disappeared {
            self.release_active_inputs(ctx, device, out);
            let fd = self
                .devices
                .iter()
                .find_map(|(fd, tracked)| (tracked.lib_device == device).then_some(*fd));
            let Some(fd) = fd else { continue };
            let Some(tracked) = self.devices.remove(&fd) else {
                continue;
            };
            (*ctx).unregister_fd(fd);
            let lib_device = tracked.close((*ctx).interface, (*ctx).user_data);
            (*lib_device)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
                payload: EventPayload::DeviceRemoved,
                context: ctx,
                device: lib_device,
            });
            Self::release_context_device(ctx, lib_device);
        }

        if (*ctx).backend_kind != BackendKind::Udev {
            return;
        }
        let mut tmp: Vec<LibinputEvent> = Vec::new();
        self.scan_and_open(ctx, &mut tmp);
        for ev in tmp {
            out.push_back(ev);
        }
    }

    pub unsafe fn suspend(&mut self, ctx: *mut LibinputContext, out: &mut VecDeque<LibinputEvent>) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        let devices: Vec<*mut LibinputDevice> = self
            .devices
            .values()
            .map(|tracked| tracked.lib_device)
            .collect();
        for device in devices {
            self.release_active_inputs(ctx, device, out);
        }
        for (fd, td) in self.devices.drain() {
            (*ctx).unregister_fd(fd);
            let lib_device = td.close((*ctx).interface, (*ctx).user_data);
            (*lib_device)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
                payload: EventPayload::DeviceRemoved,
                context: ctx,
                device: lib_device,
            });
            Self::release_context_device(ctx, lib_device);
        }
    }

    pub unsafe fn remove_device(
        &mut self,
        ctx: *mut LibinputContext,
        device: *mut LibinputDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) -> bool {
        let fd = self
            .devices
            .iter()
            .find_map(|(fd, tracked)| (tracked.lib_device == device).then_some(*fd));
        let Some(fd) = fd else {
            return false;
        };
        self.release_active_inputs(ctx, device, out);
        let tracked = self
            .devices
            .remove(&fd)
            .expect("tracked device disappeared");
        (*ctx).unregister_fd(fd);
        let lib_device = tracked.close((*ctx).interface, (*ctx).user_data);
        (*lib_device)
            .refcount
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        out.push_back(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
            payload: EventPayload::DeviceRemoved,
            context: ctx,
            device: lib_device,
        });
        true
    }

    pub unsafe fn close_all(
        &mut self,
        interface: *const crate::ffi_types::LibinputInterface,
        user_data: *mut libc::c_void,
    ) {
        for (_, tracked) in self.devices.drain() {
            tracked.close(interface, user_data);
        }
    }

    pub fn forget_path(&mut self, path: &std::path::Path) {
        if let Some(index) = self
            .requested_paths
            .iter()
            .position(|requested| requested == path)
        {
            self.requested_paths.remove(index);
        }
    }

    pub fn remember_path(&mut self, path: &std::path::Path) {
        self.requested_paths.push(path.to_path_buf());
    }

    pub unsafe fn resume(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) -> libc::c_int {
        self.suspended = false;
        if (*ctx).backend_kind == BackendKind::Udev {
            let mut events = Vec::new();
            self.scan_and_open(ctx, &mut events);
            out.extend(events);
            return 0;
        }

        let paths = self.requested_paths.to_vec();
        let mut events = Vec::new();
        let mut failed = false;
        for path in paths {
            let old_len = self.devices.len();
            self.try_open(ctx, &path, &mut events);
            failed |= self.devices.len() == old_len;
        }
        out.extend(events);
        if failed {
            for (fd, tracked) in self.devices.drain() {
                (*ctx).unregister_fd(fd);
                let lib_device = tracked.close((*ctx).interface, (*ctx).user_data);
                (*lib_device)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
                    payload: EventPayload::DeviceRemoved,
                    context: ctx,
                    device: lib_device,
                });
                Self::release_context_device(ctx, lib_device);
            }
            -1
        } else {
            0
        }
    }

    // -----------------------------------------------------------------------
    // Synthetic key repeat
    // -----------------------------------------------------------------------

    unsafe fn emit_key_repeats(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let now = Instant::now();
        for td in self.devices.values_mut() {
            if !td.is_keyboard {
                continue;
            }
            let lib_dev = td.lib_device;
            for hk in &mut td.held_keys {
                let delay = if hk.initial_fired {
                    Duration::from_millis(REPEAT_INTERVAL_MS)
                } else {
                    Duration::from_millis(REPEAT_DELAY_MS)
                };
                if now.duration_since(hk.last_fire) >= delay {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_KEYBOARD_KEY,
                        payload: EventPayload::KeyboardKey(KeyboardKeyEvent {
                            time_usec: hk.ts_usec,
                            key: hk.code as u32,
                            state: 2, // LIBINPUT_KEY_STATE_REPEAT
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                    hk.last_fire = now;
                    hk.initial_fired = true;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard event processing
    // -----------------------------------------------------------------------

    unsafe fn process_tablet_tool_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        if ev.event_type() == EventType::ABSOLUTE {
            if ev.code() == AbsoluteAxisCode::ABS_X.0
                || ev.code() == AbsoluteAxisCode::ABS_MT_POSITION_X.0
            {
                td.tablet_x = f64::from(ev.value());
                td.tablet_x_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_Y.0
                || ev.code() == AbsoluteAxisCode::ABS_MT_POSITION_Y.0
            {
                td.tablet_y = f64::from(ev.value());
                td.tablet_y_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_PRESSURE.0 {
                td.tablet_pressure = f64::from(ev.value());
                td.tablet_pressure_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_DISTANCE.0 {
                td.tablet_distance = f64::from(ev.value());
                td.tablet_distance_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_TILT_X.0 {
                td.tablet_tilt_x = f64::from(ev.value());
                td.tablet_tilt_x_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_TILT_Y.0 {
                td.tablet_tilt_y = f64::from(ev.value());
                td.tablet_tilt_y_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_Z.0
                || ev.code() == AbsoluteAxisCode::ABS_MT_ORIENTATION.0
            {
                td.tablet_rotation = f64::from(ev.value());
                td.tablet_rotation_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_WHEEL.0 {
                td.tablet_slider = f64::from(ev.value());
                td.tablet_slider_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_MISC.0 && ev.value() >= 0 {
                td.tablet_tool_id = ev.value() as u64;
            } else if ev.code() == AbsoluteAxisCode::ABS_MT_TOOL_TYPE.0 && ev.value() == 10 {
                td.tablet_tool_type = 8;
                td.tablet_proximity_timer_enabled = false;
                td.tablet_zero_pressure_since = None;
            } else if ev.code() == AbsoluteAxisCode::ABS_MT_TRACKING_ID.0 {
                td.tablet_tool_type = 8;
                td.tablet_proximity_timer_enabled = false;
                td.tablet_zero_pressure_since = None;
                td.tablet_proximity_pending = Some(ev.value() >= 0);
            }
            td.tablet_axes_changed = true;
            return;
        }
        if ev.event_type() == EventType::RELATIVE && ev.code() == RelativeAxisCode::REL_WHEEL.0 {
            td.tablet_wheel_discrete = -ev.value();
            td.tablet_wheel_delta = f64::from(td.tablet_wheel_discrete)
                * (*lib_dev).wheel_click_angle_vertical;
            td.tablet_wheel_changed = true;
            td.tablet_axes_changed = true;
            return;
        }
        if ev.event_type() == EventType::KEY {
            if ev.code() == KeyCode::BTN_TOUCH.0 {
                td.tablet_tip_down = ev.value() != 0;
                td.tablet_tip_pending = Some(td.tablet_tip_down);
                return;
            }
            let is_tool_button = td.tablet_buttons.contains(&u32::from(ev.code()))
                && match td.tablet_tool_type {
                    8 => ev.code() == 0x100,
                    _ => matches!(ev.code(), 0x149 | 0x14b | 0x14c)
                        || (0x110..=0x117).contains(&ev.code()),
                };
            if is_tool_button {
                let tool = td.tablet_tool;
                if tool.is_null() {
                    return;
                }
                let pressed = ev.value() != 0;
                if pressed {
                    if !td.tablet_held_buttons.contains(&u32::from(ev.code())) {
                        td.tablet_held_buttons.push(u32::from(ev.code()));
                    }
                } else {
                    td.tablet_held_buttons
                        .retain(|button| *button != u32::from(ev.code()));
                }
                td.tablet_buttons_down = td.tablet_held_buttons.len() as u32;
                let seat_button_count = if pressed {
                    press_seat_button(lib_dev)
                } else {
                    release_seat_button(lib_dev)
                };
                (*tool)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut payload = tablet_tool_payload(td, lib_dev, ts_usec, tool, 1);
                payload.button = u32::from(ev.code());
                payload.button_state = u32::from(pressed);
                payload.seat_button_count = seat_button_count;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_BUTTON,
                    payload: EventPayload::TabletTool(payload),
                    context: ctx,
                    device: lib_dev,
                });
                return;
            }
            td.tablet_tool_type = match ev.code() {
                code if code == KeyCode::BTN_TOOL_RUBBER.0 => 2,
                code if code == KeyCode::BTN_TOOL_BRUSH.0 => 3,
                code if code == KeyCode::BTN_TOOL_PENCIL.0 => 4,
                code if code == KeyCode::BTN_TOOL_AIRBRUSH.0 => 5,
                code if code == KeyCode::BTN_TOOL_MOUSE.0 => 6,
                code if code == KeyCode::BTN_TOOL_LENS.0 => 7,
                code if code == KeyCode::BTN_TOOL_PEN.0 => 1,
                _ => return,
            };
            if td.tablet_tool_type != 1 || ev.value() == 0 {
                td.tablet_proximity_timer_enabled = false;
                td.tablet_zero_pressure_since = None;
            }
            td.tablet_proximity_pending = Some(ev.value() != 0);
            return;
        }
        if ev.event_type().0 == 4 && ev.code() == 0 {
            if ev.value() >= 0 {
                td.tablet_serial = ev.value() as u64;
            }
            return;
        }
        if ev.event_type() != EventType::SYNCHRONIZATION || ev.code() != 0 {
            return;
        }
        if td.tablet_proximity_timer_enabled && !td.tablet_tool.is_null() {
            td.tablet_zero_pressure_since = Some(Instant::now());
        }
        if td.tablet_pressure_changed {
            if let Some((minimum, maximum)) = td.tablet_pressure_range {
                let range = f64::from(maximum - minimum);
                let lower = f64::from(minimum) + (range * 0.01).trunc();
                let upper = f64::from(minimum) + (range * 0.05).trunc();
                let next_tip_state = if td.tablet_tip_down {
                    td.tablet_pressure > lower
                } else {
                    td.tablet_pressure >= upper
                };
                if next_tip_state != td.tablet_tip_down {
                    td.tablet_tip_down = next_tip_state;
                    td.tablet_tip_pending = Some(next_tip_state);
                }
            }
        }
        // Tablets handled by the proximity watchdog may never expose a
        // BTN_TOOL_PEN key. For those devices, the first axis frame is the
        // only reliable indication that a pen is in range. The same rule
        // restores proximity after the watchdog synthesized an out event.
        if td.tablet_proximity_timer_enabled
            && td.tablet_tool.is_null()
            && td.tablet_axes_changed
            && td.tablet_proximity_pending.is_none()
        {
            td.tablet_tool_type = 1;
            td.tablet_proximity_pending = Some(true);
        }
        let Some(in_proximity) = td.tablet_proximity_pending.take() else {
            let is_tip_event = td.tablet_tip_pending.take().is_some();
            if td.tablet_tool.is_null() || (!td.tablet_axes_changed && !is_tip_event) {
                return;
            }
            let tool = td.tablet_tool;
            (*tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: if is_tip_event {
                    LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_TIP
                } else {
                    LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_AXIS
                },
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    td, lib_dev, ts_usec, tool, 1,
                )),
                context: ctx,
                device: lib_dev,
            });
            td.tablet_x_changed = false;
            td.tablet_y_changed = false;
            td.tablet_pressure_changed = false;
            td.tablet_distance_changed = false;
            td.tablet_tilt_x_changed = false;
            td.tablet_tilt_y_changed = false;
            td.tablet_rotation_changed = false;
            td.tablet_slider_changed = false;
            td.tablet_wheel_delta = 0.0;
            td.tablet_wheel_discrete = 0;
            td.tablet_wheel_changed = false;
            td.tablet_axes_changed = false;
            return;
        };
        if in_proximity
            && !td.tablet_tool.is_null()
            && (*td.tablet_tool).tool_type != td.tablet_tool_type
        {
            let previous_tool = td.tablet_tool;
            (*previous_tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY,
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    td,
                    lib_dev,
                    ts_usec,
                    previous_tool,
                    0,
                )),
                context: ctx,
                device: lib_dev,
            });
            td.tablet_tool = std::ptr::null_mut();
        }
        if in_proximity {
            // Proximity-in establishes a complete coordinate state even
            // when evdev suppresses an unchanged axis from the frame.
            td.tablet_x_changed = true;
            td.tablet_y_changed = true;
            td.tablet_pressure_changed = td.tablet_has_pressure;
            td.tablet_distance_changed = td.tablet_has_distance;
            td.tablet_tilt_x_changed = td.tablet_has_tilt;
            td.tablet_tilt_y_changed = td.tablet_has_tilt;
            td.tablet_rotation_changed = td.tablet_has_rotation;
            td.tablet_slider_changed = td.tablet_has_slider;
            td.tablet_wheel_changed = td.tablet_has_wheel;
        }
        let tool = if in_proximity {
            let tool = tablet_tool_for(
                ctx,
                lib_dev,
                td.tablet_serial,
                td.tablet_tool_id,
                td.tablet_tool_type,
                [
                    td.tablet_has_pressure,
                    td.tablet_has_distance,
                    td.tablet_has_tilt,
                    td.tablet_has_rotation,
                    td.tablet_has_slider,
                    td.tablet_has_wheel,
                    td.tablet_has_size,
                ],
                &td.tablet_buttons,
            );
            td.tablet_tool = tool;
            tool
        } else {
            td.tablet_tool
        };
        if tool.is_null() {
            return;
        }
        let tip_before_proximity = !in_proximity && td.tablet_tip_pending == Some(false);
        if tip_before_proximity {
            (*tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_TIP,
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    td, lib_dev, ts_usec, tool, 1,
                )),
                context: ctx,
                device: lib_dev,
            });
            td.tablet_tip_pending = None;
        }
        if !in_proximity {
            for button in std::mem::take(&mut td.tablet_held_buttons) {
                let seat_button_count = release_seat_button(lib_dev);
                (*tool)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut payload = tablet_tool_payload(td, lib_dev, ts_usec, tool, 1);
                payload.button = button;
                payload.button_state = 0;
                payload.seat_button_count = seat_button_count;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_BUTTON,
                    payload: EventPayload::TabletTool(payload),
                    context: ctx,
                    device: lib_dev,
                });
            }
            td.tablet_buttons_down = 0;
        }
        (*tool)
            .refcount
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        out.push_back(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY,
            payload: EventPayload::TabletTool(tablet_tool_payload(
                td,
                lib_dev,
                ts_usec,
                tool,
                u32::from(in_proximity),
            )),
            context: ctx,
            device: lib_dev,
        });
        if in_proximity && td.tablet_tip_pending.take().is_some() {
            (*tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_TIP,
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    td, lib_dev, ts_usec, tool, 1,
                )),
                context: ctx,
                device: lib_dev,
            });
        }
        if !in_proximity {
            td.tablet_tool = std::ptr::null_mut();
            td.tablet_zero_pressure_since = None;
        } else if td.tablet_proximity_timer_enabled && td.tablet_tool_type == 1 {
            td.tablet_zero_pressure_since = Some(Instant::now());
        }
        td.tablet_x_changed = false;
        td.tablet_y_changed = false;
        td.tablet_pressure_changed = false;
        td.tablet_distance_changed = false;
        td.tablet_tilt_x_changed = false;
        td.tablet_tilt_y_changed = false;
        td.tablet_rotation_changed = false;
        td.tablet_slider_changed = false;
        td.tablet_wheel_delta = 0.0;
        td.tablet_wheel_discrete = 0;
        td.tablet_wheel_changed = false;
        td.tablet_axes_changed = false;
    }

    unsafe fn process_keyboard_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
        global_typing_time: &mut Option<Instant>,
    ) {
        if ev.event_type() != EventType::KEY {
            return;
        }
        let code = ev.code();
        let value = ev.value(); // 0=up 1=down 2=repeat(kernel)

        // Track modifiers for DWT
        match code {
            c if c == KeyCode::KEY_LEFTCTRL.0 || c == KeyCode::KEY_RIGHTCTRL.0 => {}
            c if c == KeyCode::KEY_LEFTALT.0 || c == KeyCode::KEY_RIGHTALT.0 => {}
            _ => {}
        }

        if value == 1 {
            // Key down: update DWT, start repeat tracking
            *global_typing_time = Some(Instant::now());
            td.last_typing_time = Some(Instant::now());
            td.held_keys.push(HeldKey {
                code,
                ts_usec,
                last_fire: Instant::now(),
                initial_fired: false,
            });
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_KEYBOARD_KEY,
                payload: EventPayload::KeyboardKey(KeyboardKeyEvent {
                    time_usec: ts_usec,
                    key: code as u32,
                    state: 1, // LIBINPUT_KEY_STATE_PRESSED
                }),
                context: ctx,
                device: lib_dev,
            });
        } else if value == 0 {
            // Key up: remove from repeat tracking
            td.held_keys.retain(|k| k.code != code);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_KEYBOARD_KEY,
                payload: EventPayload::KeyboardKey(KeyboardKeyEvent {
                    time_usec: ts_usec,
                    key: code as u32,
                    state: 0, // LIBINPUT_KEY_STATE_RELEASED
                }),
                context: ctx,
                device: lib_dev,
            });
        }
        // value==2 (kernel repeat) is intentionally dropped; we synthesise
        // our own repeats in emit_key_repeats() with correct timing.
    }

    // -----------------------------------------------------------------------
    // Relative (mouse / trackpoint) event processing
    // -----------------------------------------------------------------------

    unsafe fn process_absolute_pointer_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        match ev.event_type() {
            EventType::ABSOLUTE => {
                if ev.code() == AbsoluteAxisCode::ABS_X.0 {
                    td.current_abs_x = Some(ev.value());
                    td.absolute_changed = true;
                } else if ev.code() == AbsoluteAxisCode::ABS_Y.0 {
                    td.current_abs_y = Some(ev.value());
                    td.absolute_changed = true;
                }
            }
            EventType::SYNCHRONIZATION if ev.code() == 0 && td.absolute_changed => {
                let (Some(x), Some(y), Some((x_min, x_max)), Some((y_min, y_max))) = (
                    td.current_abs_x,
                    td.current_abs_y,
                    td.abs_x_range,
                    td.abs_y_range,
                ) else {
                    td.absolute_changed = false;
                    return;
                };
                td.absolute_changed = false;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION_ABSOLUTE,
                    payload: EventPayload::PointerMotionAbsolute(PointerMotionAbsoluteEvent {
                        time_usec: ts_usec,
                        abs_x: f64::from(x),
                        abs_y: f64::from(y),
                        x_min: f64::from(x_min),
                        x_max: f64::from(x_max),
                        y_min: f64::from(y_min),
                        y_max: f64::from(y_max),
                    }),
                    context: ctx,
                    device: lib_dev,
                });
            }
            _ => Self::process_relative_event(ev, ts_usec, lib_dev, ctx, td, out),
        }
    }

    unsafe fn process_relative_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        match ev.event_type() {
            EventType::RELATIVE => {
                let code = ev.code();
                let val = ev.value();
                if code == RelativeAxisCode::REL_X.0 {
                    td.pending_rel_x += i64::from(val);
                } else if code == RelativeAxisCode::REL_Y.0 {
                    td.pending_rel_y += i64::from(val);
                } else if code == RelativeAxisCode::REL_WHEEL.0 {
                    td.pending_wheel_vertical += i64::from(val);
                } else if code == RelativeAxisCode::REL_HWHEEL.0 {
                    td.pending_wheel_horizontal += i64::from(val);
                } else if code == RelativeAxisCode::REL_WHEEL_HI_RES.0
                    && td.supports_hi_res_vertical
                {
                    td.pending_wheel_hi_res_vertical += i64::from(val);
                } else if code == RelativeAxisCode::REL_HWHEEL_HI_RES.0
                    && td.supports_hi_res_horizontal
                {
                    td.pending_wheel_hi_res_horizontal += i64::from(val);
                }
            }
            EventType::SYNCHRONIZATION if ev.code() == 0 => {
                if td.scroll_button_down || td.scroll_lock_active {
                    let dx = td.pending_rel_x;
                    let dy = td.pending_rel_y;
                    td.pending_rel_x = 0;
                    td.pending_rel_y = 0;
                    if dx != 0 || dy != 0 {
                        let timeout_elapsed = td
                            .scroll_button_press_time
                            .is_some_and(|press| press.elapsed() >= Duration::from_millis(200));
                        if timeout_elapsed {
                            td.scroll_button_moved = true;
                            td.scroll_button_accum_x += dx;
                            td.scroll_button_accum_y += dy;
                            if td.scroll_button_axes == 0 {
                                let abs_x = td.scroll_button_accum_x.abs();
                                let abs_y = td.scroll_button_accum_y.abs();
                                if abs_x > 1 || abs_y > 1 {
                                    td.scroll_button_axes = if abs_y >= abs_x { 1 } else { 2 };
                                }
                            }
                            for (axis, value) in
                                [(0, td.scroll_button_accum_y), (1, td.scroll_button_accum_x)]
                            {
                                if td.scroll_button_axes & (1 << axis) == 0 || value.abs() <= 1 {
                                    continue;
                                }
                                if axis == 0 {
                                    td.scroll_button_accum_y = 0;
                                } else {
                                    td.scroll_button_accum_x = 0;
                                }
                                for event_type in [
                                    LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_CONTINUOUS,
                                    LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
                                ] {
                                    out.push_back(LibinputEvent {
                                        event_type,
                                        payload: EventPayload::PointerAxis(PointerAxisEvent {
                                            time_usec: ts_usec,
                                            axis,
                                            value: value as f64,
                                            value_discrete: 0,
                                            value_v120: 0.0,
                                            source: 3,
                                        }),
                                        context: ctx,
                                        device: lib_dev,
                                    });
                                }
                            }
                        }
                    }
                    return;
                }

                if td.pending_rel_x != 0 || td.pending_rel_y != 0 {
                    let dx = td.pending_rel_x as f64;
                    let dy = td.pending_rel_y as f64;
                    td.pending_rel_x = 0;
                    td.pending_rel_y = 0;
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION,
                        payload: EventPayload::PointerMotion(PointerMotionEvent {
                            time_usec: ts_usec,
                            dx,
                            dy,
                            dx_unaccel: dx,
                            dy_unaccel: dy,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                }

                if td.is_lenovo_scrollpoint {
                    for (axis, raw, direction) in [
                        (0, td.pending_wheel_vertical, -1.0),
                        (1, td.pending_wheel_horizontal, 1.0),
                    ] {
                        if raw == 0 {
                            continue;
                        }
                        let value = raw as f64 * direction;
                        for event_type in [
                            LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_CONTINUOUS,
                            LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
                        ] {
                            out.push_back(LibinputEvent {
                                event_type,
                                payload: EventPayload::PointerAxis(PointerAxisEvent {
                                    time_usec: ts_usec,
                                    axis,
                                    value,
                                    value_discrete: 0,
                                    value_v120: 0.0,
                                    source: 3,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                        }
                    }
                    td.pending_wheel_vertical = 0;
                    td.pending_wheel_horizontal = 0;
                    td.pending_wheel_hi_res_vertical = 0;
                    td.pending_wheel_hi_res_horizontal = 0;
                    return;
                }

                let natural_direction = if (*lib_dev).natural_scroll { -1.0 } else { 1.0 };
                for (axis, low_res, high_res, direction) in [
                    (
                        0,
                        td.pending_wheel_vertical,
                        td.pending_wheel_hi_res_vertical,
                        -1.0,
                    ),
                    (
                        1,
                        td.pending_wheel_horizontal,
                        td.pending_wheel_hi_res_horizontal,
                        1.0,
                    ),
                ] {
                    let direction = direction * natural_direction;
                    let filtered_high_res = td.filter_hi_res_wheel(axis, high_res);
                    let (supports_hi_res, warned_missing_hi_res) = if axis == 0 {
                        (
                            td.supports_hi_res_vertical,
                            &mut td.warned_missing_hi_res_vertical,
                        )
                    } else {
                        (
                            td.supports_hi_res_horizontal,
                            &mut td.warned_missing_hi_res_horizontal,
                        )
                    };
                    if low_res != 0 && high_res == 0 && supports_hi_res && !*warned_missing_hi_res {
                        *warned_missing_hi_res = true;
                        crate::emit_error_log(
                            ctx,
                            "kernel bug: device supports high-resolution scroll but only low-resolution events have been received.",
                        );
                    }
                    let click_angle = if axis == 0 {
                        (*lib_dev).wheel_click_angle_vertical
                    } else {
                        (*lib_dev).wheel_click_angle_horizontal
                    };
                    if filtered_high_res.is_some() || (high_res == 0 && low_res != 0) {
                        let raw_v120 = if let Some(high_res) = filtered_high_res {
                            high_res as f64
                        } else {
                            low_res as f64 * 120.0
                        };
                        let value_v120 = raw_v120 * direction;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_WHEEL,
                            payload: EventPayload::PointerAxis(PointerAxisEvent {
                                time_usec: ts_usec,
                                axis,
                                value: value_v120 / 120.0 * click_angle,
                                value_discrete: (value_v120 / 120.0) as i32,
                                value_v120,
                                source: 1,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    if low_res != 0 {
                        let discrete = (low_res as f64 * direction) as i32;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
                            payload: EventPayload::PointerAxis(PointerAxisEvent {
                                time_usec: ts_usec,
                                axis,
                                value: f64::from(discrete) * click_angle,
                                value_discrete: discrete,
                                value_v120: f64::from(discrete) * 120.0,
                                source: 1,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                }
                td.pending_wheel_vertical = 0;
                td.pending_wheel_horizontal = 0;
                td.pending_wheel_hi_res_vertical = 0;
                td.pending_wheel_hi_res_horizontal = 0;
            }
            EventType::KEY => {
                let code = ev.code();
                if (*lib_dev).middle_emulation && (*lib_dev).scroll_method != 4 {
                    let middle = KeyCode::BTN_MIDDLE.0;
                    if code == middle && ev.value() != 0 {
                        if td.middle_chord_active {
                            td.middle_chord_active = false;
                            Self::emit_pointer_button(
                                ts_usec, lib_dev, ctx, td, middle, false, out,
                            );
                        }
                        if let Some(pending) = td.middle_pending_button.take() {
                            td.middle_pending_since = None;
                            Self::emit_pointer_button(
                                ts_usec, lib_dev, ctx, td, pending, true, out,
                            );
                        }
                        td.middle_real_down = true;
                    } else if code == middle {
                        td.middle_real_down = false;
                    }

                    if code == KeyCode::BTN_LEFT.0 || code == KeyCode::BTN_RIGHT.0 {
                        let down = ev.value() != 0;
                        let is_left = code == KeyCode::BTN_LEFT.0;
                        if down
                            && !td.middle_left_down
                            && !td.middle_right_down
                            && td.held_buttons.is_empty()
                        {
                            td.left_handed_applied = (*lib_dev).left_handed;
                        }
                        if is_left {
                            td.middle_left_down = down;
                        } else {
                            td.middle_right_down = down;
                        }

                        let real_middle_down = td.middle_real_down;
                        let a_physical_button_was_delivered = td.held_buttons.iter().any(|held| {
                            *held == KeyCode::BTN_LEFT.0 || *held == KeyCode::BTN_RIGHT.0
                        });

                        if real_middle_down || a_physical_button_was_delivered {
                            let suppressed = if is_left {
                                &mut td.middle_suppressed_left
                            } else {
                                &mut td.middle_suppressed_right
                            };
                            if !down && *suppressed {
                                *suppressed = false;
                                return;
                            }
                            Self::emit_pointer_button(ts_usec, lib_dev, ctx, td, code, down, out);
                            return;
                        }

                        if down {
                            let opposite_is_down = if is_left {
                                td.middle_right_down
                            } else {
                                td.middle_left_down
                            };
                            let opposite_was_suppressed = if is_left {
                                td.middle_suppressed_right
                            } else {
                                td.middle_suppressed_left
                            };
                            if opposite_is_down
                                && (td.middle_pending_button.is_some() || opposite_was_suppressed)
                            {
                                td.middle_pending_button = None;
                                td.middle_pending_since = None;
                                td.middle_chord_active = true;
                                td.middle_suppressed_left = true;
                                td.middle_suppressed_right = true;
                                Self::emit_pointer_button(
                                    ts_usec, lib_dev, ctx, td, middle, true, out,
                                );
                            } else if td.middle_pending_button.is_none() {
                                td.middle_pending_button = Some(code);
                                td.middle_pending_since = Some(Instant::now());
                            }
                            return;
                        }

                        if td.middle_chord_active {
                            td.middle_chord_active = false;
                            if is_left {
                                td.middle_suppressed_left = false;
                            } else {
                                td.middle_suppressed_right = false;
                            }
                            Self::emit_pointer_button(
                                ts_usec, lib_dev, ctx, td, middle, false, out,
                            );
                            return;
                        }

                        let suppressed = if is_left {
                            &mut td.middle_suppressed_left
                        } else {
                            &mut td.middle_suppressed_right
                        };
                        if *suppressed {
                            *suppressed = false;
                            return;
                        }
                        if td.middle_pending_button == Some(code) {
                            td.middle_pending_since = None;
                            Self::emit_pointer_button(ts_usec, lib_dev, ctx, td, code, true, out);
                            Self::emit_pointer_button(ts_usec, lib_dev, ctx, td, code, false, out);
                            td.middle_pending_button = None;
                        } else {
                            Self::emit_pointer_button(ts_usec, lib_dev, ctx, td, code, false, out);
                        }
                        return;
                    }
                }
                if (*lib_dev).middle_emulation
                    && (*lib_dev).scroll_method == 4
                    && (*lib_dev).scroll_button == u32::from(KeyCode::BTN_LEFT.0)
                    && (code == KeyCode::BTN_LEFT.0 || code == KeyCode::BTN_RIGHT.0)
                {
                    let down = ev.value() != 0;
                    if code == KeyCode::BTN_LEFT.0 {
                        td.middle_left_down = down;
                    } else {
                        td.middle_right_down = down;
                    }

                    if down && td.middle_left_down && td.middle_right_down {
                        if td.scroll_button_down {
                            td.scroll_button_down = false;
                            td.scroll_button_lock_press = false;
                            td.scroll_button_press_time = None;
                        }
                        if !td.middle_chord_active {
                            td.middle_chord_active = true;
                            let seat_button_count = press_seat_button(lib_dev);
                            out.push_back(LibinputEvent {
                                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                payload: EventPayload::PointerButton(PointerButtonEvent {
                                    time_usec: ts_usec,
                                    button: u32::from(KeyCode::BTN_MIDDLE.0),
                                    state: 1,
                                    seat_button_count,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                        }
                        return;
                    }

                    if td.middle_chord_active {
                        if !td.middle_left_down && !td.middle_right_down {
                            td.middle_chord_active = false;
                            let seat_button_count = release_seat_button(lib_dev);
                            out.push_back(LibinputEvent {
                                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                payload: EventPayload::PointerButton(PointerButtonEvent {
                                    time_usec: ts_usec,
                                    button: u32::from(KeyCode::BTN_MIDDLE.0),
                                    state: 0,
                                    seat_button_count,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                        }
                        return;
                    }

                    // Hold a lone right press briefly so a following left press
                    // can still form a middle-button chord. A completed right
                    // click is replayed normally if no chord materializes.
                    if code == KeyCode::BTN_RIGHT.0 {
                        if down {
                            return;
                        }
                        let pressed_count = press_seat_button(lib_dev);
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                            payload: EventPayload::PointerButton(PointerButtonEvent {
                                time_usec: ts_usec,
                                button: u32::from(code),
                                state: 1,
                                seat_button_count: pressed_count,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                        let released_count = release_seat_button(lib_dev);
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                            payload: EventPayload::PointerButton(PointerButtonEvent {
                                time_usec: ts_usec,
                                button: u32::from(code),
                                state: 0,
                                seat_button_count: released_count,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                        return;
                    }
                }
                if (*lib_dev).middle_emulation
                    && (*lib_dev).scroll_method == 4
                    && (*lib_dev).scroll_button == u32::from(KeyCode::BTN_MIDDLE.0)
                    && (code == KeyCode::BTN_LEFT.0 || code == KeyCode::BTN_RIGHT.0)
                {
                    let down = ev.value() != 0;
                    if code == KeyCode::BTN_LEFT.0 {
                        td.middle_left_down = down;
                    } else {
                        td.middle_right_down = down;
                    }
                    if td.middle_left_down && td.middle_right_down && !td.scroll_button_down {
                        td.scroll_button_down = true;
                        td.scroll_button_press_time = Some(Instant::now());
                        td.scroll_button_moved = false;
                        td.scroll_button_axes = 0;
                        td.scroll_button_accum_x = 0;
                        td.scroll_button_accum_y = 0;
                    } else if !down && td.scroll_button_down {
                        td.scroll_button_down = false;
                        td.scroll_button_press_time = None;
                        Self::emit_button_scroll_stops(ts_usec, lib_dev, ctx, td, out);
                        td.scroll_button_moved = false;
                        td.scroll_button_axes = 0;
                        td.scroll_button_accum_x = 0;
                        td.scroll_button_accum_y = 0;
                    }
                    return;
                }
                if (*lib_dev).scroll_method == 4 && u32::from(code) == (*lib_dev).scroll_button {
                    let lock_enabled = (*lib_dev).scroll_button_lock == 1;
                    if ev.value() != 0 {
                        if td.scroll_button_down {
                            return;
                        }
                        // A lock configured while any button is already held only
                        // becomes eligible after the device returns to neutral.
                        // Until then the configured button remains an ordinary
                        // pointer button.
                        if lock_enabled && !td.held_buttons.is_empty() {
                            // Fall through to the generic button path below.
                        } else {
                            td.scroll_button_down = true;
                            td.scroll_button_lock_press = lock_enabled;
                            td.scroll_button_press_time = Some(Instant::now());
                            td.scroll_button_moved = false;
                            if !td.scroll_lock_active {
                                td.scroll_button_axes = 0;
                                td.scroll_button_accum_x = 0;
                                td.scroll_button_accum_y = 0;
                            }
                            return;
                        }
                    } else {
                        if !td.scroll_button_down {
                            // This button press pre-dated the active scroll
                            // configuration, so its release must stay paired with
                            // the ordinary press in the generic path.
                        } else {
                            td.scroll_button_down = false;
                            if td.scroll_button_lock_press {
                                td.scroll_button_lock_press = false;
                                if td.scroll_lock_active {
                                    td.scroll_lock_active = false;
                                    td.scroll_button_press_time = None;
                                    if td.scroll_button_moved || td.scroll_button_axes != 0 {
                                        Self::emit_button_scroll_stops(
                                            ts_usec, lib_dev, ctx, td, out,
                                        );
                                    } else {
                                        let pressed_count = press_seat_button(lib_dev);
                                        out.push_back(LibinputEvent {
                                            event_type:
                                                LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                            payload: EventPayload::PointerButton(
                                                PointerButtonEvent {
                                                    time_usec: ts_usec,
                                                    button: u32::from(code),
                                                    state: 1,
                                                    seat_button_count: pressed_count,
                                                },
                                            ),
                                            context: ctx,
                                            device: lib_dev,
                                        });
                                        let released_count = release_seat_button(lib_dev);
                                        out.push_back(LibinputEvent {
                                            event_type:
                                                LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                            payload: EventPayload::PointerButton(
                                                PointerButtonEvent {
                                                    time_usec: ts_usec,
                                                    button: u32::from(code),
                                                    state: 0,
                                                    seat_button_count: released_count,
                                                },
                                            ),
                                            context: ctx,
                                            device: lib_dev,
                                        });
                                    }
                                    td.scroll_button_moved = false;
                                    td.scroll_button_axes = 0;
                                    td.scroll_button_accum_x = 0;
                                    td.scroll_button_accum_y = 0;
                                } else {
                                    td.scroll_lock_active = true;
                                }
                                return;
                            }
                            td.scroll_button_lock_press = false;
                            td.scroll_button_press_time = None;
                            if td.scroll_button_moved {
                                Self::emit_button_scroll_stops(ts_usec, lib_dev, ctx, td, out);
                            } else {
                                let pressed_count = press_seat_button(lib_dev);
                                out.push_back(LibinputEvent {
                                    event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                    payload: EventPayload::PointerButton(PointerButtonEvent {
                                        time_usec: ts_usec,
                                        button: u32::from(code),
                                        state: 1,
                                        seat_button_count: pressed_count,
                                    }),
                                    context: ctx,
                                    device: lib_dev,
                                });
                                let released_count = release_seat_button(lib_dev);
                                out.push_back(LibinputEvent {
                                    event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                    payload: EventPayload::PointerButton(PointerButtonEvent {
                                        time_usec: ts_usec,
                                        button: u32::from(code),
                                        state: 0,
                                        seat_button_count: released_count,
                                    }),
                                    context: ctx,
                                    device: lib_dev,
                                });
                            }
                            td.scroll_button_moved = false;
                            td.scroll_button_axes = 0;
                            return;
                        }
                    }
                }
                let is_numbered_button = (KeyCode::BTN_0.0..=KeyCode::BTN_9.0).contains(&code);
                let is_mouse_button = (KeyCode::BTN_LEFT.0..=KeyCode::BTN_TASK.0).contains(&code);
                if is_numbered_button || is_mouse_button {
                    Self::emit_debounced_pointer_button(
                        ts_usec,
                        lib_dev,
                        ctx,
                        td,
                        code,
                        ev.value() != 0,
                        out,
                    );
                }
            }
            _ => {}
        }
    }

    unsafe fn emit_debounced_pointer_button(
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        code: u16,
        down: bool,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let now = Instant::now();
        let timeout = Duration::from_millis(25);
        let activate_bypass = !td.debounce_bypass
            && td.debounce_buttons.iter().any(|(other_code, state)| {
                *other_code != code
                    && (state.delivered_down
                        || state.pending_down.is_some()
                        || state
                            .window_since
                            .is_some_and(|since| since.elapsed() < timeout))
            });
        if activate_bypass {
            let pending: Vec<(u16, bool)> = td
                .debounce_buttons
                .iter()
                .filter_map(|(other_code, state)| {
                    (*other_code != code).then_some((*other_code, state.pending_down?))
                })
                .collect();
            for state in td.debounce_buttons.values_mut() {
                state.pending_down = None;
                state.pending_since = None;
                state.window_since = None;
            }
            td.debounce_bypass = true;
            for (pending_code, pending_down) in pending {
                if let Some(state) = td.debounce_buttons.get_mut(&pending_code) {
                    state.delivered_down = pending_down;
                }
                Self::emit_pointer_button(
                    ts_usec,
                    lib_dev,
                    ctx,
                    td,
                    pending_code,
                    pending_down,
                    out,
                );
            }
        }
        if td.debounce_bypass {
            let state = td.debounce_buttons.entry(code).or_default();
            if state.delivered_down != down {
                state.delivered_down = down;
                Self::emit_pointer_button(ts_usec, lib_dev, ctx, td, code, down, out);
            }
            if !down
                && td
                    .debounce_buttons
                    .values()
                    .all(|state| !state.delivered_down && state.pending_down.is_none())
            {
                td.debounce_bypass = false;
            }
            return;
        }
        let spurious = td.debounce_spurious;
        {
            let state = td.debounce_buttons.entry(code).or_default();
            if spurious {
                if down == state.delivered_down {
                    state.pending_down = None;
                    state.pending_since = None;
                } else {
                    state.pending_down = Some(down);
                    state.pending_since = Some(now);
                }
                return;
            }

            let window_active = state
                .window_since
                .is_some_and(|since| since.elapsed() < timeout);
            if down == state.delivered_down {
                if state.pending_down.is_some() {
                    state.pending_down = None;
                    state.pending_since = None;
                }
                return;
            }
            if window_active {
                state.pending_down = Some(down);
                state.pending_since = Some(now);
                return;
            }
            state.delivered_down = down;
            state.window_since = Some(now);
            state.pending_down = None;
            state.pending_since = None;
        }
        Self::emit_pointer_button(ts_usec, lib_dev, ctx, td, code, down, out);
    }

    unsafe fn emit_pointer_button(
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        code: u16,
        down: bool,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        if td.held_buttons.is_empty()
            && !td.middle_left_down
            && !td.middle_right_down
            && td.middle_pending_button.is_none()
        {
            td.left_handed_applied = (*lib_dev).left_handed;
        }
        let code = if td.left_handed_applied && code == KeyCode::BTN_LEFT.0 {
            KeyCode::BTN_RIGHT.0
        } else if td.left_handed_applied && code == KeyCode::BTN_RIGHT.0 {
            KeyCode::BTN_LEFT.0
        } else {
            code
        };
        let (state, seat_button_count) = if down {
            if td.held_buttons.contains(&code) {
                return;
            }
            td.held_buttons.push(code);
            (1, press_seat_button(lib_dev))
        } else {
            if !td.held_buttons.contains(&code) {
                return;
            }
            td.held_buttons.retain(|button| *button != code);
            let count = release_seat_button(lib_dev);
            if td.held_buttons.is_empty() {
                td.left_handed_applied = (*lib_dev).left_handed;
            }
            (0, count)
        };
        out.push_back(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
            payload: EventPayload::PointerButton(PointerButtonEvent {
                time_usec: ts_usec,
                button: u32::from(code),
                state,
                seat_button_count,
            }),
            context: ctx,
            device: lib_dev,
        });
    }

    unsafe fn emit_button_scroll_stops(
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        for axis in 0..2 {
            if td.scroll_button_axes & (1 << axis) == 0 {
                continue;
            }
            for event_type in [
                LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_CONTINUOUS,
                LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
            ] {
                out.push_back(LibinputEvent {
                    event_type,
                    payload: EventPayload::PointerAxis(PointerAxisEvent {
                        time_usec: ts_usec,
                        axis,
                        value: 0.0,
                        value_discrete: 0,
                        value_v120: 0.0,
                        source: 3,
                    }),
                    context: ctx,
                    device: lib_dev,
                });
            }
        }
    }

    // -----------------------------------------------------------------------
    // Absolute (touchpad) event processing
    // -----------------------------------------------------------------------

    unsafe fn process_disabled_topbutton_event(
        ev: &InputEvent,
        ts_usec: u64,
        pointing_stick_device: Option<*mut LibinputDevice>,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        if ev.event_type() == EventType::ABSOLUTE {
            if ev.code() == AbsoluteAxisCode::ABS_X.0 {
                td.last_x = Some(ev.value());
            } else if ev.code() == AbsoluteAxisCode::ABS_Y.0 {
                td.last_y = Some(ev.value());
            }
            return;
        }

        if ev.event_type() != EventType::KEY || ev.code() != KeyCode::BTN_LEFT.0 {
            return;
        }

        let (button, event_device) = if ev.value() != 0 {
            let Some(event_device) = pointing_stick_device else {
                return;
            };
            let (Some(x), Some(y), Some((xmin, xmax)), Some((ymin, ymax))) =
                (td.last_x, td.last_y, td.abs_x_range, td.abs_y_range)
            else {
                return;
            };
            let top_button_bottom = ymin + (ymax - ymin) / 5;
            if y > top_button_bottom {
                return;
            }
            let third = (xmax - xmin) / 3;
            let button = if x >= xmin + third * 2 {
                KeyCode::BTN_RIGHT.0
            } else if x >= xmin + third {
                KeyCode::BTN_MIDDLE.0
            } else {
                KeyCode::BTN_LEFT.0
            };
            td.active_click_button = Some(button);
            td.active_click_device = Some(event_device);
            (button, event_device)
        } else {
            let Some(button) = td.active_click_button.take() else {
                return;
            };
            let event_device = td.active_click_device.take().unwrap_or(td.lib_device);
            (button, event_device)
        };

        let seat_button_count = if ev.value() != 0 {
            press_seat_button(event_device)
        } else {
            release_seat_button(event_device)
        };

        out.push_back(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
            payload: EventPayload::PointerButton(PointerButtonEvent {
                time_usec: ts_usec,
                button: button as u32,
                state: ev.value() as u32,
                seat_button_count,
            }),
            context: ctx,
            device: event_device,
        });
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn process_absolute_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        pointing_stick_device: Option<*mut LibinputDevice>,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
        cfg_tap: bool,
        cfg_nat: bool,
        cfg_accel: f32,
        dwt_active: bool,
    ) {
        if dwt_active {
            td.tap_emitted = true;
        }

        match ev.event_type() {
            EventType::KEY => {
                let code = ev.code();
                let value = ev.value();

                if (*lib_dev).middle_emulation
                    && (*lib_dev).scroll_method != 4
                    && (code == KeyCode::BTN_LEFT.0
                        || code == KeyCode::BTN_RIGHT.0
                        || code == KeyCode::BTN_MIDDLE.0)
                {
                    Self::process_relative_event(ev, ts_usec, lib_dev, ctx, td, out);
                    return;
                }

                if code == KeyCode::BTN_TOUCH.0 {
                    td.touch_active = value != 0;
                    if (*lib_dev).has_touch && !td.has_mt {
                        let slot = &mut td.mt_slots[0];
                        slot.active = value != 0;
                        slot.dirty = true;
                        if value != 0 {
                            slot.tracking_id = 0;
                        }
                    }
                    if td.touch_active {
                        td.touch_start_time = Some(Instant::now());
                        td.tap_emitted = false;
                        td.touch_fingers = td.active_slot_count().max(1) as u32;
                        td.last_x = None;
                        td.last_y = None;
                        // Pinch BEGIN if 2+ fingers just landed
                        if td.touch_fingers >= 2 && !td.pinch_active {
                            if let Some(dist) = td.primary_slot_distance() {
                                td.pinch_active = true;
                                td.pinch_base_dist = dist;
                                td.pinch_base_angle = td.primary_slot_angle();
                                td.pinch_fingers = td.touch_fingers as i32;
                                out.push_back(LibinputEvent {
                                    event_type:
                                        LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_BEGIN,
                                    payload: EventPayload::GesturePinchBegin(GestureEvent {
                                        time_usec: ts_usec,
                                        finger_count: td.pinch_fingers,
                                        dx: 0.0,
                                        dy: 0.0,
                                        scale: 1.0,
                                        angle: 0.0,
                                        cancelled: false,
                                    }),
                                    context: ctx,
                                    device: lib_dev,
                                });
                            }
                        }
                    } else {
                        // Finger lift — end pinch
                        if td.pinch_active {
                            td.pinch_active = false;
                            out.push_back(LibinputEvent {
                                event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_END,
                                payload: EventPayload::GesturePinchEnd(GestureEvent {
                                    time_usec: ts_usec,
                                    finger_count: td.pinch_fingers,
                                    dx: 0.0,
                                    dy: 0.0,
                                    scale: td
                                        .primary_slot_distance()
                                        .map(|d| {
                                            if td.pinch_base_dist > 0.0 {
                                                d / td.pinch_base_dist
                                            } else {
                                                1.0
                                            }
                                        })
                                        .unwrap_or(1.0),
                                    angle: td.primary_slot_angle() - td.pinch_base_angle,
                                    cancelled: false,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                        }

                        // Tap-to-click
                        if cfg_tap && !td.tap_emitted && !dwt_active {
                            if let Some(start) = td.touch_start_time {
                                if start.elapsed() < Duration::from_millis(250)
                                    && td.touch_fingers <= 1
                                {
                                    let pressed_count = press_seat_button(lib_dev);
                                    out.push_back(LibinputEvent {
                                        event_type:
                                            LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                        payload: EventPayload::PointerButton(PointerButtonEvent {
                                            time_usec: ts_usec,
                                            button: KeyCode::BTN_LEFT.0 as u32,
                                            state: 1,
                                            seat_button_count: pressed_count,
                                        }),
                                        context: ctx,
                                        device: lib_dev,
                                    });
                                    let released_count = release_seat_button(lib_dev);
                                    out.push_back(LibinputEvent {
                                        event_type:
                                            LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                        payload: EventPayload::PointerButton(PointerButtonEvent {
                                            time_usec: ts_usec,
                                            button: KeyCode::BTN_LEFT.0 as u32,
                                            state: 0,
                                            seat_button_count: released_count,
                                        }),
                                        context: ctx,
                                        device: lib_dev,
                                    });
                                }
                            }
                        }
                        td.last_x = None;
                        td.last_y = None;
                        td.current_dx = 0;
                        td.current_dy = 0;
                        td.touch_start_time = None;
                        td.touch_fingers = 0;
                    }
                } else if code == KeyCode::BTN_TOOL_DOUBLETAP.0 {
                    td.touch_fingers = if value != 0 { 2 } else { 1 };
                } else if code == KeyCode::BTN_TOOL_TRIPLETAP.0 {
                    td.touch_fingers = if value != 0 { 3 } else { 2 };
                } else if code == KeyCode::BTN_TOOL_QUADTAP.0 {
                    td.touch_fingers = if value != 0 { 4 } else { 3 };
                } else {
                    // Physical click with finger-count remapping
                    let mut mapped = code;
                    let mut event_device = lib_dev;
                    if code == KeyCode::BTN_LEFT.0 {
                        if value != 0 {
                            // If a release was lost (for example while another
                            // client held an EVIOCGRAB), preserve the existing
                            // logical press and suppress the duplicate press.
                            if td.active_click_button.is_some() {
                                return;
                            }
                            let (in_right_button_area, in_top_button_area) =
                                match (td.last_x, td.last_y, td.abs_x_range, td.abs_y_range) {
                                    (Some(x), Some(y), Some((xmin, xmax)), Some((ymin, ymax))) => {
                                        let right_edge = xmin + (xmax - xmin) * 2 / 3;
                                        let button_top = ymin + (ymax - ymin) * 4 / 5;
                                        let top_button_bottom = ymin + (ymax - ymin) / 5;
                                        (
                                            x >= right_edge && y >= button_top,
                                            td.is_topbuttonpad && y <= top_button_bottom,
                                        )
                                    }
                                    _ => (false, false),
                                };
                            if in_top_button_area {
                                if let (Some(x), Some((xmin, xmax))) = (td.last_x, td.abs_x_range) {
                                    let third = (xmax - xmin) / 3;
                                    mapped = if x >= xmin + third * 2 {
                                        KeyCode::BTN_RIGHT.0
                                    } else if x >= xmin + third {
                                        KeyCode::BTN_MIDDLE.0
                                    } else {
                                        KeyCode::BTN_LEFT.0
                                    };
                                }
                                if let Some(pointing_stick) = pointing_stick_device {
                                    event_device = pointing_stick;
                                }
                            } else if in_right_button_area || td.touch_fingers == 2 {
                                mapped = KeyCode::BTN_RIGHT.0;
                            } else if td.touch_fingers >= 3 {
                                mapped = KeyCode::BTN_MIDDLE.0;
                            }
                            td.active_click_button = Some(mapped);
                            td.active_click_device = Some(event_device);
                        } else {
                            let Some(active) = td.active_click_button.take() else {
                                return;
                            };
                            mapped = active;
                            event_device = td.active_click_device.take().unwrap_or(lib_dev);
                        }
                    }
                    if matches!(mapped,
                        c if c == KeyCode::BTN_LEFT.0
                          || c == KeyCode::BTN_RIGHT.0
                          || c == KeyCode::BTN_MIDDLE.0
                    ) {
                        let seat_button_count = if value != 0 {
                            press_seat_button(event_device)
                        } else {
                            release_seat_button(event_device)
                        };
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                            payload: EventPayload::PointerButton(PointerButtonEvent {
                                time_usec: ts_usec,
                                button: mapped as u32,
                                state: value as u32,
                                seat_button_count,
                            }),
                            context: ctx,
                            device: event_device,
                        });
                    }
                }
            }

            EventType::ABSOLUTE => {
                let code = ev.code();
                let val = ev.value();

                let expected_range = if code == AbsoluteAxisCode::ABS_X.0 {
                    td.abs_x_range
                } else if code == AbsoluteAxisCode::ABS_Y.0 {
                    td.abs_y_range
                } else {
                    None
                };
                if expected_range.is_some_and(|(minimum, maximum)| val < minimum || val > maximum)
                    && match td.axis_range_warning_at {
                        Some(last) => last.elapsed() >= Duration::from_secs(5 * 60),
                        None => true,
                    }
                {
                    td.axis_range_warning_at = Some(Instant::now());
                    crate::emit_info_log(ctx, "input value is outside expected range");
                }

                if code == AbsoluteAxisCode::ABS_MT_TRACKING_ID.0 {
                    td.protocol_a_tracking_id = Some(val);
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_X.0 {
                    td.protocol_a_x = Some(val as f64);
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_Y.0 {
                    td.protocol_a_y = Some(val as f64);
                }
                if td.protocol_a
                    && (code == AbsoluteAxisCode::ABS_MT_TRACKING_ID.0
                        || code == AbsoluteAxisCode::ABS_MT_POSITION_X.0
                        || code == AbsoluteAxisCode::ABS_MT_POSITION_Y.0)
                {
                    return;
                }

                // ---- MT slot tracking ----
                if code == AbsoluteAxisCode::ABS_MT_SLOT.0 {
                    let slot = val as usize;
                    if slot < td.mt_slots.len() {
                        td.current_slot = slot;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_TRACKING_ID.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        mt_slot.active = val >= 0;
                        mt_slot.tracking_id = val;
                        if val >= 0 {
                            mt_slot.palm_suppressed = mt_slot.tool_type == 2;
                            mt_slot.cancel_pending = false;
                        }
                        mt_slot.dirty = true;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_X.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        if !mt_slot.reported
                            || (mt_slot.x - val as f64).abs() > f64::from(td.mt_x_fuzz)
                        {
                            mt_slot.x = val as f64;
                            mt_slot.dirty = true;
                        }
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_Y.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        if !mt_slot.reported
                            || (mt_slot.y - val as f64).abs() > f64::from(td.mt_y_fuzz)
                        {
                            mt_slot.y = val as f64;
                            mt_slot.dirty = true;
                        }
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_DISTANCE.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        td.mt_slots[slot].distance = val as f64;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_TOOL_TYPE.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        mt_slot.tool_type = val;
                        if val == 2 {
                            mt_slot.cancel_pending |= mt_slot.active && mt_slot.reported;
                            mt_slot.palm_suppressed = true;
                            mt_slot.dirty = true;
                        }
                    }
                }
                // ---- Single-touch ABS_X/Y ----
                else if code == AbsoluteAxisCode::ABS_X.0 {
                    if (*lib_dev).has_touch && !td.has_mt {
                        td.mt_slots[0].x = val as f64;
                        td.mt_slots[0].dirty = true;
                    }
                    if let Some(last) = td.last_movement_time {
                        if last.elapsed() > Duration::from_millis(50) {
                            td.last_x = None;
                        }
                    }
                    td.last_movement_time = Some(Instant::now());
                    if let Some(px) = td.last_x {
                        td.current_dx += val - px;
                    }
                    td.last_x = Some(val);
                } else if code == AbsoluteAxisCode::ABS_Y.0 {
                    if (*lib_dev).has_touch && !td.has_mt {
                        td.mt_slots[0].y = val as f64;
                        td.mt_slots[0].dirty = true;
                    }
                    if let Some(last) = td.last_movement_time {
                        if last.elapsed() > Duration::from_millis(50) {
                            td.last_y = None;
                        }
                    }
                    td.last_movement_time = Some(Instant::now());
                    if let Some(py) = td.last_y {
                        td.current_dy += val - py;
                    }
                    td.last_y = Some(val);
                }
            }

            EventType::SYNCHRONIZATION => {
                if ev.code() == 2 {
                    td.protocol_a = true;
                    let tracking_id = td.protocol_a_tracking_id.take().unwrap_or(-1);
                    if let (Some(x), Some(y)) = (td.protocol_a_x.take(), td.protocol_a_y.take()) {
                        td.protocol_a_contacts.push((tracking_id, x, y));
                    }
                    td.protocol_a_x = None;
                    td.protocol_a_y = None;
                    return;
                }
                if ev.code() != 0 {
                    return;
                }

                if (*lib_dev).has_touch {
                    if td.protocol_a {
                        let mut matched = vec![false; td.mt_slots.len()];
                        let existing_slots: Vec<usize> = td
                            .mt_slots
                            .iter()
                            .enumerate()
                            .filter_map(|(index, slot)| slot.reported.then_some(index))
                            .collect();
                        let complete_frame = td.protocol_a_contacts.len() >= existing_slots.len();
                        for (contact_index, (tracking_id, x, y)) in
                            td.protocol_a_contacts.drain(..).enumerate()
                        {
                            let by_tracking_id = (tracking_id >= 0).then(|| {
                                td.mt_slots.iter().enumerate().position(|(index, slot)| {
                                    !matched[index]
                                        && slot.reported
                                        && slot.tracking_id == tracking_id
                                })
                            });
                            let by_order = complete_frame
                                .then(|| existing_slots.get(contact_index).copied())
                                .flatten()
                                .filter(|index| !matched[*index]);
                            let by_position = td
                                .mt_slots
                                .iter()
                                .enumerate()
                                .filter(|(index, slot)| !matched[*index] && slot.reported)
                                .min_by(|(_, a), (_, b)| {
                                    let da = (a.x - x).powi(2) + (a.y - y).powi(2);
                                    let db = (b.x - x).powi(2) + (b.y - y).powi(2);
                                    da.total_cmp(&db)
                                })
                                .map(|(index, _)| index);
                            let slot_index = by_tracking_id
                                .flatten()
                                .or(by_order)
                                .or(by_position)
                                .or_else(|| {
                                    td.mt_slots
                                        .iter()
                                        .enumerate()
                                        .position(|(index, slot)| !matched[index] && !slot.reported)
                                })
                                .unwrap_or(0);
                            matched[slot_index] = true;
                            let slot = &mut td.mt_slots[slot_index];
                            let changed = !slot.reported || slot.x != x || slot.y != y;
                            slot.active = true;
                            if tracking_id >= 0 {
                                slot.tracking_id = tracking_id;
                            }
                            slot.x = x;
                            slot.y = y;
                            slot.dirty |= changed;
                        }
                        for (index, slot) in td.mt_slots.iter_mut().enumerate() {
                            if slot.reported && !matched[index] {
                                slot.active = false;
                                slot.dirty = true;
                            }
                        }
                    }
                    let mut emitted = false;
                    for (slot_index, slot) in td.mt_slots.iter_mut().enumerate() {
                        if !slot.dirty {
                            continue;
                        }
                        if slot.cancel_pending {
                            slot.cancel_pending = false;
                            slot.reported = false;
                            let seat_slot = slot.seat_slot.take().unwrap_or(-1);
                            release_touch_seat_slot(ctx, seat_slot);
                            if !slot.active {
                                slot.palm_suppressed = false;
                            }
                            slot.dirty = false;
                            emitted = true;
                            out.push_back(LibinputEvent {
                                event_type: LibinputEventType::LIBINPUT_EVENT_TOUCH_CANCEL,
                                payload: EventPayload::TouchCancel(TouchEvent {
                                    time_usec: ts_usec,
                                    slot: slot_index as i32,
                                    seat_slot,
                                    x: slot.x,
                                    y: slot.y,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                            continue;
                        }
                        if slot.palm_suppressed {
                            if !slot.active {
                                slot.palm_suppressed = false;
                            }
                            slot.dirty = false;
                            continue;
                        }
                        let (event_type, payload) = if slot.active && !slot.reported {
                            slot.reported = true;
                            let seat_slot = allocate_touch_seat_slot(ctx);
                            slot.seat_slot = Some(seat_slot);
                            (
                                LibinputEventType::LIBINPUT_EVENT_TOUCH_DOWN,
                                EventPayload::TouchDown(TouchEvent {
                                    time_usec: ts_usec,
                                    slot: slot_index as i32,
                                    seat_slot,
                                    x: slot.x,
                                    y: slot.y,
                                }),
                            )
                        } else if slot.active {
                            (
                                LibinputEventType::LIBINPUT_EVENT_TOUCH_MOTION,
                                EventPayload::TouchMotion(TouchEvent {
                                    time_usec: ts_usec,
                                    slot: slot_index as i32,
                                    seat_slot: slot.seat_slot.unwrap_or(-1),
                                    x: slot.x,
                                    y: slot.y,
                                }),
                            )
                        } else if slot.reported {
                            slot.reported = false;
                            let seat_slot = slot.seat_slot.take().unwrap_or(-1);
                            release_touch_seat_slot(ctx, seat_slot);
                            (
                                LibinputEventType::LIBINPUT_EVENT_TOUCH_UP,
                                EventPayload::TouchUp(TouchEvent {
                                    time_usec: ts_usec,
                                    slot: slot_index as i32,
                                    seat_slot,
                                    x: slot.x,
                                    y: slot.y,
                                }),
                            )
                        } else {
                            slot.dirty = false;
                            continue;
                        };
                        slot.dirty = false;
                        emitted = true;
                        out.push_back(LibinputEvent {
                            event_type,
                            payload,
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    td.touch_active = td.mt_slots.iter().any(|slot| slot.active);
                    if emitted {
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_TOUCH_FRAME,
                            payload: EventPayload::TouchFrame { time_usec: ts_usec },
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    return;
                }

                // ---- Pinch UPDATE on SYN_REPORT ----
                if td.pinch_active && !dwt_active {
                    if let Some(dist) = td.primary_slot_distance() {
                        let scale = if td.pinch_base_dist > 0.0 {
                            dist / td.pinch_base_dist
                        } else {
                            1.0
                        };
                        let angle = td.primary_slot_angle() - td.pinch_base_angle;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_UPDATE,
                            payload: EventPayload::GesturePinchUpdate(GestureEvent {
                                time_usec: ts_usec,
                                finger_count: td.pinch_fingers,
                                dx: 0.0,
                                dy: 0.0,
                                scale,
                                angle,
                                cancelled: false,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    td.current_dx = 0;
                    td.current_dy = 0;
                    return;
                }

                let has_movement = td.current_dx != 0 || td.current_dy != 0;
                if dwt_active {
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.tap_emitted = true;
                    return;
                }
                if !has_movement {
                    return;
                }

                td.tap_emitted = true;
                let hw_scale: f32 = 0.18;
                let n_fingers = td.active_slot_count().max(td.touch_fingers as usize);

                if n_fingers <= 1 {
                    let total_x = (td.current_dx as f32 * hw_scale) * cfg_accel + td.remainder_x;
                    let total_y = (td.current_dy as f32 * hw_scale) * cfg_accel + td.remainder_y;
                    let emit_x = total_x.round() as i32;
                    let emit_y = total_y.round() as i32;
                    td.remainder_x = total_x - emit_x as f32;
                    td.remainder_y = total_y - emit_y as f32;
                    if emit_x != 0 || emit_y != 0 {
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION,
                            payload: EventPayload::PointerMotion(PointerMotionEvent {
                                time_usec: ts_usec,
                                dx: emit_x as f64,
                                dy: emit_y as f64,
                                dx_unaccel: (td.current_dx as f32 * hw_scale) as f64,
                                dy_unaccel: (td.current_dy as f32 * hw_scale) as f64,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                } else if n_fingers == 2 {
                    let scroll_scale: f32 = 0.02;
                    let total_y = td.current_dy as f32 * scroll_scale + td.remainder_y;
                    let total_x = td.current_dx as f32 * scroll_scale + td.remainder_x;
                    let emit_y = total_y.round() as i32;
                    let emit_x = total_x.round() as i32;
                    td.remainder_y = total_y - emit_y as f32;
                    td.remainder_x = total_x - emit_x as f32;
                    if emit_y != 0 {
                        let v = if cfg_nat { -emit_y } else { emit_y };
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_FINGER,
                            payload: EventPayload::PointerAxis(PointerAxisEvent {
                                time_usec: ts_usec,
                                axis: 0,
                                value: v as f64 * 15.0,
                                value_discrete: v,
                                value_v120: v as f64 * 120.0,
                                source: 2,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    if emit_x != 0 {
                        let v = if cfg_nat { -emit_x } else { emit_x };
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_FINGER,
                            payload: EventPayload::PointerAxis(PointerAxisEvent {
                                time_usec: ts_usec,
                                axis: 1,
                                value: v as f64 * 15.0,
                                value_discrete: v,
                                value_v120: v as f64 * 120.0,
                                source: 2,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                } else {
                    // 3+ fingers = swipe gesture
                    let gscale: f64 = 0.18;
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_UPDATE,
                        payload: EventPayload::GestureSwipeUpdate(GestureEvent {
                            time_usec: ts_usec,
                            finger_count: n_fingers as i32,
                            dx: td.current_dx as f64 * gscale,
                            dy: td.current_dy as f64 * gscale,
                            scale: 1.0,
                            angle: 0.0,
                            cancelled: false,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                }
                td.current_dx = 0;
                td.current_dy = 0;
            }
            _ => {}
        }
    }
}
