//! Evdev backend for the libinput-rs shared library.
//!
//! BackendState owns every open DeviceWrapper and provides a single
//! drain() call that reads all pending kernel events, applies
//! motion/scroll/DWT/tap/pinch/keyboard logic, and appends finished
//! LibinputEvents to a caller-supplied queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use evdev::{AbsoluteAxisCode, Device, EventType, InputEvent, KeyCode, RelativeAxisCode};
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

use crate::ffi_types::{
    BackendKind, EventPayload, GestureEvent, KeyboardKeyEvent, LibinputContext, LibinputDevice,
    LibinputEvent, LibinputEventType, LibinputTabletPadModeGroup, LibinputTabletTool,
    PointerAxisEvent, PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent,
    TabletPadEvent, TabletToolEvent, TouchEvent,
};

#[cfg(feature = "libwacom")]
#[link(name = "wacom")]
extern "C" {
    fn libwacom_database_new() -> *mut libc::c_void;
    fn libwacom_database_destroy(database: *mut libc::c_void);
    fn libwacom_new_from_path(
        database: *const libc::c_void,
        path: *const libc::c_char,
        fallback: libc::c_int,
        error: *mut libc::c_void,
    ) -> *mut libc::c_void;
    fn libwacom_destroy(device: *mut libc::c_void);
    fn libwacom_get_integration_flags(device: *const libc::c_void) -> libc::c_int;
    fn libwacom_new_from_usbid(
        database: *const libc::c_void,
        vendor_id: libc::c_int,
        product_id: libc::c_int,
        error: *mut libc::c_void,
    ) -> *mut libc::c_void;
    fn libwacom_get_num_buttons(device: *const libc::c_void) -> libc::c_int;
    fn libwacom_is_reversible(device: *const libc::c_void) -> libc::c_int;
    fn libwacom_get_button_evdev_code(
        device: *const libc::c_void,
        button: libc::c_char,
    ) -> libc::c_int;
    fn libwacom_stylus_get_for_id(
        database: *const libc::c_void,
        tool_id: libc::c_int,
    ) -> *const libc::c_void;
    fn libwacom_stylus_get_name(stylus: *const libc::c_void) -> *const libc::c_char;
    #[allow(dead_code)]
    fn libwacom_stylus_is_generic(stylus: *const libc::c_void) -> libc::c_int;
    fn libwacom_stylus_has_eraser(stylus: *const libc::c_void) -> libc::c_int;
    fn libwacom_stylus_get_eraser_type(stylus: *const libc::c_void) -> libc::c_int;
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_database_new() -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_database_destroy(_database: *mut libc::c_void) {}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_new_from_path(
    _database: *const libc::c_void,
    _path: *const libc::c_char,
    _fallback: libc::c_int,
    _error: *mut libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_destroy(_device: *mut libc::c_void) {}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_get_integration_flags(_device: *const libc::c_void) -> libc::c_int {
    0
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_new_from_usbid(
    _database: *const libc::c_void,
    _vendor_id: libc::c_int,
    _product_id: libc::c_int,
    _error: *mut libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_get_num_buttons(_device: *const libc::c_void) -> libc::c_int {
    0
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_is_reversible(_device: *const libc::c_void) -> libc::c_int {
    0
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_get_button_evdev_code(
    _device: *const libc::c_void,
    _button: libc::c_char,
) -> libc::c_int {
    0
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_stylus_get_for_id(
    _database: *const libc::c_void,
    _tool_id: libc::c_int,
) -> *const libc::c_void {
    std::ptr::null()
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_stylus_get_name(_stylus: *const libc::c_void) -> *const libc::c_char {
    std::ptr::null()
}

#[cfg(not(feature = "libwacom"))]
#[allow(dead_code)]
unsafe fn libwacom_stylus_is_generic(_stylus: *const libc::c_void) -> libc::c_int {
    0
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_stylus_has_eraser(_stylus: *const libc::c_void) -> libc::c_int {
    0
}

#[cfg(not(feature = "libwacom"))]
unsafe fn libwacom_stylus_get_eraser_type(_stylus: *const libc::c_void) -> libc::c_int {
    0
}

unsafe fn tablet_is_display_device(path: &std::path::Path) -> bool {
    const WACOM_DEVICE_INTEGRATED_DISPLAY: libc::c_int = 1 << 0;
    const WACOM_DEVICE_INTEGRATED_SYSTEM: libc::c_int = 1 << 1;

    let Ok(path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return true;
    };
    let database = libwacom_database_new();
    if database.is_null() {
        return true;
    }

    let device = libwacom_new_from_path(database, path.as_ptr(), 0, std::ptr::null_mut());
    let is_display = if device.is_null() {
        true
    } else {
        let flags = libwacom_get_integration_flags(device);
        libwacom_destroy(device);
        flags & (WACOM_DEVICE_INTEGRATED_DISPLAY | WACOM_DEVICE_INTEGRATED_SYSTEM) != 0
    };
    libwacom_database_destroy(database);
    is_display
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

/// Drop input records that were queued before libinput took ownership of a
/// device file description.  In particular, an `open_restricted` callback is
/// allowed to hand us a still-open descriptor that was previously used by a
/// removed device.  Those records belong to the former device lifecycle and
/// must not be replayed for the newly-added device.
///
/// This mirrors upstream's `evdev_drain_fd()` setup step.  The descriptor is
/// nonblocking by the time this is called, so a short read or a non-interrupt
/// error means there are no more records to discard.
fn discard_pending_input_events(fd: RawFd) {
    let mut events = [std::mem::MaybeUninit::<libc::input_event>::uninit(); 24];
    let bytes = std::mem::size_of_val(&events);
    loop {
        let read = unsafe { libc::read(fd, events.as_mut_ptr().cast(), bytes) };
        if read == bytes as libc::ssize_t
            || (read < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted)
        {
            continue;
        }
        break;
    }
}

// ---------------------------------------------------------------------------
// Multi-touch slot
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum EdgeScrollState {
    #[default]
    None,
    New,
    Active,
    Area,
}

const EDGE_RIGHT: u8 = 1;
const EDGE_BOTTOM: u8 = 2;
const AXIS_VERTICAL: u8 = 1;
const AXIS_HORIZONTAL: u8 = 2;

#[derive(Clone, Default)]
struct MtSlot {
    active: bool,
    reported: bool,
    dirty: bool,
    began_this_frame: bool,
    dwt_released_this_frame: bool,
    palm_suppressed: bool,
    generic_palm: Option<GenericPalmKind>,
    dwt_palm: Option<DwtPalmKind>,
    dwt_palm_started_usec: Option<u64>,
    cancel_pending: bool,
    button_area_classification_pending: bool,
    button_area_excluded: bool,
    button_area_initial_x: f64,
    button_area_initial_y: f64,
    button_area_down_usec: u64,
    gesture_initial_x: f64,
    gesture_initial_y: f64,
    gesture_rebase_pending: bool,
    gesture_position_seen: bool,
    edge_classification_pending: bool,
    edge_state: EdgeScrollState,
    edge_mask: u8,
    edge_initial_x: f64,
    edge_initial_y: f64,
    edge_last_x: f64,
    edge_last_y: f64,
    edge_started_at: Option<Instant>,
    edge_axis: u8,
    seat_slot: Option<i32>,
    tracking_id: i32,
    tool_type: i32,
    x: f64,
    y: f64,
    pressure: f64,
    touch_major: f64,
    touch_minor: f64,
    distance: f64, // ABS_MT_DISTANCE (for stylus / palm)
}

/// Why a touch contact was ignored by the touchpad's typing/trackpoint palm
/// detection. Keeping this separate from the generic palm flag is essential:
/// a contact may be released after the relevant activity timer expires only
/// when it began after the last keyboard or trackpoint event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DwtPalmKind {
    Typing,
    Trackpoint,
}

/// Palm classifications that remain in effect for the lifetime of a
/// contact. They are intentionally distinct from the timeout-based DWT/DWTP
/// state, which may be released by a later contact update.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GenericPalmKind {
    Pressure,
    Size,
}

#[derive(Default)]
struct DwtRuntime {
    paired_keyboards: HashSet<RawFd>,
    pressed_keys: HashSet<u16>,
    modifier_keys: HashSet<u16>,
    active: bool,
    deadline: Option<Instant>,
    active_started_usec: Option<u64>,
    deadline_usec: Option<u64>,
    last_key_press_usec: Option<u64>,
    event_clock_anchor_at: Option<Instant>,
    event_clock_anchor_usec: Option<u64>,
}

#[derive(Default)]
struct DwtpRuntime {
    paired_trackpoint: Option<RawFd>,
    event_count: u32,
    active: bool,
    deadline: Option<Instant>,
    active_started_usec: Option<u64>,
    deadline_usec: Option<u64>,
    last_event_usec: Option<u64>,
}

fn estimate_event_time(
    anchor_at: Option<Instant>,
    anchor_usec: Option<u64>,
    now: Instant,
) -> Option<u64> {
    let (Some(anchor_at), Some(anchor_usec)) = (anchor_at, anchor_usec) else {
        return None;
    };
    Some(anchor_usec.saturating_add(now.saturating_duration_since(anchor_at).as_micros() as u64))
}

#[derive(Clone, Copy)]
struct DwtPairingInfo {
    fd: RawFd,
    is_touchpad: bool,
    touchpad_is_external: bool,
    is_dwt_keyboard: bool,
    keyboard_internal: bool,
    is_pointing_stick: bool,
    vendor_id: u16,
    product_id: u16,
    bus_type: u16,
}

enum DwtFollowup {
    Keyboard {
        source_fd: RawFd,
        code: u16,
        value: i32,
        at: Instant,
        time_usec: u64,
    },
    TrackpointActivity {
        source_fd: RawFd,
        at: Instant,
        time_usec: u64,
    },
}

fn dwt_key_is_shift(code: u16) -> bool {
    matches!(code, 42 | 54) // KEY_LEFTSHIFT, KEY_RIGHTSHIFT
}

fn dwt_key_is_modifier(code: u16) -> bool {
    matches!(
        code,
        29 | 97 // Ctrl
            | 56 | 100 // Alt
            | 42 | 54 // Shift
            | 464 // Fn
            | 58 // Caps Lock
            | 15 // Tab
            | 127 // Compose
            | 125 | 126 // Meta
    )
}

fn dwt_key_is_ignored(code: u16) -> bool {
    if dwt_key_is_modifier(code) {
        return false;
    }
    // KEY_ESC, KEY_KPASTERISK, then the complete F-key/media/numpad range.
    matches!(code, 1 | 55) || code >= 59
}

#[derive(Clone)]
struct HeldKey {
    code: u16,
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
    is_dwt_keyboard: bool,
    keyboard_internal: bool,
    is_pointer: bool,
    is_touchpad: bool,
    touchpad_is_external: bool,
    is_clickpad: bool,
    is_topbuttonpad: bool,
    is_pointing_stick: bool,
    vendor_id: u16,
    product_id: u16,
    bus_type: u16,
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
    finger_scroll_axes: u8,
    finger_scroll_constraint: FingerScrollConstraint,

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
    touch_arbitration_suppressed: bool,
    touch_arbitration_tablet_was_active: bool,
    touchpad_left_handed_applied: bool,
    touchpad_left_handed_sequence_active: bool,
    palm_pressure_threshold: Option<f64>,
    palm_size_threshold: Option<f64>,
    tap_emitted: bool,
    tap_button_down: Option<u16>,
    tap_release_since: Option<Instant>,
    tap_drag_active: bool,
    tap_fingers: u32,
    touch_start_time: Option<Instant>,
    last_movement_time: Option<Instant>,
    active_click_button: Option<u16>,
    active_click_device: Option<*mut LibinputDevice>,

    // --- tablet tool ---
    tablet_serial: u64,
    tablet_tool_id: u64,
    tablet_tool_type: u32,
    tablet_active_tool_keys: Vec<u16>,
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
    tablet_last_event_x: f64,
    tablet_last_event_y: f64,
    tablet_last_event_pressure: f64,
    tablet_last_event_distance: f64,
    tablet_last_event_tilt_x: f64,
    tablet_last_event_tilt_y: f64,
    tablet_last_event_rotation: f64,
    tablet_last_event_slider: f64,
    tablet_x_changed: bool,
    tablet_y_changed: bool,
    tablet_axes_changed: bool,
    tablet_pressure: f64,
    tablet_pressure_range: Option<(i32, i32)>,
    tablet_pressure_changed: bool,
    tablet_pressure_offset: Option<f64>,
    tablet_pressure_offset_candidate: Option<f64>,
    tablet_pressure_proximity_samples: u8,
    tablet_pressure_offset_rejected: bool,
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
    tablet_size_major: f64,
    tablet_size_minor: f64,
    tablet_size_major_resolution: i32,
    tablet_size_minor_resolution: i32,
    tablet_last_event_size_major: f64,
    tablet_last_event_size_minor: f64,
    tablet_size_major_changed: bool,
    tablet_size_minor_changed: bool,
    tablet_wheel_delta: f64,
    tablet_wheel_discrete: i32,
    tablet_wheel_changed: bool,
    tablet_tip_down: bool,
    tablet_tip_pending: Option<bool>,
    tablet_touch_button_changed: bool,
    tablet_area_sequence_suppressed: bool,
    tablet_left_handed_applied: bool,
    tablet_cursor_out_of_range: bool,
    tablet_eraser_button_active: bool,
    tablet_eraser_pen_out_since: Option<Instant>,
    tablet_eraser_pending_tip_up: bool,
    tablet_zero_pressure_since: Option<Instant>,
    tablet_proximity_timer_enabled: bool,
    tablet_buttons_down: u32,
    tablet_held_buttons: Vec<u32>,
    tablet_ignored_initial_buttons: Vec<u32>,
    tablet_pending_button_events: Vec<(u32, bool)>,

    // --- tablet pad ---
    pad_ring_ranges: [Option<(i32, i32)>; 2],
    pad_strip_ranges: [Option<(i32, i32)>; 2],
    pad_ring_values: [i32; 2],
    pad_strip_values: [i32; 2],
    pad_changed_axes: u8,
    pad_abs_misc_terminator: bool,
    pad_dial_values: [Option<f64>; 2],

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
    swipe_active: bool,
    swipe_fingers: i32,
    mt_contact_count_changed: bool,
    gesture_fingers_last: usize,
    pinch_base_centroid: Option<(f64, f64)>,
    gesture_last_centroid: Option<(f64, f64)>,
    hold_started_at: Option<Instant>,
    hold_active: bool,
    hold_fingers: i32,
    hold_blocked: bool,
    hold_contact_changed: bool,
    drag_3fg_candidate_since: Option<Instant>,
    drag_3fg_candidate_time_usec: u64,
    drag_3fg_active: bool,
    drag_3fg_button_down: bool,
    drag_3fg_release_since: Option<Instant>,

    // --- keyboard repeat ---
    held_keys: Vec<HeldKey>,
    held_buttons: Vec<u16>,

    // --- per-touchpad typing and pointing-stick suppression ---
    dwt: DwtRuntime,
    dwtp: DwtpRuntime,

    // libinput device pointer (context owns the allocation)
    lib_device: *mut LibinputDevice,
}

unsafe impl Send for TrackedDevice {}

#[derive(Default)]
struct FingerScrollConstraint {
    active_horizontal: bool,
    active_vertical: bool,
    vector_x_mm: f64,
    vector_y_mm: f64,
    last_motion_usec: Option<u64>,
    horizontal_duration_usec: u64,
    vertical_duration_usec: u64,
}

impl FingerScrollConstraint {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn constrain(
        &mut self,
        time_usec: u64,
        mut scroll_x: f64,
        mut scroll_y: f64,
        delta_x_mm: f64,
        delta_y_mm: f64,
    ) -> (f64, f64) {
        const ACTIVE_THRESHOLD_USEC: u64 = 100_000;
        const INACTIVE_THRESHOLD_USEC: u64 = 50_000;
        const EVENT_TIMEOUT_USEC: u64 = 100_000;
        const DEGREE_75: f64 = 3.73;
        const DEGREE_60: f64 = 1.73;
        const DEGREE_30: f64 = 0.57;
        const DEGREE_15: f64 = 0.27;
        const MIN_VECTOR_MM: f64 = 0.15;

        if self.active_horizontal && self.active_vertical {
            return (scroll_x, scroll_y);
        }

        let time_delta_usec = self
            .last_motion_usec
            .map(|previous| time_usec.saturating_sub(previous))
            .filter(|delta| *delta <= EVENT_TIMEOUT_USEC)
            .unwrap_or(0);
        self.last_motion_usec = Some(time_usec);

        let vector_decay = if time_delta_usec == 0 {
            0.0
        } else {
            let delta = time_delta_usec as f64;
            if delta <= EVENT_TIMEOUT_USEC as f64 * 0.33 {
                ((EVENT_TIMEOUT_USEC as f64 / 2.0) - delta) / (EVENT_TIMEOUT_USEC as f64 / 2.0)
            } else {
                (EVENT_TIMEOUT_USEC as f64 - delta) / (EVENT_TIMEOUT_USEC as f64 * 2.0)
            }
        };
        self.vector_x_mm = self.vector_x_mm * vector_decay + delta_x_mm;
        self.vector_y_mm = self.vector_y_mm * vector_decay + delta_y_mm;
        let vector_length = self.vector_x_mm.hypot(self.vector_y_mm);
        let slope = if self.vector_x_mm != 0.0 {
            (self.vector_y_mm / self.vector_x_mm).abs()
        } else {
            f64::INFINITY
        };

        if slope >= DEGREE_30 && vector_length > MIN_VECTOR_MM {
            self.vertical_duration_usec = self
                .vertical_duration_usec
                .saturating_add(time_delta_usec)
                .min(ACTIVE_THRESHOLD_USEC);
            if slope >= DEGREE_75 {
                self.horizontal_duration_usec = self
                    .horizontal_duration_usec
                    .saturating_sub(time_delta_usec);
            }
        }
        if slope < DEGREE_60 && vector_length > MIN_VECTOR_MM {
            self.horizontal_duration_usec = self
                .horizontal_duration_usec
                .saturating_add(time_delta_usec)
                .min(ACTIVE_THRESHOLD_USEC);
            if slope < DEGREE_15 {
                self.vertical_duration_usec =
                    self.vertical_duration_usec.saturating_sub(time_delta_usec);
            }
        }

        if self.horizontal_duration_usec == ACTIVE_THRESHOLD_USEC {
            self.active_horizontal = true;
            if self.vertical_duration_usec < INACTIVE_THRESHOLD_USEC {
                self.active_vertical = false;
            }
        }
        if self.vertical_duration_usec == ACTIVE_THRESHOLD_USEC {
            self.active_vertical = true;
            if self.horizontal_duration_usec < INACTIVE_THRESHOLD_USEC {
                self.active_horizontal = false;
            }
        }

        if vector_length > 5.0 && (DEGREE_30..DEGREE_60).contains(&slope) {
            self.active_horizontal = true;
            self.active_vertical = true;
        }

        if !self.active_horizontal && self.active_vertical {
            scroll_x = 0.0;
        }
        if self.active_horizontal && !self.active_vertical {
            scroll_y = 0.0;
        }
        if !self.active_horizontal && !self.active_vertical {
            if slope >= DEGREE_60 {
                scroll_x = 0.0;
            }
            if slope < DEGREE_30 {
                scroll_y = 0.0;
            }
        }

        (scroll_x, scroll_y)
    }
}

#[derive(Default)]
struct EdgeScrollFrame {
    scrolls: Vec<(u8, f64)>,
    stop_axes: u8,
    suppress_normal_motion: bool,
}

fn touchpad_delta_mm(
    raw_delta: f64,
    resolution: Option<i32>,
    range: Option<(i32, i32)>,
    size_mm: Option<f64>,
) -> f64 {
    if let Some(resolution) = resolution.filter(|resolution| *resolution > 0) {
        return raw_delta / f64::from(resolution);
    }
    if let (Some((minimum, maximum)), Some(size_mm)) = (range, size_mm) {
        let span = f64::from(maximum) - f64::from(minimum);
        if span > 0.0 && size_mm.is_finite() && size_mm > 0.0 {
            return raw_delta * size_mm / span;
        }
    }
    raw_delta
}

/// Return the physical-motion octants used by libinput's gesture
/// disambiguation. Small vectors deliberately cover the neighbouring octants
/// as well, matching the tolerance used for noisy touchpad hardware.
fn gesture_direction_mask(x: f64, y: f64) -> u8 {
    const N: u8 = 1 << 0;
    const NE: u8 = 1 << 1;
    const E: u8 = 1 << 2;
    const SE: u8 = 1 << 3;
    const S: u8 = 1 << 4;
    const SW: u8 = 1 << 5;
    const W: u8 = 1 << 6;
    const NW: u8 = 1 << 7;

    if x.abs() < 2.0 && y.abs() < 2.0 {
        return if x > 0.0 && y > 0.0 {
            S | SE | E
        } else if x > 0.0 && y < 0.0 {
            N | NE | E
        } else if x < 0.0 && y > 0.0 {
            S | SW | W
        } else if x < 0.0 && y < 0.0 {
            N | NW | W
        } else if x > 0.0 {
            NE | E | SE
        } else if x < 0.0 {
            NW | W | SW
        } else if y > 0.0 {
            SE | S | SW
        } else if y < 0.0 {
            NE | N | NW
        } else {
            u8::MAX
        };
    }

    let angle = (y.atan2(x) + 2.5 * std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI)
        * 4.0
        / std::f64::consts::PI;
    let primary = (angle + 0.9) as u32 % 8;
    let adjacent = (angle + 0.1) as u32 % 8;
    (1 << primary) | (1 << adjacent)
}

fn gesture_directions_overlap(first: u8, second: u8) -> bool {
    ((first | (first >> 1)) & second) != 0
        || ((second | (second >> 1)) & first) != 0
        || (first & (1 << 7) != 0 && second & 1 != 0)
        || (second & (1 << 7) != 0 && first & 1 != 0)
}

#[derive(Default)]
struct DebounceButton {
    delivered_down: bool,
    window_since: Option<Instant>,
    pending_down: Option<bool>,
    pending_since: Option<Instant>,
}

impl TrackedDevice {
    fn dwt_pairing_info(&self, fd: RawFd) -> DwtPairingInfo {
        DwtPairingInfo {
            fd,
            is_touchpad: self.is_touchpad,
            touchpad_is_external: self.touchpad_is_external,
            is_dwt_keyboard: self.is_dwt_keyboard,
            keyboard_internal: self.keyboard_internal,
            is_pointing_stick: self.is_pointing_stick,
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            bus_type: self.bus_type,
        }
    }

    unsafe fn dwt_palm_for_new_touch(&self, time_usec: u64) -> Option<DwtPalmKind> {
        let typing_active_at_touch = self.dwt.active
            && (time_usec == 0
                || (match self.dwt.active_started_usec {
                    Some(started) => time_usec >= started,
                    None => true,
                } && match self.dwt.deadline_usec {
                    Some(deadline) => time_usec <= deadline,
                    None => true,
                }));
        let trackpoint_active_at_touch = self.dwtp.active
            && (time_usec == 0
                || (match self.dwtp.active_started_usec {
                    Some(started) => time_usec >= started,
                    None => true,
                } && match self.dwtp.deadline_usec {
                    Some(deadline) => time_usec <= deadline,
                    None => true,
                }));
        // Configuration changes are non-retroactive: an already-active DWT
        // interval continues to classify contacts even if it is disabled.
        if typing_active_at_touch {
            Some(DwtPalmKind::Typing)
        } else if trackpoint_active_at_touch {
            Some(DwtPalmKind::Trackpoint)
        } else {
            None
        }
    }

    fn mark_dwt_palm(slot: &mut MtSlot, palm: Option<DwtPalmKind>, time_usec: u64) {
        let Some(palm) = palm else { return };
        slot.dwt_palm = Some(palm);
        slot.dwt_palm_started_usec = (time_usec != 0).then_some(time_usec);
        slot.palm_suppressed = true;
    }

    /// Re-enable only contacts that started after the final relevant input
    /// event. This intentionally preserves upstream's long-standing
    /// distinction between a resting palm and a fresh touch after typing.
    fn release_expired_dwt_palms(&mut self) {
        let keyboard_active = self.dwt.active;
        let keyboard_last_press = self.dwt.last_key_press_usec;
        let trackpoint_active = self.dwtp.active;
        let trackpoint_last_event = self.dwtp.last_event_usec;
        let mut released = false;

        for slot in &mut self.mt_slots {
            if !slot.active || !slot.dirty {
                continue;
            }
            let last_activity = match slot.dwt_palm {
                Some(DwtPalmKind::Typing) if !keyboard_active => keyboard_last_press,
                Some(DwtPalmKind::Trackpoint) if !trackpoint_active => trackpoint_last_event,
                _ => continue,
            };
            let started_after_activity = match (slot.dwt_palm_started_usec, last_activity) {
                (Some(started), Some(activity)) if started != 0 && activity != 0 => {
                    started > activity
                }
                // An unknown timestamp is handled like upstream's zero
                // palm timestamp: release it rather than leave it stuck.
                _ => true,
            };
            if !started_after_activity {
                continue;
            }
            slot.dwt_palm = None;
            slot.dwt_palm_started_usec = None;
            slot.dwt_released_this_frame = true;
            slot.palm_suppressed = slot.generic_palm.is_some()
                || slot.tool_type == 2
                || self.touch_arbitration_suppressed;
            slot.gesture_rebase_pending = false;
            slot.gesture_position_seen = false;
            released = true;
        }

        if released {
            self.mt_contact_count_changed = true;
        }
    }

    /// Apply palm classifications that cannot be released until the contact
    /// lifts. Upstream checks pressure before (and after) DWT so a contact
    /// suppressed while typing can transition to a permanent pressure or
    /// touch-size palm when the typing timeout expires.
    fn refresh_generic_palms(&mut self) {
        let pressure_threshold = self.palm_pressure_threshold;
        let size_threshold = self.palm_size_threshold;
        let mut palm_detected = false;

        for slot in &mut self.mt_slots {
            if !slot.active {
                continue;
            }
            // Pressure and size are evaluated before DWT for a fresh touch,
            // but after DWT for subsequent updates. This preserves the
            // upstream priority ordering while keeping a typing palm intact
            // until a later post-timeout update can release it.
            let began_this_frame = std::mem::take(&mut slot.began_this_frame);
            let dwt_released_this_frame = std::mem::take(&mut slot.dwt_released_this_frame);
            if slot.generic_palm.is_some() {
                slot.palm_suppressed = true;
                continue;
            }
            // A high pressure value is checked before DWT for a new touch.
            // Touch size, however, is intentionally checked after DWT and
            // may only take effect once the typing/trackpoint palm releases.
            if slot.dwt_palm.is_some() {
                if began_this_frame
                    && pressure_threshold.is_some_and(|threshold| slot.pressure > threshold)
                {
                    // Pressure is evaluated before DWT for a new contact.
                    // Do not retain a timeout palm that upstream never set.
                    slot.dwt_palm = None;
                    slot.dwt_palm_started_usec = None;
                    slot.generic_palm = Some(GenericPalmKind::Pressure);
                    slot.palm_suppressed = true;
                    palm_detected = true;
                }
                continue;
            }
            if slot.tool_type == 2 || self.touch_arbitration_suppressed {
                continue;
            }

            let pressure_palm =
                pressure_threshold.is_some_and(|threshold| slot.pressure > threshold);
            let size_palm = size_threshold.is_some_and(|threshold| {
                slot.touch_major > threshold || slot.touch_minor > threshold
            });
            let palm = if dwt_released_this_frame {
                size_palm
                    .then_some(GenericPalmKind::Size)
                    .or_else(|| pressure_palm.then_some(GenericPalmKind::Pressure))
            } else if pressure_palm {
                Some(GenericPalmKind::Pressure)
            } else if size_palm {
                Some(GenericPalmKind::Size)
            } else {
                None
            };
            if let Some(palm) = palm {
                slot.generic_palm = Some(palm);
                slot.palm_suppressed = true;
                palm_detected = true;
            }
        }

        if palm_detected {
            // A palm must never produce a deferred tap after it lifts.
            self.tap_emitted = true;
            // Rebase gesture/scroll state when an established eligible
            // contact turns into a palm, just as a contact-count change does.
            self.mt_contact_count_changed = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        device: Device,
        restricted_fd: Option<RawFd>,
        path: PathBuf,
        lib_device: *mut LibinputDevice,
        uses_absolute_events: bool,
        is_absolute_pointer: bool,
        is_keyboard: bool,
        is_dwt_keyboard: bool,
        keyboard_internal: bool,
        is_pointer: bool,
        is_touchpad: bool,
        touchpad_is_external: bool,
        palm_pressure_threshold: Option<u32>,
        palm_size_threshold: Option<u32>,
        is_clickpad: bool,
        is_topbuttonpad: bool,
        is_pointing_stick: bool,
        vendor_id: u16,
        product_id: u16,
        bus_type: u16,
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
        let tablet_buttons: Vec<u32> = device
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
        let mut initial_tablet_tool_type = {
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
        let initial_tablet_buttons = {
            use std::os::fd::AsRawFd;
            let mut key_bits = [0_u8; 96];
            let request =
                ((2_u64 << 30) | ((key_bits.len() as u64) << 16) | ((b'E' as u64) << 8) | 0x18)
                    as libc::c_ulong;
            if unsafe { libc::ioctl(device.as_raw_fd(), request, key_bits.as_mut_ptr()) } >= 0 {
                tablet_buttons
                    .iter()
                    .copied()
                    .filter(|code| {
                        key_bits
                            .get((*code as usize) / 8)
                            .is_some_and(|byte| byte & (1 << (*code % 8)) != 0)
                    })
                    .collect()
            } else {
                Vec::new()
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
        let mut tablet_size_major = 0.0;
        let mut tablet_size_minor = 0.0;
        let mut tablet_size_major_resolution = 0;
        let mut tablet_size_minor_resolution = 0;
        let mut current_mt_tracking_id = -1;
        let mut pad_ring_ranges = [None; 2];
        let mut pad_strip_ranges = [None; 2];
        let mut pad_ring_values = [0; 2];
        let mut pad_strip_values = [0; 2];
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
                    tablet_tilt_x_info = Some((info.minimum(), info.maximum(), info.resolution()));
                } else if axis == AbsoluteAxisCode::ABS_TILT_Y {
                    tablet_tilt_y = f64::from(info.value());
                    tablet_tilt_y_info = Some((info.minimum(), info.maximum(), info.resolution()));
                } else if axis == AbsoluteAxisCode::ABS_Z
                    || axis == AbsoluteAxisCode::ABS_MT_ORIENTATION
                {
                    tablet_rotation = f64::from(info.value());
                    tablet_rotation_info = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_WHEEL {
                    tablet_slider = f64::from(info.value());
                    tablet_slider_range = Some((info.minimum(), info.maximum()));
                } else if axis == AbsoluteAxisCode::ABS_MT_TOUCH_MAJOR {
                    tablet_size_major = f64::from(info.value());
                    tablet_size_major_resolution = info.resolution();
                } else if axis == AbsoluteAxisCode::ABS_MT_TOUCH_MINOR {
                    tablet_size_minor = f64::from(info.value());
                    tablet_size_minor_resolution = info.resolution();
                } else if axis == AbsoluteAxisCode::ABS_MT_TRACKING_ID {
                    has_mt = true;
                    current_mt_tracking_id = info.value();
                } else if axis == AbsoluteAxisCode::ABS_MT_SLOT {
                    has_mt_slot = true;
                    mt_slot_count = (info.maximum() - info.minimum() + 1).clamp(1, 256) as usize;
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    mt_x_fuzz = info.fuzz();
                } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                    mt_y_fuzz = info.fuzz();
                }
                if axis == AbsoluteAxisCode::ABS_WHEEL {
                    pad_ring_ranges[0] = Some((info.minimum(), info.maximum()));
                    pad_ring_values[0] = info.value();
                } else if axis == AbsoluteAxisCode::ABS_THROTTLE {
                    pad_ring_ranges[1] = Some((info.minimum(), info.maximum()));
                    pad_ring_values[1] = info.value();
                } else if axis == AbsoluteAxisCode::ABS_RX {
                    pad_strip_ranges[0] = Some((info.minimum(), info.maximum()));
                    pad_strip_values[0] = info.value();
                } else if axis == AbsoluteAxisCode::ABS_RY {
                    pad_strip_ranges[1] = Some((info.minimum(), info.maximum()));
                    pad_strip_values[1] = info.value();
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
                if unsafe { (*lib_device).has_tablet } && tablet_has_size {
                    current_abs_x = Some(mt_slots[0].x as i32);
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_POSITION_Y) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.y = value as f64;
                }
                if unsafe { (*lib_device).has_tablet } && tablet_has_size {
                    current_abs_y = Some(mt_slots[0].y as i32);
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_PRESSURE) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.pressure = value as f64;
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_TOUCH_MAJOR) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.touch_major = value as f64;
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_TOUCH_MINOR) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.touch_minor = value as f64;
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_TOOL_TYPE) {
                for (slot, value) in mt_slots.iter_mut().zip(values) {
                    slot.tool_type = value;
                }
            }
            if let Some(values) = query_slots(AbsoluteAxisCode::ABS_MT_TRACKING_ID) {
                current_mt_tracking_id = values[0];
            }
        }
        if unsafe { (*lib_device).has_tablet } && tablet_has_size && current_mt_tracking_id >= 0 {
            initial_tablet_tool_type = Some(8);
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
            is_dwt_keyboard,
            keyboard_internal,
            is_pointer,
            is_touchpad,
            touchpad_is_external,
            is_clickpad,
            is_topbuttonpad,
            is_pointing_stick,
            vendor_id,
            product_id,
            bus_type,
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
            finger_scroll_axes: 0,
            finger_scroll_constraint: FingerScrollConstraint::default(),
            touch_active: false,
            touch_fingers: 0,
            last_x: None,
            last_y: None,
            current_dx: 0,
            current_dy: 0,
            abs_x_range,
            abs_y_range,
            axis_range_warning_at: None,
            touch_arbitration_suppressed: false,
            touch_arbitration_tablet_was_active: false,
            touchpad_left_handed_applied: false,
            touchpad_left_handed_sequence_active: false,
            palm_pressure_threshold: palm_pressure_threshold.map(f64::from),
            palm_size_threshold: palm_size_threshold.map(f64::from),
            tap_emitted: false,
            tap_button_down: None,
            tap_release_since: None,
            tap_drag_active: false,
            tap_fingers: 0,
            touch_start_time: None,
            last_movement_time: None,
            active_click_button: None,
            active_click_device: None,
            tablet_serial: 0,
            tablet_tool_id: 0,
            tablet_tool_type: initial_tablet_tool_type.unwrap_or(1),
            tablet_active_tool_keys: initial_tablet_tool_type
                .and_then(|tool_type| match tool_type {
                    2 => Some(KeyCode::BTN_TOOL_RUBBER.0),
                    3 => Some(KeyCode::BTN_TOOL_BRUSH.0),
                    4 => Some(KeyCode::BTN_TOOL_PENCIL.0),
                    5 => Some(KeyCode::BTN_TOOL_AIRBRUSH.0),
                    6 => Some(KeyCode::BTN_TOOL_MOUSE.0),
                    7 => Some(KeyCode::BTN_TOOL_LENS.0),
                    8 => None,
                    _ => Some(KeyCode::BTN_TOOL_PEN.0),
                })
                .into_iter()
                .collect(),
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
            tablet_last_event_x: current_abs_x.unwrap_or_default() as f64,
            tablet_last_event_y: current_abs_y.unwrap_or_default() as f64,
            tablet_last_event_pressure: tablet_pressure,
            tablet_last_event_distance: tablet_distance,
            tablet_last_event_tilt_x: tablet_tilt_x,
            tablet_last_event_tilt_y: tablet_tilt_y,
            tablet_last_event_rotation: tablet_rotation,
            tablet_last_event_slider: tablet_slider,
            tablet_x_changed: false,
            tablet_y_changed: false,
            tablet_axes_changed: false,
            tablet_pressure,
            tablet_pressure_range,
            tablet_pressure_changed: false,
            tablet_pressure_offset: None,
            tablet_pressure_offset_candidate: None,
            tablet_pressure_proximity_samples: 0,
            tablet_pressure_offset_rejected: false,
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
            tablet_size_major,
            tablet_size_minor,
            tablet_size_major_resolution,
            tablet_size_minor_resolution,
            tablet_last_event_size_major: tablet_size_major,
            tablet_last_event_size_minor: tablet_size_minor,
            tablet_size_major_changed: false,
            tablet_size_minor_changed: false,
            tablet_wheel_delta: 0.0,
            tablet_wheel_discrete: 0,
            tablet_wheel_changed: false,
            tablet_tip_down: initial_tablet_tool_type == Some(8),
            tablet_tip_pending: (initial_tablet_tool_type == Some(8)).then_some(true),
            tablet_touch_button_changed: false,
            tablet_area_sequence_suppressed: false,
            tablet_left_handed_applied: false,
            tablet_cursor_out_of_range: false,
            tablet_eraser_button_active: false,
            tablet_eraser_pen_out_since: None,
            tablet_eraser_pending_tip_up: false,
            tablet_zero_pressure_since: None,
            tablet_proximity_timer_enabled: true,
            tablet_buttons_down: 0,
            tablet_held_buttons: Vec::new(),
            tablet_ignored_initial_buttons: initial_tablet_buttons,
            tablet_pending_button_events: Vec::new(),
            pad_ring_ranges,
            pad_strip_ranges,
            pad_ring_values,
            pad_strip_values,
            pad_changed_axes: 0,
            pad_abs_misc_terminator: false,
            pad_dial_values: [None; 2],
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
            swipe_active: false,
            swipe_fingers: 0,
            mt_contact_count_changed: false,
            gesture_fingers_last: 0,
            pinch_base_centroid: None,
            gesture_last_centroid: None,
            hold_started_at: None,
            hold_active: false,
            hold_fingers: 0,
            hold_blocked: false,
            hold_contact_changed: false,
            drag_3fg_candidate_since: None,
            drag_3fg_candidate_time_usec: 0,
            drag_3fg_active: false,
            drag_3fg_button_down: false,
            drag_3fg_release_since: None,
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
            dwt: DwtRuntime::default(),
            dwtp: DwtpRuntime::default(),
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

    /// Whether a contact participates in pointer-motion and gesture state.
    ///
    /// Edge-scroll contacts stay outside that state until they are classified
    /// into the regular touch area. This keeps an edge-scroll timeout from
    /// arming an unrelated hold gesture.
    fn gesture_slot_eligible(slot: &MtSlot, button_areas: bool) -> bool {
        slot.active
            && !slot.palm_suppressed
            && slot.edge_state == EdgeScrollState::Area
            && (!button_areas || !slot.button_area_excluded)
    }

    fn gesture_tracked_contact_count(&self, button_areas: bool) -> usize {
        self.mt_slots
            .iter()
            .filter(|slot| Self::gesture_slot_eligible(slot, button_areas))
            .count()
    }

    fn gesture_finger_count(&self, button_areas: bool) -> usize {
        let active = self.active_slot_count();
        let eligible = self.gesture_tracked_contact_count(button_areas);
        let untracked = (self.touch_fingers as usize).saturating_sub(active);
        eligible + untracked
    }

    fn gesture_centroid(&self, button_areas: bool) -> Option<(f64, f64)> {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut count = 0usize;
        for slot in &self.mt_slots {
            if Self::gesture_slot_eligible(slot, button_areas) {
                x += slot.x;
                y += slot.y;
                count += 1;
            }
        }
        (count != 0).then(|| (x / count as f64, y / count as f64))
    }

    fn reset_gesture_contact_initials(&mut self, button_areas: bool) {
        for slot in &mut self.mt_slots {
            if Self::gesture_slot_eligible(slot, button_areas) {
                slot.gesture_initial_x = slot.x;
                slot.gesture_initial_y = slot.y;
            }
        }
    }

    fn gesture_contact_rebase_ready(&self) -> bool {
        self.mt_slots
            .iter()
            .all(|slot| !slot.active || !slot.gesture_rebase_pending || slot.gesture_position_seen)
    }

    fn complete_gesture_contact_rebase(&mut self) {
        for slot in &mut self.mt_slots {
            if slot.active && slot.gesture_rebase_pending {
                slot.gesture_rebase_pending = false;
                slot.gesture_position_seen = false;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn gesture_contact_moved_at_least(
        &self,
        button_areas: bool,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
        minimum_mm: f64,
    ) -> Option<bool> {
        let mut any_eligible = false;
        for slot in &self.mt_slots {
            if !Self::gesture_slot_eligible(slot, button_areas) {
                continue;
            }
            any_eligible = true;
            let moved_mm = touchpad_delta_mm(
                slot.x - slot.gesture_initial_x,
                x_resolution,
                x_range,
                width_mm,
            )
            .hypot(touchpad_delta_mm(
                slot.y - slot.gesture_initial_y,
                y_resolution,
                y_range,
                height_mm,
            ));
            if moved_mm >= minimum_mm {
                return Some(true);
            }
        }
        any_eligible.then_some(false)
    }

    #[allow(clippy::too_many_arguments)]
    fn gesture_contacts_same_direction(
        &self,
        button_areas: bool,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) -> Option<bool> {
        const PINCH_DISAMBIGUATION_MOVE_THRESHOLD_MM: f64 = 1.5;

        let mut contacts = self
            .mt_slots
            .iter()
            .filter(|slot| Self::gesture_slot_eligible(slot, button_areas));
        let (Some(first), Some(second)) = (contacts.next(), contacts.next()) else {
            return None;
        };

        let first_delta = (
            touchpad_delta_mm(
                first.x - first.gesture_initial_x,
                x_resolution,
                x_range,
                width_mm,
            ),
            touchpad_delta_mm(
                first.y - first.gesture_initial_y,
                y_resolution,
                y_range,
                height_mm,
            ),
        );
        let second_delta = (
            touchpad_delta_mm(
                second.x - second.gesture_initial_x,
                x_resolution,
                x_range,
                width_mm,
            ),
            touchpad_delta_mm(
                second.y - second.gesture_initial_y,
                y_resolution,
                y_range,
                height_mm,
            ),
        );
        if first_delta.0.hypot(first_delta.1) < PINCH_DISAMBIGUATION_MOVE_THRESHOLD_MM
            || second_delta.0.hypot(second_delta.1) < PINCH_DISAMBIGUATION_MOVE_THRESHOLD_MM
        {
            return None;
        }

        // Parallel contact motion is a two-finger scroll even if its contact
        // separation is noisy. Keep the result tri-state: before both
        // contacts have moved far enough, existing pinch buildup remains the
        // more reliable signal on devices with coarse coordinate resolution.
        Some(gesture_directions_overlap(
            gesture_direction_mask(first_delta.0, first_delta.1),
            gesture_direction_mask(second_delta.0, second_delta.1),
        ))
    }

    fn bottom_button_area_top(
        y_range: Option<(i32, i32)>,
        y_resolution: Option<i32>,
        height_mm: Option<f64>,
    ) -> Option<f64> {
        let (minimum, maximum) = y_range?;
        let span = f64::from(maximum) - f64::from(minimum);
        if span <= 0.0 {
            return None;
        }

        // The lower softbutton strip is the smaller of 10 mm and 15% of
        // the touchpad height. Prefer the kernel's unit/mm resolution, then
        // fall back to the size inferred during device initialization.
        let button_height = if let Some(resolution) = y_resolution.filter(|value| *value > 0) {
            (10.0 * f64::from(resolution)).min(span * 0.15)
        } else if let Some(height_mm) = height_mm.filter(|value| value.is_finite() && *value > 0.0)
        {
            span * (10.0 / height_mm).min(0.15)
        } else {
            span * 0.15
        };
        Some(f64::from(maximum) - button_height)
    }

    fn classify_gesture_contacts(
        &mut self,
        button_areas: bool,
        button_top: Option<f64>,
        time_usec: u64,
    ) {
        for slot in &mut self.mt_slots {
            if slot.active && !slot.palm_suppressed && slot.button_area_classification_pending {
                slot.button_area_initial_x = slot.x;
                slot.button_area_initial_y = slot.y;
                slot.button_area_down_usec = time_usec;
                slot.button_area_excluded =
                    button_areas && button_top.is_some_and(|top| slot.y >= top);
                slot.button_area_classification_pending = false;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_button_area_contacts(
        &mut self,
        button_areas: bool,
        button_top: Option<f64>,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) -> bool {
        if !button_areas {
            return false;
        }
        let Some(button_top) = button_top else {
            return false;
        };

        let activation_times: Vec<u64> = self
            .mt_slots
            .iter()
            .filter_map(|slot| {
                if !slot.active || slot.palm_suppressed || !slot.button_area_excluded {
                    return None;
                }
                let movement_mm = touchpad_delta_mm(
                    slot.x - slot.button_area_initial_x,
                    x_resolution,
                    x_range,
                    width_mm,
                )
                .hypot(touchpad_delta_mm(
                    slot.y - slot.button_area_initial_y,
                    y_resolution,
                    y_range,
                    height_mm,
                ));
                (slot.y < button_top || movement_mm > 5.0).then_some(slot.button_area_down_usec)
            })
            .collect();
        if activation_times.is_empty() {
            return false;
        }

        let mut changed = false;
        for slot in &mut self.mt_slots {
            if slot.active
                && !slot.palm_suppressed
                && slot.button_area_excluded
                && activation_times
                    .iter()
                    .any(|time_usec| slot.button_area_down_usec.abs_diff(*time_usec) <= 80_000)
            {
                // A bottom-area contact becomes a gesture contact for the
                // rest of its lifetime. Simultaneous bottom contacts belong
                // to that gesture as well.
                slot.button_area_excluded = false;
                changed = true;
            }
        }
        changed
    }

    fn edge_boundary(
        range: Option<(i32, i32)>,
        resolution: Option<i32>,
        size_mm: Option<f64>,
    ) -> Option<f64> {
        let (minimum, maximum) = range?;
        let span = f64::from(maximum) - f64::from(minimum);
        if span <= 0.0 {
            return None;
        }
        let edge_width = if let Some(resolution) = resolution.filter(|value| *value > 0) {
            7.0 * f64::from(resolution)
        } else if let Some(size_mm) = size_mm.filter(|value| value.is_finite() && *value > 0.0) {
            span * 7.0 / size_mm
        } else {
            // The kernel failed to provide physical metadata. Keep the
            // equivalent conservative 7% fallback rather than silently
            // disabling edge scrolling for an otherwise usable touchpad.
            span * 0.07
        };
        Some(f64::from(maximum) - edge_width.min(span))
    }

    fn edge_boundaries(
        &self,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) -> (Option<f64>, Option<f64>) {
        let right = Self::edge_boundary(x_range, x_resolution, width_mm);
        let horizontal_allowed = self.is_clickpad
            || height_mm.is_some_and(|height| height.is_finite() && height >= 40.0);
        let bottom = horizontal_allowed
            .then(|| Self::edge_boundary(y_range, y_resolution, height_mm))
            .flatten();
        (right, bottom)
    }

    fn edge_mask_at(x: f64, y: f64, right_edge: Option<f64>, bottom_edge: Option<f64>) -> u8 {
        let mut mask = 0;
        if right_edge.is_some_and(|edge| x > edge) {
            mask |= EDGE_RIGHT;
        }
        if bottom_edge.is_some_and(|edge| y > edge) {
            mask |= EDGE_BOTTOM;
        }
        mask
    }

    #[allow(clippy::too_many_arguments)]
    fn classify_edge_contacts(
        &mut self,
        edge_enabled: bool,
        button_areas: bool,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) {
        let (right_edge, bottom_edge) = self.edge_boundaries(
            x_resolution,
            y_resolution,
            x_range,
            y_range,
            width_mm,
            height_mm,
        );
        for slot in &mut self.mt_slots {
            if !slot.active || slot.palm_suppressed || !slot.edge_classification_pending {
                continue;
            }

            slot.edge_classification_pending = false;
            slot.edge_initial_x = slot.x;
            slot.edge_initial_y = slot.y;
            slot.edge_last_x = slot.x;
            slot.edge_last_y = slot.y;
            slot.edge_axis = 0;
            slot.edge_started_at = None;
            slot.edge_mask = if edge_enabled {
                Self::edge_mask_at(slot.x, slot.y, right_edge, bottom_edge)
            } else {
                0
            };
            slot.edge_state = if slot.edge_mask != 0 {
                if !button_areas {
                    slot.edge_started_at = Some(Instant::now());
                }
                EdgeScrollState::New
            } else {
                EdgeScrollState::Area
            };
        }
    }

    fn expire_edge_scroll_timeouts(&mut self) {
        for slot in &mut self.mt_slots {
            if slot.edge_state == EdgeScrollState::New
                && slot
                    .edge_started_at
                    .is_some_and(|started| started.elapsed() >= Duration::from_millis(300))
            {
                slot.edge_state = EdgeScrollState::Active;
                slot.edge_started_at = None;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_edge_scroll_frame(
        &mut self,
        edge_enabled: bool,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) -> EdgeScrollFrame {
        let (right_edge, bottom_edge) = self.edge_boundaries(
            x_resolution,
            y_resolution,
            x_range,
            y_range,
            width_mm,
            height_mm,
        );
        let mut frame = EdgeScrollFrame::default();

        for slot in &mut self.mt_slots {
            if !slot.active || slot.palm_suppressed {
                frame.stop_axes |= slot.edge_axis;
                slot.edge_axis = 0;
                slot.edge_state = EdgeScrollState::None;
                slot.edge_mask = 0;
                slot.edge_started_at = None;
                continue;
            }

            if !edge_enabled {
                frame.stop_axes |= slot.edge_axis;
                slot.edge_axis = 0;
                slot.edge_state = EdgeScrollState::Area;
                slot.edge_mask = 0;
                slot.edge_started_at = None;
                continue;
            }

            let current_edge = Self::edge_mask_at(slot.x, slot.y, right_edge, bottom_edge);
            match slot.edge_state {
                EdgeScrollState::None => {
                    slot.edge_state = EdgeScrollState::Area;
                }
                EdgeScrollState::Area => {}
                EdgeScrollState::New => {
                    slot.edge_mask &= current_edge;
                    if slot.edge_mask == 0 {
                        slot.edge_state = EdgeScrollState::Area;
                        slot.edge_started_at = None;
                    } else {
                        frame.suppress_normal_motion = true;
                        let (axis, displacement_mm, raw_delta) = match slot.edge_mask {
                            EDGE_RIGHT => (
                                AXIS_VERTICAL,
                                touchpad_delta_mm(
                                    slot.y - slot.edge_initial_y,
                                    y_resolution,
                                    y_range,
                                    height_mm,
                                ),
                                slot.y - slot.edge_last_y,
                            ),
                            EDGE_BOTTOM => (
                                AXIS_HORIZONTAL,
                                touchpad_delta_mm(
                                    slot.x - slot.edge_initial_x,
                                    x_resolution,
                                    x_range,
                                    width_mm,
                                ),
                                slot.x - slot.edge_last_x,
                            ),
                            _ => (0, 0.0, 0.0),
                        };
                        // Upstream keeps EdgeNew until it has emitted the
                        // current frame after the cumulative 3 mm threshold.
                        if axis != 0 && displacement_mm.abs() >= 3.0 && raw_delta != 0.0 {
                            frame.scrolls.push((axis, raw_delta));
                            slot.edge_axis = axis;
                            slot.edge_state = EdgeScrollState::Active;
                            slot.edge_started_at = None;
                        }
                    }
                }
                EdgeScrollState::Active => {
                    if slot.edge_mask == (EDGE_RIGHT | EDGE_BOTTOM) {
                        slot.edge_mask &= current_edge;
                        if slot.edge_mask == 0 {
                            slot.edge_state = EdgeScrollState::Area;
                        }
                    }
                    if slot.edge_state == EdgeScrollState::Active {
                        frame.suppress_normal_motion = true;
                        let (axis, raw_delta) = match slot.edge_mask {
                            EDGE_RIGHT if current_edge & EDGE_RIGHT != 0 => {
                                (AXIS_VERTICAL, slot.y - slot.edge_last_y)
                            }
                            EDGE_BOTTOM if current_edge & EDGE_BOTTOM != 0 => {
                                (AXIS_HORIZONTAL, slot.x - slot.edge_last_x)
                            }
                            _ => (0, 0.0),
                        };
                        if axis != 0 && raw_delta != 0.0 {
                            frame.scrolls.push((axis, raw_delta));
                            slot.edge_axis = axis;
                        }
                    }
                }
            }

            slot.edge_last_x = slot.x;
            slot.edge_last_y = slot.y;
        }

        frame
    }

    fn stop_edge_scroll(&mut self) -> u8 {
        let mut axes = 0;
        for slot in &mut self.mt_slots {
            axes |= slot.edge_axis;
            let was_edge_contact = matches!(
                slot.edge_state,
                EdgeScrollState::New | EdgeScrollState::Active
            );
            slot.edge_axis = 0;
            slot.edge_mask = 0;
            slot.edge_started_at = None;
            slot.edge_classification_pending = false;
            if slot.active {
                if was_edge_contact {
                    // A click releases an edge touch back into pointer
                    // motion, including when it began in a softbutton area.
                    slot.button_area_excluded = false;
                }
                slot.edge_state = EdgeScrollState::Area;
            } else {
                slot.edge_state = EdgeScrollState::None;
            }
        }
        axes
    }

    fn clickfinger_finger_count(
        &self,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) -> u32 {
        let contacts: Vec<&MtSlot> = self
            .mt_slots
            .iter()
            .filter(|slot| slot.active && !slot.palm_suppressed)
            .collect();
        let mut fingers = self.touch_fingers.max(contacts.len() as u32);
        if fingers == 2
            && contacts.len() >= 2
            && !Self::clickfinger_contacts_within_distance(
                contacts[0],
                contacts[1],
                x_resolution,
                y_resolution,
                x_range,
                y_range,
                width_mm,
                height_mm,
            )
        {
            fingers = 1;
        }
        fingers
    }

    // The separate axes and dimensions mirror libinput's physical-distance
    // calculation; grouping them would obscure which axis each value serves.
    #[allow(clippy::too_many_arguments)]
    fn clickfinger_contacts_within_distance(
        first: &MtSlot,
        second: &MtSlot,
        x_resolution: Option<i32>,
        y_resolution: Option<i32>,
        x_range: Option<(i32, i32)>,
        y_range: Option<(i32, i32)>,
        width_mm: Option<f64>,
        height_mm: Option<f64>,
    ) -> bool {
        let x_distance =
            touchpad_delta_mm(first.x - second.x, x_resolution, x_range, width_mm).abs();
        let y_distance =
            touchpad_delta_mm(first.y - second.y, y_resolution, y_range, height_mm).abs();
        if x_distance > 40.0 || y_distance > 30.0 {
            return false;
        }
        if y_distance <= 20.0 {
            return true;
        }

        let height = height_mm.or_else(|| {
            y_range
                .zip(y_resolution)
                .and_then(|((minimum, maximum), resolution)| {
                    (resolution > 0)
                        .then_some((maximum - minimum).unsigned_abs() as f64 / resolution as f64)
                })
        });
        if height.is_some_and(|height| height < 50.0) {
            return true;
        }

        let distance_from_bottom = |position: f64| {
            if let (Some((_, maximum)), Some(resolution)) = (y_range, y_resolution) {
                if resolution > 0 {
                    return (maximum as f64 - position) / resolution as f64;
                }
            }
            if let (Some((minimum, maximum)), Some(height)) = (y_range, height) {
                let range = (maximum - minimum).unsigned_abs() as f64;
                if range > 0.0 {
                    return (maximum as f64 - position) * height / range;
                }
            }
            f64::INFINITY
        };
        (distance_from_bottom(first.y) < 20.0) == (distance_from_bottom(second.y) < 20.0)
    }

    /// Euclidean distance between the two primary gesture contacts.
    fn gesture_primary_slot_distance(&self, button_areas: bool) -> Option<f64> {
        let active: Vec<&MtSlot> = self
            .mt_slots
            .iter()
            .filter(|slot| Self::gesture_slot_eligible(slot, button_areas))
            .collect();
        if active.len() < 2 {
            return None;
        }
        let dx = active[0].x - active[1].x;
        let dy = active[0].y - active[1].y;
        Some((dx * dx + dy * dy).sqrt())
    }

    /// Angle (degrees) of the vector between the two primary gesture contacts.
    fn gesture_primary_slot_angle(&self, button_areas: bool) -> f64 {
        let active: Vec<&MtSlot> = self
            .mt_slots
            .iter()
            .filter(|slot| Self::gesture_slot_eligible(slot, button_areas))
            .collect();
        if active.len() < 2 {
            return 0.0;
        }
        let dx = active[1].x - active[0].x;
        let dy = active[1].y - active[0].y;
        dy.atan2(dx).to_degrees()
    }

    fn rebase_gesture_contacts_after_eligibility_change(&mut self, button_areas: bool) {
        self.current_dx = 0;
        self.current_dy = 0;
        self.remainder_x = 0.0;
        self.remainder_y = 0.0;
        self.finger_scroll_constraint.reset();
        self.reset_gesture_contact_initials(button_areas);
        self.gesture_last_centroid = self.gesture_centroid(button_areas);
        self.pinch_base_centroid = self.gesture_last_centroid;

        let n_fingers = self.gesture_finger_count(button_areas);
        if !self.pinch_active && n_fingers >= 2 {
            if let Some(distance) = self.gesture_primary_slot_distance(button_areas) {
                self.pinch_base_dist = distance;
                self.pinch_base_angle = self.gesture_primary_slot_angle(button_areas);
                self.pinch_fingers = n_fingers as i32;
            }
        }
    }

    fn sync_gesture_finger_count(&mut self, n_fingers: usize) {
        if self.gesture_fingers_last == n_fingers {
            return;
        }

        self.gesture_fingers_last = n_fingers;
        self.hold_contact_changed = true;
        if self.hold_active {
            return;
        }

        if n_fingers == 0 {
            self.hold_started_at = None;
            self.hold_blocked = false;
            self.hold_fingers = 0;
        } else if !self.hold_blocked {
            self.hold_started_at = Some(Instant::now());
            self.hold_fingers = n_fingers as i32;
        }
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
    if tool_type == 2 {
        if let Some(tool) = (*ctx).tablet_tools.iter().copied().find(|tool| {
            !tool.is_null()
                && (**tool).tool_type == 1
                && (**tool).eraser_button_mode == 1
                && if serial == 0 {
                    (**tool).serial == 0 && (**tool).device == device
                } else {
                    (**tool).serial == serial
                }
        }) {
            (*tool).device = device;
            return tool;
        }
    }
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
    let mut eraser_button_modes = u32::from(tool_type == 1);
    let name = tablet_tool_name_for_id(tool_id);
    if let Some(stylus) = libwacom_stylus_for_id(tool_id) {
        if libwacom_stylus_has_eraser(stylus) != 0 && libwacom_stylus_get_eraser_type(stylus) == 2 {
            eraser_button_modes = 0;
        }
    }
    let tool_buttons: Vec<u32> = buttons
        .iter()
        .copied()
        .filter(|button| match tool_type {
            6 | 7 => (0x110..=0x117).contains(button),
            8 => *button == 0x100,
            _ => matches!(*button, 0x149 | 0x14b | 0x14c),
        })
        .collect();
    let default_eraser_button = if !tool_buttons.contains(&0x14b) {
        0x14b
    } else if !tool_buttons.contains(&0x14c) {
        0x14c
    } else {
        0x149
    };
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
        pressure_range_minimum: 0.0,
        pressure_range_maximum: 1.0,
        wanted_pressure_range_minimum: 0.0,
        wanted_pressure_range_maximum: 1.0,
        eraser_button_modes,
        eraser_button_mode: 0,
        wanted_eraser_button_mode: 0,
        eraser_button: default_eraser_button,
        wanted_eraser_button: default_eraser_button,
        default_eraser_button,
        in_proximity: false,
        buttons: tool_buttons,
    }));
    (*ctx).tablet_tools.push(tool);
    tool
}

pub(crate) fn tablet_tool_name_for_id(tool_id: u64) -> Option<std::ffi::CString> {
    let tool_id = i32::try_from(tool_id).ok()?;
    unsafe {
        let database = libwacom_database_new();
        if database.is_null() {
            return None;
        }
        let stylus = libwacom_stylus_get_for_id(database, tool_id);
        let name = stylus.as_ref().and_then(|stylus| {
            let value = libwacom_stylus_get_name(stylus);
            (!value.is_null()).then(|| std::ffi::CStr::from_ptr(value).to_owned())
        });
        libwacom_database_destroy(database);
        name
    }
}

pub(crate) fn libwacom_stylus_for_id(tool_id: u64) -> Option<*const libc::c_void> {
    let tool_id = i32::try_from(tool_id).ok()?;
    let database = unsafe { libwacom_database_new() };
    if database.is_null() {
        return None;
    }
    let stylus = unsafe { libwacom_stylus_get_for_id(database, tool_id) };
    unsafe { libwacom_database_destroy(database) };
    Some(stylus).filter(|s| !s.is_null())
}

unsafe fn tablet_tool_payload(
    td: &TrackedDevice,
    lib_dev: *mut LibinputDevice,
    time_usec: u64,
    tool: *mut LibinputTabletTool,
    proximity_state: u32,
) -> TabletToolEvent {
    let (raw_x_min, raw_x_max) = (*lib_dev).abs_x_range.unwrap_or((0, 0));
    let (raw_y_min, raw_y_max) = (*lib_dev).abs_y_range.unwrap_or((0, 0));
    let area = (*lib_dev).area;
    let has_custom_area = (*lib_dev).area_available && area != [0.0, 0.0, 1.0, 1.0];
    let (x_min, y_min, x_max, y_max) = if has_custom_area {
        let x_span = f64::from(raw_x_max - raw_x_min);
        let y_span = f64::from(raw_y_max - raw_y_min);
        (
            f64::from(raw_x_min) + (x_span * area[0]).trunc(),
            f64::from(raw_y_min) + (y_span * area[1]).trunc(),
            f64::from(raw_x_min) + (x_span * area[2]).trunc(),
            f64::from(raw_y_min) + (y_span * area[3]).trunc(),
        )
    } else {
        (
            f64::from(raw_x_min),
            f64::from(raw_y_min),
            f64::from(raw_x_max),
            f64::from(raw_y_max),
        )
    };
    let (pressure_lower, _, configured_pressure_max) = tablet_pressure_thresholds(td, tool);
    let pressure_is_active = td.tablet_pressure > pressure_lower;
    let distance = td.tablet_distance_range.map_or(0.0, |(minimum, maximum)| {
        let range = f64::from(maximum - minimum);
        if pressure_is_active || range <= 0.0 {
            0.0
        } else {
            ((td.tablet_distance - f64::from(minimum)) / range).clamp(0.0, 1.0)
        }
    });
    // libinput treats an inclusive evdev axis as maximum - minimum + 1
    // units when building its calibration transform.
    let x_range = x_max - x_min + 1.0;
    let y_range = y_max - y_min + 1.0;
    let mut tablet_x = if has_custom_area {
        td.tablet_x.clamp(x_min, x_max)
    } else {
        td.tablet_x
    };
    let mut tablet_y = if has_custom_area {
        td.tablet_y.clamp(y_min, y_max)
    } else {
        td.tablet_y
    };
    if td.tablet_left_handed_applied {
        tablet_x = x_min + x_max - tablet_x;
        tablet_y = y_min + y_max - tablet_y;
    }
    let normalized_x = if x_range > 0.0 {
        (tablet_x - x_min) / x_range
    } else {
        0.0
    };
    let normalized_y = if y_range > 0.0 {
        (tablet_y - y_min) / y_range
    } else {
        0.0
    };
    let mut previous_x = if has_custom_area {
        td.tablet_last_event_x.clamp(x_min, x_max)
    } else {
        td.tablet_last_event_x
    };
    let mut previous_y = if has_custom_area {
        td.tablet_last_event_y.clamp(y_min, y_max)
    } else {
        td.tablet_last_event_y
    };
    if td.tablet_left_handed_applied {
        previous_x = x_min + x_max - previous_x;
        previous_y = y_min + y_max - previous_y;
    }
    let previous_normalized_x = if x_range > 0.0 {
        (previous_x - x_min) / x_range
    } else {
        0.0
    };
    let previous_normalized_y = if y_range > 0.0 {
        (previous_y - y_min) / y_range
    } else {
        0.0
    };
    let matrix = (*lib_dev).calibration.map(f64::from);
    let transformed_x = matrix[0] * normalized_x + matrix[1] * normalized_y + matrix[2];
    let transformed_y = matrix[3] * normalized_x + matrix[4] * normalized_y + matrix[5];
    let dx = (matrix[0] * (normalized_x - previous_normalized_x)
        + matrix[1] * (normalized_y - previous_normalized_y))
        * x_range;
    let dy = (matrix[3] * (normalized_x - previous_normalized_x)
        + matrix[4] * (normalized_y - previous_normalized_y))
        * y_range;
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
    let mut tilt_x = normalize_tilt(td.tablet_tilt_x, td.tablet_tilt_x_info);
    let mut tilt_y = normalize_tilt(td.tablet_tilt_y, td.tablet_tilt_y_info);
    if td.tablet_left_handed_applied {
        tilt_x = -tilt_x;
        tilt_y = -tilt_y;
    }
    (*lib_dev).tablet_current_x = td.tablet_x;
    (*lib_dev).tablet_current_y = td.tablet_y;
    (*lib_dev).tablet_current_tilt_x = tilt_x;
    let tool_type = (*tool).tool_type;
    let mut rotation = if matches!(tool_type, 6 | 7) {
        ((-tilt_x).atan2(tilt_y).to_degrees() - 5.0).rem_euclid(360.0)
    } else if tool_type == 8 {
        (360.0 - td.tablet_rotation).rem_euclid(360.0)
    } else if let Some((minimum, maximum)) = td.tablet_rotation_info {
        let range = f64::from(maximum - minimum + 1);
        if range > 0.0 {
            (((td.tablet_rotation - f64::from(minimum)) / range) * 360.0 + 90.0).rem_euclid(360.0)
        } else {
            0.0
        }
    } else {
        0.0
    };
    if td.tablet_left_handed_applied && !matches!(tool_type, 6 | 7) {
        rotation = (rotation + 180.0).rem_euclid(360.0);
    }
    let slider = td.tablet_slider_range.map_or(0.0, |(minimum, maximum)| {
        let range = f64::from(maximum - minimum);
        if range > 0.0 {
            ((td.tablet_slider - f64::from(minimum)) / range) * 2.0 - 1.0
        } else {
            0.0
        }
    });
    let size_major = if td.tablet_size_major_resolution > 0 {
        td.tablet_size_major / f64::from(td.tablet_size_major_resolution)
    } else {
        0.0
    };
    let size_minor = if td.tablet_size_minor_resolution > 0 {
        td.tablet_size_minor / f64::from(td.tablet_size_minor_resolution)
    } else {
        0.0
    };
    (*lib_dev).tablet_current_tool_type = tool_type;
    (*lib_dev).tablet_current_size_major = size_major;
    (*lib_dev).tablet_current_size_minor = size_minor;
    TabletToolEvent {
        time_usec,
        tool,
        proximity_state,
        x: transformed_x * x_range + x_min,
        y: transformed_y * y_range + y_min,
        dx,
        dy,
        x_min,
        x_max,
        y_min,
        y_max,
        x_resolution: f64::from((*lib_dev).abs_x_resolution.unwrap_or(0)),
        y_resolution: f64::from((*lib_dev).abs_y_resolution.unwrap_or(0)),
        x_changed: (matrix[0] != 0.0 && td.tablet_x_changed)
            || (matrix[1] != 0.0 && td.tablet_y_changed),
        y_changed: (matrix[3] != 0.0 && td.tablet_x_changed)
            || (matrix[4] != 0.0 && td.tablet_y_changed),
        pressure: td.tablet_pressure,
        pressure_min: pressure_lower,
        pressure_max: configured_pressure_max,
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
        size_major,
        size_minor,
        size_major_changed: td.tablet_size_major_changed,
        size_minor_changed: td.tablet_size_minor_changed,
        tip_state: u32::from(td.tablet_tip_down),
        button: 0,
        button_state: 0,
        seat_button_count: 0,
    }
}

unsafe fn push_tablet_tool_button(
    td: &TrackedDevice,
    lib_dev: *mut LibinputDevice,
    ctx: *mut LibinputContext,
    out: &mut VecDeque<LibinputEvent>,
    time_usec: u64,
    tool: *mut LibinputTabletTool,
    button_state: (u32, bool),
) {
    let (button, pressed) = button_state;
    let seat_button_count = if pressed {
        press_seat_button(lib_dev)
    } else {
        release_seat_button(lib_dev)
    };
    (*tool)
        .refcount
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut payload = tablet_tool_payload(td, lib_dev, time_usec, tool, 1);
    payload.button = button;
    payload.button_state = u32::from(pressed);
    payload.seat_button_count = seat_button_count;
    out.push_back(LibinputEvent {
        event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_BUTTON,
        payload: EventPayload::TabletTool(payload),
        context: ctx,
        device: lib_dev,
    });
}

unsafe fn active_tablet_for_touch(
    ctx: *mut LibinputContext,
    touch_device: *mut LibinputDevice,
) -> Option<*mut LibinputDevice> {
    let touch_group =
        crate::udev::property_value((*touch_device).udev_device, "LIBINPUT_DEVICE_GROUP");
    let mut fallback_tablet = None;
    for &candidate in &(*ctx).devices {
        if candidate.is_null()
            || candidate == touch_device
            || !(*candidate).has_tablet
            || (*candidate).has_gesture
            || (*candidate).has_tablet_pad
            || !(*candidate).tablet_in_proximity
        {
            continue;
        }
        fallback_tablet = Some(candidate);
        let tablet_group =
            crate::udev::property_value((*candidate).udev_device, "LIBINPUT_DEVICE_GROUP");
        if touch_group.is_some() && touch_group == tablet_group {
            return Some(candidate);
        }
    }
    fallback_tablet.filter(|_| (*touch_device).has_touch)
}

unsafe fn paired_left_handed_rotation_enabled(
    ctx: *mut LibinputContext,
    device: *mut LibinputDevice,
) -> bool {
    let device_is_tablet_tool = (*device).has_tablet
        && !(*device).has_touch
        && !(*device).has_gesture
        && !(*device).has_tablet_pad;
    let device_is_gesture_touchpad = (*device).has_gesture && !(*device).has_tablet_pad;
    if !device_is_tablet_tool && !device_is_gesture_touchpad {
        return (*device).left_handed;
    }

    let Some(device_group) =
        crate::udev::property_value((*device).udev_device, "LIBINPUT_DEVICE_GROUP")
    else {
        return (*device).left_handed;
    };

    for &candidate in &(*ctx).devices {
        if candidate.is_null() || candidate == device || (*candidate).has_tablet_pad {
            continue;
        }
        let candidate_is_tablet_tool =
            (*candidate).has_tablet && !(*candidate).has_touch && !(*candidate).has_gesture;
        let candidate_is_gesture_touchpad = (*candidate).has_gesture;
        let is_paired_counterpart = (device_is_tablet_tool && candidate_is_gesture_touchpad)
            || (device_is_gesture_touchpad && candidate_is_tablet_tool);
        if !is_paired_counterpart {
            continue;
        }
        let candidate_group =
            crate::udev::property_value((*candidate).udev_device, "LIBINPUT_DEVICE_GROUP");
        if candidate_group.as_deref() == Some(device_group.as_str()) {
            return (*device).left_handed || (*candidate).left_handed;
        }
    }

    (*device).left_handed
}

unsafe fn touch_arbitration_active(
    ctx: *mut LibinputContext,
    touch_device: *mut LibinputDevice,
    touch_point: Option<(f64, f64)>,
) -> bool {
    if (*ctx)
        .touch_arbitration_until
        .is_some_and(|deadline| Instant::now() < deadline)
    {
        return true;
    }

    let Some(tablet) = active_tablet_for_touch(ctx, touch_device) else {
        return false;
    };
    if (*touch_device).has_gesture {
        return true;
    }
    if (*tablet).tablet_current_tool_type == 8 {
        let Some((touch_x, touch_y)) = touch_point else {
            return false;
        };
        let (tablet_x_min, _) = (*tablet).abs_x_range.unwrap_or((0, 0));
        let (tablet_y_min, _) = (*tablet).abs_y_range.unwrap_or((0, 0));
        let (touch_x_min, _) = (*touch_device).abs_x_range.unwrap_or((0, 0));
        let (touch_y_min, _) = (*touch_device).abs_y_range.unwrap_or((0, 0));
        let tablet_x_resolution = f64::from((*tablet).abs_x_resolution.unwrap_or(1).max(1));
        let tablet_y_resolution = f64::from((*tablet).abs_y_resolution.unwrap_or(1).max(1));
        let touch_x_resolution = f64::from((*touch_device).abs_x_resolution.unwrap_or(1).max(1));
        let touch_y_resolution = f64::from((*touch_device).abs_y_resolution.unwrap_or(1).max(1));
        let totem_x = ((*tablet).tablet_current_x - f64::from(tablet_x_min)) / tablet_x_resolution;
        let totem_y = ((*tablet).tablet_current_y - f64::from(tablet_y_min)) / tablet_y_resolution;
        let finger_x = (touch_x - f64::from(touch_x_min)) / touch_x_resolution;
        let finger_y = (touch_y - f64::from(touch_y_min)) / touch_y_resolution;
        let half_width = (*tablet).tablet_current_size_major * 0.5;
        let half_height = (*tablet).tablet_current_size_minor * 0.5;
        return (finger_x - totem_x).abs() <= half_width
            && (finger_y - totem_y).abs() <= half_height;
    }

    // Direct touchscreens are the conservative fallback pairing until a
    // same-group touch device appears, matching libinput's tablet heuristic.
    if (*tablet).tablet_current_tilt_x == 0.0 {
        return true;
    }
    let Some((touch_x, touch_y)) = touch_point else {
        return false;
    };
    let (tablet_x_min, _) = (*tablet).abs_x_range.unwrap_or((0, 0));
    let (tablet_y_min, _) = (*tablet).abs_y_range.unwrap_or((0, 0));
    let (touch_x_min, _) = (*touch_device).abs_x_range.unwrap_or((0, 0));
    let (touch_y_min, _) = (*touch_device).abs_y_range.unwrap_or((0, 0));
    let tablet_x_resolution = f64::from((*tablet).abs_x_resolution.unwrap_or(1).max(1));
    let tablet_y_resolution = f64::from((*tablet).abs_y_resolution.unwrap_or(1).max(1));
    let touch_x_resolution = f64::from((*touch_device).abs_x_resolution.unwrap_or(1).max(1));
    let touch_y_resolution = f64::from((*touch_device).abs_y_resolution.unwrap_or(1).max(1));
    let pen_x = ((*tablet).tablet_current_x - f64::from(tablet_x_min)) / tablet_x_resolution;
    let pen_y = ((*tablet).tablet_current_y - f64::from(tablet_y_min)) / tablet_y_resolution;
    let finger_x = (touch_x - f64::from(touch_x_min)) / touch_x_resolution;
    let finger_y = (touch_y - f64::from(touch_y_min)) / touch_y_resolution;
    let (left, right) = if (*tablet).tablet_current_tilt_x > 0.0 {
        (pen_x - 20.0, pen_x + 180.0)
    } else {
        (pen_x - 180.0, pen_x + 20.0)
    };
    finger_x >= left.max(0.0)
        && finger_x <= right
        && finger_y >= (pen_y - 100.0).max(0.0)
        && finger_y <= pen_y + 150.0
}

unsafe fn tablet_pressure_thresholds(
    td: &TrackedDevice,
    tool: *mut LibinputTabletTool,
) -> (f64, f64, f64) {
    let (minimum, maximum) = td.tablet_pressure_range.unwrap_or((0, 0));
    let full_span = f64::from(maximum - minimum);
    if let Some(offset) = td.tablet_pressure_offset {
        let gap = (full_span * 0.04).trunc().max(1.0);
        return (offset, offset + gap, f64::from(maximum));
    }
    let configured_minimum =
        f64::from(minimum) + (full_span * (*tool).pressure_range_minimum).trunc();
    let configured_maximum =
        f64::from(minimum) + (full_span * (*tool).pressure_range_maximum).trunc();
    let configured_span = configured_maximum - configured_minimum;
    let lower = configured_minimum + (configured_span * 0.01).trunc();
    let upper = configured_minimum + (configured_span * 0.05).trunc();
    (lower, upper, configured_maximum)
}

unsafe fn update_tablet_pressure_offset(td: &mut TrackedDevice, entering_proximity: bool) {
    if !td.tablet_pressure_changed || td.tablet_tool.is_null() {
        return;
    }
    let tool = &*td.tablet_tool;
    if tool.pressure_range_minimum != 0.0 || tool.pressure_range_maximum != 1.0 {
        td.tablet_pressure_offset = None;
        td.tablet_pressure_offset_candidate = None;
        td.tablet_pressure_proximity_samples = 0;
        td.tablet_pressure_offset_rejected = false;
        return;
    }
    let Some((minimum, _)) = td.tablet_pressure_range else {
        return;
    };
    let pressure = td.tablet_pressure;
    if pressure <= f64::from(minimum) {
        return;
    }
    if td.tablet_pressure_offset_rejected {
        return;
    }
    if let Some(offset) = &mut td.tablet_pressure_offset {
        if pressure < *offset {
            *offset = pressure;
        }
        return;
    }

    if td.tablet_has_distance {
        if !entering_proximity {
            return;
        }
        let Some((distance_minimum, distance_maximum)) = td.tablet_distance_range else {
            return;
        };
        let midpoint =
            f64::from(distance_minimum) + f64::from(distance_maximum - distance_minimum) * 0.5;
        if td.tablet_distance >= midpoint {
            let (_, maximum) = td.tablet_pressure_range.unwrap_or((minimum, minimum));
            let normalized = (pressure - f64::from(minimum)) / f64::from(maximum - minimum);
            if normalized > 0.5 {
                td.tablet_pressure_offset_rejected = true;
                crate::emit_error_log(
                    (*td.lib_device).context,
                    "Ignoring pressure offset greater than 50% detected on tool pen",
                );
            } else {
                td.tablet_pressure_offset = Some(pressure);
                crate::emit_info_log(
                    (*td.lib_device).context,
                    &format!(
                        "Pressure offset of {:.0}% detected on tool pen",
                        normalized * 100.0
                    ),
                );
            }
        }
        return;
    }

    td.tablet_pressure_offset_candidate = Some(
        td.tablet_pressure_offset_candidate
            .map_or(pressure, |candidate| candidate.min(pressure)),
    );
    if entering_proximity {
        td.tablet_pressure_proximity_samples =
            td.tablet_pressure_proximity_samples.saturating_add(1);
        if td.tablet_pressure_proximity_samples >= 3 {
            let candidate = td.tablet_pressure_offset_candidate.unwrap_or(pressure);
            let (_, maximum) = td.tablet_pressure_range.unwrap_or((minimum, minimum));
            let normalized = (candidate - f64::from(minimum)) / f64::from(maximum - minimum);
            if normalized > 0.5 {
                td.tablet_pressure_offset_rejected = true;
                crate::emit_error_log(
                    (*td.lib_device).context,
                    "Ignoring pressure offset greater than 50% detected on tool pen",
                );
            } else {
                td.tablet_pressure_offset = Some(candidate);
                crate::emit_info_log(
                    (*td.lib_device).context,
                    &format!(
                        "Pressure offset of {:.0}% detected on tool pen",
                        normalized * 100.0
                    ),
                );
            }
        }
    }
}

unsafe fn update_tablet_tip_from_pressure(td: &mut TrackedDevice, touch_button_changed: bool) {
    if !td.tablet_pressure_changed || touch_button_changed {
        return;
    }
    let Some(_) = td.tablet_pressure_range else {
        return;
    };
    if td.tablet_tool.is_null() {
        return;
    }
    let (lower, upper, _) = tablet_pressure_thresholds(td, td.tablet_tool);
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

// ---------------------------------------------------------------------------
// BackendState
// ---------------------------------------------------------------------------

pub struct BackendState {
    devices: HashMap<RawFd, TrackedDevice>,
    requested_paths: Vec<PathBuf>,
    inotify: Option<Inotify>,
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
            suspended: false,
        }
    }

    fn dwt_wants_keyboard(touchpad: DwtPairingInfo, keyboard: DwtPairingInfo) -> bool {
        if touchpad.fd == keyboard.fd || !touchpad.is_touchpad || !keyboard.is_dwt_keyboard {
            return false;
        }
        if touchpad.touchpad_is_external {
            touchpad.vendor_id == keyboard.vendor_id && touchpad.product_id == keyboard.product_id
        } else {
            keyboard.keyboard_internal
        }
    }

    fn dwtp_wants_trackpoint(touchpad: DwtPairingInfo, trackpoint: DwtPairingInfo) -> bool {
        touchpad.fd != trackpoint.fd
            && touchpad.is_touchpad
            && !touchpad.touchpad_is_external
            && trackpoint.is_pointing_stick
            && !matches!(trackpoint.bus_type, 0x03 | 0x05)
    }

    /// Build DWT/DWTP relationships on every device addition. This handles
    /// either arrival order without treating an arbitrary global pointer
    /// device as a trackpoint partner.
    fn pair_new_dwt_device(&mut self, new_fd: RawFd) {
        let Some(new_device) = self.devices.get(&new_fd) else {
            return;
        };
        let new_info = new_device.dwt_pairing_info(new_fd);
        let infos: Vec<DwtPairingInfo> = self
            .devices
            .iter()
            .map(|(fd, device)| device.dwt_pairing_info(*fd))
            .collect();

        if new_info.is_touchpad {
            let paired_keyboards: Vec<RawFd> = infos
                .iter()
                .copied()
                .filter(|candidate| Self::dwt_wants_keyboard(new_info, *candidate))
                .map(|candidate| candidate.fd)
                .collect();
            let paired_trackpoint = infos
                .iter()
                .copied()
                .find(|candidate| Self::dwtp_wants_trackpoint(new_info, *candidate))
                .map(|candidate| candidate.fd);
            if let Some(touchpad) = self.devices.get_mut(&new_fd) {
                touchpad.dwt.paired_keyboards.extend(paired_keyboards);
                touchpad.dwtp.paired_trackpoint = paired_trackpoint;
            }
        }

        if new_info.is_dwt_keyboard {
            let targets: Vec<RawFd> = infos
                .iter()
                .copied()
                .filter(|candidate| Self::dwt_wants_keyboard(*candidate, new_info))
                .map(|candidate| candidate.fd)
                .collect();
            for target_fd in targets {
                if let Some(touchpad) = self.devices.get_mut(&target_fd) {
                    touchpad.dwt.paired_keyboards.insert(new_fd);
                }
            }
        }

        if new_info.is_pointing_stick && !matches!(new_info.bus_type, 0x03 | 0x05) {
            let targets: Vec<RawFd> = infos
                .iter()
                .copied()
                .filter(|candidate| Self::dwtp_wants_trackpoint(*candidate, new_info))
                .map(|candidate| candidate.fd)
                .collect();
            for target_fd in targets {
                if let Some(touchpad) = self.devices.get_mut(&target_fd) {
                    // Upstream keeps the first paired trackpoint and does not
                    // silently substitute a different existing device.
                    if touchpad.dwtp.paired_trackpoint.is_none() {
                        touchpad.dwtp.paired_trackpoint = Some(new_fd);
                    }
                }
            }
        }
    }

    fn unpair_removed_dwt_device(&mut self, removed_fd: RawFd) {
        for touchpad in self.devices.values_mut() {
            if touchpad.dwt.paired_keyboards.remove(&removed_fd) {
                // Match the upstream removal listener: stop active typing
                // suppression immediately, while keeping any already-marked
                // typing palm suppressed until it lifts.
                touchpad.dwt.active = false;
                touchpad.dwt.deadline = None;
                touchpad.dwt.active_started_usec = None;
                touchpad.dwt.deadline_usec = None;
                touchpad.dwt.pressed_keys.clear();
                touchpad.dwt.modifier_keys.clear();
            }
            if touchpad.dwtp.paired_trackpoint == Some(removed_fd) {
                touchpad.dwtp.paired_trackpoint = None;
            }
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
        if device.set_nonblocking(true).is_err() {
            return;
        }
        {
            use std::os::fd::AsRawFd;
            discard_pending_input_events(device.as_raw_fd());
        }
        {
            use std::os::fd::AsRawFd;
            let clock_id: libc::c_int = libc::CLOCK_MONOTONIC;
            let request = ((1_u64 << 30)
                | ((std::mem::size_of::<libc::c_int>() as u64) << 16)
                | ((b'E' as u64) << 8)
                | 0xa0) as libc::c_ulong;
            let _ = libc::ioctl(device.as_raw_fd(), request, &clock_id);
        }

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
        let mut mt_slot_count = 0;
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
        let mut mt_size_major_resolution = None;
        let mut mt_size_minor_resolution = None;
        if let Ok(absinfo) = device.get_absinfo() {
            for (axis, info) in absinfo {
                has_abs_x |= axis == AbsoluteAxisCode::ABS_X;
                has_abs_y |= axis == AbsoluteAxisCode::ABS_Y;
                has_mt_x |= axis == AbsoluteAxisCode::ABS_MT_POSITION_X;
                has_mt_y |= axis == AbsoluteAxisCode::ABS_MT_POSITION_Y;
                has_mt_slot |= axis == AbsoluteAxisCode::ABS_MT_SLOT;
                if axis == AbsoluteAxisCode::ABS_MT_SLOT {
                    mt_slot_count = (info.maximum() - info.minimum() + 1).max(0);
                }

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
                } else if axis == AbsoluteAxisCode::ABS_MT_TOUCH_MAJOR {
                    mt_size_major_resolution = Some(info.resolution());
                } else if axis == AbsoluteAxisCode::ABS_MT_TOUCH_MINOR {
                    mt_size_minor_resolution = Some(info.resolution());
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
        let mt_size_resolution_mismatch = matches!(
            (mt_size_major_resolution, mt_size_minor_resolution),
            (Some(major), Some(minor)) if (major == 0) != (minor == 0)
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
            || mt_size_resolution_mismatch
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
        let tag_key = has_tag("ID_INPUT_KEY");
        let tag_touchpad = has_tag("ID_INPUT_TOUCHPAD");
        let tag_touchscreen = has_tag("ID_INPUT_TOUCHSCREEN");
        let tag_tablet = has_tag("ID_INPUT_TABLET");
        let tag_tablet_pad = has_tag("ID_INPUT_TABLET_PAD");
        let tag_switch = has_tag("ID_INPUT_SWITCH");
        let tag_mouse = has_tag("ID_INPUT_MOUSE");
        let tag_pointing_stick =
            has_tag("ID_INPUT_POINTINGSTICK") || props.contains(evdev::PropType::POINTING_STICK);
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
        // DWT only pairs ordinary typewriter keyboards. A node exposing a
        // handful of media or receiver keys must not suppress its touchpad.
        let is_typewriter_keyboard = is_keyboard
            && keys.is_some_and(|codes| (16_u16..=25).all(|code| codes.contains(KeyCode(code))));
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
        // libwacom may assign the tablet tag to companion touch and pad
        // nodes.  Upstream dispatches those nodes as touchpads, touchscreens,
        // or tablet pads before considering the tablet-tool interface.
        let mut has_tablet = (tag_tablet && !tag_touchpad && !tag_touchscreen && !tag_tablet_pad)
            || (use_evdev_fallback && has_pen);
        let mut has_touch = tag_touchscreen
            || (use_evdev_fallback
                && !has_pen
                && !is_touchpad
                && has_touch_button
                && (has_abs_xy || has_mt_xy));
        let input_id = device.input_id();
        let has_tablet_pad = tag_tablet_pad
            || (use_evdev_fallback
                && !is_keyboard
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
        (*lib_dev).has_gesture = is_touchpad && !props.contains(evdev::PropType::SEMI_MT);
        (*lib_dev).hold_enabled = (*lib_dev).has_gesture;
        (*lib_dev).hold_default_enabled = (*lib_dev).has_gesture;
        (*lib_dev).mt_slot_count = if has_mt_slot { mt_slot_count } else { 0 };
        (*lib_dev).has_switch = has_switch;
        (*lib_dev).has_tablet = has_tablet;
        (*lib_dev).has_tablet_pad = has_tablet_pad;
        (*lib_dev).abs_x_range = abs_x_range_raw;
        (*lib_dev).abs_y_range = abs_y_range_raw;
        (*lib_dev).abs_x_resolution = abs_x_resolution.filter(|resolution| *resolution > 0);
        (*lib_dev).abs_y_resolution = abs_y_resolution.filter(|resolution| *resolution > 0);
        (*lib_dev).calibration_available = has_abs_xy
            && !is_touchpad
            && !has_tablet_pad
            && (!has_tablet
                || props.contains(evdev::PropType::DIRECT)
                || tablet_is_display_device(path));
        (*lib_dev).area_available = has_tablet && !props.contains(evdev::PropType::DIRECT);
        (*lib_dev).accel_available =
            (is_touchpad || has_relative_motion || has_tablet) && !has_tablet_pad;
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
        (*lib_dev).scroll_methods = u32::from((*lib_dev).supports_button_scroll) * 4;
        let mut event_codes: Vec<u16> = keys
            .map(|codes| codes.iter().map(|code| code.0).collect())
            .unwrap_or_default();
        // Devices may legitimately carry more than one udev role (for
        // example a keyboard receiver that also exposes pointer axes).
        // Preserve every applicable role for quirk matching instead of
        // letting an arbitrary single role hide keyboard integration quirks.
        let mut udev_types = Vec::new();
        if is_keyboard || tag_key {
            udev_types.push("keyboard");
        }
        if is_touchpad {
            udev_types.push("touchpad");
        }
        if has_touch {
            udev_types.push("touchscreen");
        }
        if has_tablet {
            udev_types.push("tablet");
        }
        if has_tablet_pad {
            udev_types.push("tablet-pad");
        }
        if tag_pointing_stick {
            udev_types.push("pointingstick");
        }
        if tag_mouse || has_relative_pointer {
            udev_types.push("mouse");
        }
        if has_switch {
            udev_types.push("switch");
        }
        if udev_types.is_empty() {
            udev_types.push("unknown");
        }
        let applied_quirks = crate::quirks::apply_quirks(
            &name,
            input_id.bus_type().0,
            input_id.vendor(),
            input_id.product(),
            input_id.version(),
            &udev_types,
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
        // A tablet pad can share a physical device with a tablet tool, but
        // its dial/ring/strip node is never an accelerated pointer.
        (*lib_dev).accel_available =
            (is_touchpad || has_relative_motion || has_tablet) && !has_tablet_pad;
        (*lib_dev).event_codes = event_codes;
        let mut left_handed_available = is_pointer;
        if is_touchpad && applied_quirks.model_apple_touchpad_onebutton {
            left_handed_available = false;
        }
        (*lib_dev).left_handed_available = left_handed_available;
        if has_tablet_pad {
            let mut pad_button_codes = Vec::new();
            let mut pad_left_handed_available = true;
            let database = libwacom_database_new();
            if !database.is_null() {
                let wacom = libwacom_new_from_usbid(
                    database,
                    libc::c_int::from(input_id.vendor()),
                    libc::c_int::from(input_id.product()),
                    std::ptr::null_mut(),
                );
                if !wacom.is_null() {
                    pad_left_handed_available = libwacom_is_reversible(wacom) != 0;
                    for index in 0..libwacom_get_num_buttons(wacom).max(0) {
                        let code = libwacom_get_button_evdev_code(
                            wacom,
                            (b'A' + index as u8) as libc::c_char,
                        );
                        if code > 0
                            && code <= i32::from(u16::MAX)
                            && (*lib_dev).event_codes.contains(&(code as u16))
                        {
                            pad_button_codes.push(code as u16);
                        }
                    }
                    libwacom_destroy(wacom);
                }
                libwacom_database_destroy(database);
            }
            if pad_button_codes.is_empty() {
                for range in [0x100_u16..0x10a, 0x126..0x128, 0x130..0x136, 0x110..0x117] {
                    pad_button_codes
                        .extend(range.filter(|code| (*lib_dev).event_codes.contains(code)));
                }
            }
            (*lib_dev).tablet_pad_button_codes = pad_button_codes;
            (*lib_dev).left_handed_available = pad_left_handed_available;
            (*lib_dev).tablet_pad_num_dials = u32::from(rel.is_some_and(|axes| {
                axes.contains(RelativeAxisCode::REL_WHEEL)
                    || axes.contains(RelativeAxisCode::REL_DIAL)
            })) + u32::from(
                rel.is_some_and(|axes| axes.contains(RelativeAxisCode::REL_HWHEEL)),
            );
            (*lib_dev).tablet_pad_num_rings =
                u32::from(abs.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_WHEEL)))
                    + u32::from(
                        abs.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_THROTTLE)),
                    );
            (*lib_dev).tablet_pad_num_strips =
                u32::from(abs.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_RX)))
                    + u32::from(abs.is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_RY)));
            (*lib_dev).tablet_pad_mode_group =
                Box::into_raw(Box::new(LibinputTabletPadModeGroup {
                    refcount: std::sync::atomic::AtomicI32::new(1),
                    user_data: std::ptr::null_mut(),
                    device: lib_dev,
                    index: 0,
                    num_modes: if input_id.vendor() == 0x056a && input_id.product() == 0x034e {
                        4
                    } else {
                        1
                    },
                    current_mode: 0,
                    num_buttons: (*lib_dev).tablet_pad_button_codes.len() as u32,
                    num_dials: (*lib_dev).tablet_pad_num_dials,
                    num_rings: (*lib_dev).tablet_pad_num_rings,
                    num_strips: (*lib_dev).tablet_pad_num_strips,
                }));
        }
        let has_left_button = (*lib_dev).event_codes.contains(&KeyCode::BTN_LEFT.0);
        let has_right_button = (*lib_dev).event_codes.contains(&KeyCode::BTN_RIGHT.0);
        let has_middle_button = (*lib_dev).event_codes.contains(&KeyCode::BTN_MIDDLE.0);
        let is_clickpad = props.contains(evdev::PropType::BUTTONPAD);
        let mut click_methods = 0;
        if is_clickpad {
            click_methods |= 1;
            if has_mt_xy {
                click_methods |= 2;
            }
        }
        if applied_quirks.model_apple_touchpad_onebutton {
            click_methods |= 2;
        }
        let clickfinger_is_default = applied_quirks.model_apple_touchpad
            || applied_quirks.model_apple_touchpad_onebutton
            || applied_quirks.model_clickfinger_default
            || (is_clickpad && input_id.vendor() == 0x05ac);
        let click_default_method = if clickfinger_is_default && click_methods & 2 != 0 {
            2
        } else if click_methods & 1 != 0 {
            1
        } else {
            0
        };
        (*lib_dev).click_methods = click_methods;
        (*lib_dev).click_method = click_default_method;
        (*lib_dev).click_default_method = click_default_method;
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
        if is_touchpad && input_id.vendor() == 0x05ac {
            // Apple touchpads opt in to natural scrolling by default.
            (*lib_dev).natural_scroll = true;
        }
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
        let touchpad_integration =
            crate::udev::property_value((*lib_dev).udev_device, "ID_INPUT_TOUCHPAD_INTEGRATION");
        // Match upstream's touchpad integration classification. Bluetooth
        // touchpads without an explicit udev classification are external;
        // otherwise the absence of the property defaults to internal. The
        // Logitech and Wacom-touchpad exceptions are quirk/device-model
        // based, rather than a blanket vendor exclusion.
        let mut external_touchpad = match touchpad_integration.as_deref() {
            Some("internal") => false,
            Some("external") => true,
            _ => input_id.bus_type().0 == 0x05,
        };
        external_touchpad |= input_id.vendor() == 0x046d || applied_quirks.model_wacom_touchpad;
        // DWT is available on internal touchpads and on external keyboard /
        // touchpad combinations explicitly identified by the quirks database.
        // DWTP is only meaningful for an internal touchpad paired with a
        // trackpoint. Device-model quirks above classify tablet touch
        // surfaces as external where appropriate.
        let dwt_device = is_touchpad;
        (*lib_dev).dwt_available =
            dwt_device && (!external_touchpad || applied_quirks.tpkb_combo_layout_below);
        (*lib_dev).dwt_enabled = (*lib_dev).dwt_available;
        (*lib_dev).dwt_timeout = 500;
        (*lib_dev).dwtp_available = dwt_device && !external_touchpad;
        (*lib_dev).dwtp_enabled = (*lib_dev).dwtp_available;
        (*lib_dev).dwtp_timeout = 300;
        (*lib_dev).send_events_modes = if is_touchpad && !external_touchpad {
            0b11
        } else {
            0b01
        };

        // The upstream palm detector enables pressure detection whenever a
        // touchpad exposes ABS_MT_PRESSURE, using 130 as its default
        // threshold. Size detection is deliberately opt-in through quirks.
        // External touchpads use neither detector unless they are an
        // explicitly identified keyboard/touchpad combination.
        let palm_detection_enabled =
            is_touchpad && (!external_touchpad || applied_quirks.tpkb_combo_layout_below);
        // INPUT_PROP_PRESSUREPAD (0x07) and a non-zero ABS_MT_PRESSURE
        // resolution mean the axis is contact/touch detection data rather
        // than the raw palm-pressure signal used by libinput's palm logic.
        let has_raw_mt_pressure = device.get_absinfo().ok().is_some_and(|axes| {
            axes.into_iter().any(|(axis, info)| {
                axis == AbsoluteAxisCode::ABS_MT_PRESSURE && info.resolution() == 0
            })
        }) && !props.contains(evdev::PropType(0x07));
        let palm_pressure_threshold = (palm_detection_enabled && has_raw_mt_pressure)
            .then_some(applied_quirks.palm_pressure_threshold.unwrap_or(130))
            .filter(|threshold| *threshold != 0);
        let palm_size_threshold = palm_detection_enabled
            .then_some(applied_quirks.palm_size_threshold)
            .flatten()
            .filter(|threshold| *threshold != 0);

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
                let supports_two_finger = (has_mt_slot && mt_slot_count > 1)
                    || (input_id.vendor() == 0x05ac && input_id.product() == 0x021a);
                (*lib_dev).scroll_methods = 2 | if supports_two_finger { 1 } else { 0 };
                let default_scroll_method = if supports_two_finger { 1 } else { 2 };
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
            is_typewriter_keyboard,
            matches!(
                applied_quirks.keyboard_integration,
                Some(crate::quirks::KeyboardIntegration::Internal)
            ),
            is_pointer,
            is_touchpad,
            external_touchpad,
            palm_pressure_threshold,
            palm_size_threshold,
            is_clickpad,
            is_topbuttonpad,
            tag_pointing_stick,
            input_id.vendor(),
            input_id.product(),
            input_id.bus_type().0,
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

        out.push(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_ADDED,
            payload: EventPayload::DeviceAdded,
            context: ctx,
            device: lib_dev,
        });
        if td.tablet_tool_type == 8 && td.tablet_proximity_pending == Some(true) {
            let mut initial_events = VecDeque::new();
            Self::process_tablet_tool_event(
                &InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0),
                systime_to_usec(SystemTime::now()),
                lib_dev,
                ctx,
                &mut td,
                &mut initial_events,
            );
            out.extend(initial_events);
        }
        self.devices.insert(fd, td);
        self.pair_new_dwt_device(fd);
    }

    pub unsafe fn stop_scroll_for_device(
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
        if tracked.scroll_button_axes != 0 {
            Self::emit_button_scroll_stops(time_usec, device, ctx, tracked, out);
        }
        tracked.scroll_button_down = false;
        tracked.scroll_lock_active = false;
        tracked.scroll_button_lock_press = false;
        tracked.scroll_button_press_time = None;
        tracked.scroll_button_moved = false;
        tracked.scroll_button_axes = 0;
        tracked.scroll_button_accum_x = 0;
        tracked.scroll_button_accum_y = 0;
        tracked.pending_rel_x = 0;
        tracked.pending_rel_y = 0;
        if tracked.finger_scroll_axes != 0 {
            Self::emit_finger_axis(
                time_usec,
                device,
                ctx,
                tracked.finger_scroll_axes,
                [0.0; 2],
                out,
            );
            tracked.finger_scroll_axes = 0;
        }
        let button_areas = (*tracked.lib_device).click_method == 1;
        let edge_gesture_contacts_before = tracked.gesture_tracked_contact_count(button_areas);
        let edge_axes = tracked.stop_edge_scroll();
        Self::emit_finger_axis(time_usec, device, ctx, edge_axes, [0.0; 2], out);
        let n_fingers = tracked.gesture_finger_count(button_areas);
        if edge_gesture_contacts_before != tracked.gesture_tracked_contact_count(button_areas) {
            tracked.rebase_gesture_contacts_after_eligibility_change(button_areas);
            tracked.sync_gesture_finger_count(n_fingers);
        }
        tracked.finger_scroll_constraint.reset();
        tracked.remainder_x = 0.0;
        tracked.remainder_y = 0.0;
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
        if let Some(button) = tracked.tap_button_down.take() {
            tracked.tap_release_since = None;
            tracked.tap_drag_active = false;
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
        if std::mem::take(&mut tracked.drag_3fg_button_down) {
            tracked.drag_3fg_active = false;
            tracked.drag_3fg_candidate_since = None;
            tracked.drag_3fg_candidate_time_usec = 0;
            tracked.drag_3fg_release_since = None;
            let seat_button_count = release_seat_button(device);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec,
                    button: KeyCode::BTN_LEFT.0 as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device,
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
            if tracked.tablet_tip_down {
                tracked.tablet_tip_down = false;
                tracked.tablet_tip_pending = None;
                (*tool)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_TIP,
                    payload: EventPayload::TabletTool(tablet_tool_payload(
                        tracked, device, time_usec, tool, 1,
                    )),
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
            (*tool).in_proximity = false;
            (*tool).eraser_button_mode = (*tool).wanted_eraser_button_mode;
            (*tool).eraser_button = (*tool).wanted_eraser_button;
            tracked.tablet_tool = std::ptr::null_mut();
            tracked.tablet_zero_pressure_since = None;
            tracked.tablet_buttons_down = 0;
        }
        if tracked.touch_active && (*device).has_touch {
            let had_reported_touch = tracked.mt_slots.iter().any(|slot| slot.reported)
                && !touch_arbitration_active(ctx, device, None);
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
            if had_reported_touch {
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
            let Some(since) = td.tablet_eraser_pen_out_since else {
                continue;
            };
            if since.elapsed() < Duration::from_millis(30) || td.tablet_tool.is_null() {
                continue;
            }
            let tool = td.tablet_tool;
            let time_usec = systime_to_usec(SystemTime::now());
            if td.tablet_eraser_pending_tip_up {
                td.tablet_tip_down = false;
                td.tablet_tip_pending = None;
                (*tool)
                    .refcount
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_TIP,
                    payload: EventPayload::TabletTool(tablet_tool_payload(
                        td,
                        td.lib_device,
                        time_usec,
                        tool,
                        1,
                    )),
                    context: ctx,
                    device: td.lib_device,
                });
            }
            (*tool)
                .refcount
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY,
                payload: EventPayload::TabletTool(tablet_tool_payload(
                    td,
                    td.lib_device,
                    time_usec,
                    tool,
                    0,
                )),
                context: ctx,
                device: td.lib_device,
            });
            (*tool).in_proximity = false;
            (*tool).eraser_button_mode = (*tool).wanted_eraser_button_mode;
            (*tool).eraser_button = (*tool).wanted_eraser_button;
            td.tablet_tool = std::ptr::null_mut();
            td.tablet_eraser_pen_out_since = None;
            td.tablet_eraser_pending_tip_up = false;
            (*td.lib_device).tablet_in_proximity = false;
            (*td.lib_device).area = (*td.lib_device).wanted_area;
        }
        for td in self.devices.values_mut() {
            if !td.tablet_proximity_timer_enabled || td.tablet_buttons_down != 0 {
                continue;
            }
            let Some(since) = td.tablet_zero_pressure_since else {
                continue;
            };
            if since.elapsed() < Duration::from_millis(150) {
                continue;
            }
            if td.tablet_tool.is_null() {
                if td.tablet_area_sequence_suppressed {
                    td.tablet_area_sequence_suppressed = false;
                    td.tablet_zero_pressure_since = None;
                    (*td.lib_device).tablet_in_proximity = false;
                    (*td.lib_device).area = (*td.lib_device).wanted_area;
                }
                continue;
            }
            if td.tablet_tip_down {
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
            (*td.lib_device).tablet_in_proximity = false;
            (*td.lib_device).area = (*td.lib_device).wanted_area;
        }
    }

    pub unsafe fn drain_into_queue(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        // --- hotplug ---
        self.handle_hotplug(ctx, out);

        // Expire per-touchpad DWT/DWTP timers before consuming a new touch
        // frame. This preserves the upstream distinction between a contact
        // that began while activity was active and one that begins afterward.
        self.expire_dwt_timeouts();

        let quick_hold_timeout = Duration::from_millis(40);
        let hold_timeout = Duration::from_millis(180);
        let tap_release_timeout = Duration::from_millis(180);
        let drag_3fg_release_timeout = Duration::from_millis(700);
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

        for tracked in self.devices.values_mut() {
            tracked.expire_edge_scroll_timeouts();
        }

        // --- read events from every open device ---
        let pointing_stick_device = self
            .devices
            .values()
            .find(|tracked| tracked.is_pointing_stick)
            .map(|tracked| tracked.lib_device);
        // Process paired sources before touchpads so a keyboard or
        // trackpoint event and a following touch frame in the same dispatch
        // observe the new suppression state deterministically.
        let mut fds: Vec<RawFd> = self.devices.keys().copied().collect();
        fds.sort_by_key(|fd| {
            self.devices
                .get(fd)
                .map(|tracked| {
                    if tracked.is_keyboard {
                        0_u8
                    } else if tracked.is_pointing_stick {
                        1
                    } else {
                        2
                    }
                })
                .unwrap_or(3)
        });
        let mut dead_fds: Vec<RawFd> = Vec::new();

        for fd in fds {
            // fetch_events() may return a partial batch. Drain this device
            // before returning from dispatch so one libinput_dispatch() call
            // cannot split a queued gesture across client iterations.
            loop {
                let batch: Vec<InputEvent> = {
                    let Some(td) = self.devices.get_mut(&fd) else {
                        break;
                    };
                    match td.device.fetch_events() {
                        Ok(batch) => batch.collect(),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error)
                            if error.raw_os_error() == Some(nix::libc::ENODEV)
                                || error.kind() == std::io::ErrorKind::UnexpectedEof =>
                        {
                            dead_fds.push(fd);
                            break;
                        }
                        Err(_) => break,
                    }
                };
                if batch.is_empty() {
                    break;
                }

                let mut followups = Vec::new();
                if let Some(td) = self.devices.get_mut(&fd) {
                    Self::process_device_batch(
                        fd,
                        &batch,
                        pointing_stick_device,
                        ctx,
                        td,
                        out,
                        &mut followups,
                    );
                }
                // The source borrow has ended. Applying follow-ups here lets
                // a source event mutate its paired touchpad without aliasing
                // the HashMap entry currently being read.
                for followup in followups {
                    match followup {
                        DwtFollowup::Keyboard {
                            source_fd,
                            code,
                            value,
                            at,
                            time_usec,
                        } => self.apply_dwt_keyboard_event(
                            source_fd, code, value, at, time_usec, ctx, out,
                        ),
                        DwtFollowup::TrackpointActivity {
                            source_fd,
                            at,
                            time_usec,
                        } => self.apply_dwtp_activity(source_fd, at, time_usec, ctx, out),
                    }
                }
            }
        }

        // Resolve hold contact-count changes after the complete read batch so
        // a rapid all-fingers lift ends normally rather than looking like a
        // sequence of cancelled partial lifts.
        for tracked in self.devices.values_mut() {
            if !std::mem::take(&mut tracked.hold_contact_changed) {
                continue;
            }
            let fingers =
                tracked.gesture_finger_count((*tracked.lib_device).click_method == 1) as i32;
            if tracked.hold_active && (fingers == 0 || fingers != tracked.hold_fingers) {
                let cancelled = fingers != 0;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_END,
                    payload: EventPayload::GestureHoldEnd(GestureEvent {
                        time_usec: systime_to_usec(SystemTime::now()),
                        finger_count: tracked.hold_fingers,
                        dx: 0.0,
                        dy: 0.0,
                        scale: 1.0,
                        angle: 0.0,
                        cancelled,
                    }),
                    context: ctx,
                    device: tracked.lib_device,
                });
                tracked.hold_active = false;
                tracked.hold_started_at = None;
                tracked.hold_blocked = cancelled;
            }
            if fingers == 0 {
                tracked.hold_started_at = None;
                tracked.hold_blocked = false;
                tracked.hold_fingers = 0;
            }
        }

        // Device input wins over an expiring hold timer. This ordering lets a
        // contact already queued by the kernel join the hold before its begin
        // event is emitted.
        for tracked in self.devices.values_mut() {
            // A one-finger contact that has already travelled past the
            // hold-and-motion threshold remains pointer motion. Upstream
            // only promotes an in-motion single finger to a hold when it is
            // still within 0.5 mm of its gesture origin at timer expiry.
            // Without this check, a normal slow pointer move can leak a
            // hold-begin event after the quick-hold timeout.
            let button_areas = (*tracked.lib_device).click_method == 1;
            let single_finger_moved_past_hold_threshold = tracked.hold_fingers == 1
                && tracked
                    .gesture_contact_moved_at_least(
                        button_areas,
                        (*tracked.lib_device).abs_x_resolution,
                        (*tracked.lib_device).abs_y_resolution,
                        (*tracked.lib_device).abs_x_range,
                        (*tracked.lib_device).abs_y_range,
                        (*tracked.lib_device).width_mm,
                        (*tracked.lib_device).height_mm,
                        0.5,
                    )
                    .unwrap_or(false);
            if single_finger_moved_past_hold_threshold {
                tracked.hold_started_at = None;
                tracked.hold_blocked = true;
                continue;
            }
            let timeout = if tracked.hold_fingers <= 2 {
                quick_hold_timeout
            } else {
                hold_timeout
            };
            if !(*tracked.lib_device).has_gesture
                || tracked.hold_active
                || tracked.hold_blocked
                || !tracked
                    .hold_started_at
                    .is_some_and(|start| start.elapsed() >= timeout)
            {
                continue;
            }
            let active_fingers =
                tracked.gesture_finger_count((*tracked.lib_device).click_method == 1) as i32;
            if active_fingers == 0 {
                tracked.hold_started_at = None;
                continue;
            }
            let fingers = tracked.hold_fingers.max(active_fingers);
            tracked.hold_started_at = None;
            tracked.hold_active = true;
            tracked.hold_fingers = fingers;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_BEGIN,
                payload: EventPayload::GestureHoldBegin(GestureEvent {
                    time_usec: systime_to_usec(SystemTime::now()),
                    finger_count: fingers,
                    dx: 0.0,
                    dy: 0.0,
                    scale: 1.0,
                    angle: 0.0,
                    cancelled: false,
                }),
                context: ctx,
                device: tracked.lib_device,
            });
        }

        // A second contact arriving before this deadline converts the tap
        // into a drag and clears tap_release_since while input is processed.
        // Only release here, after input has had that opportunity.
        for tracked in self.devices.values_mut() {
            if !tracked
                .tap_release_since
                .is_some_and(|since| since.elapsed() >= tap_release_timeout)
            {
                continue;
            }
            tracked.tap_release_since = None;
            tracked.tap_drag_active = false;
            let Some(button) = tracked.tap_button_down.take() else {
                continue;
            };
            let seat_button_count = release_seat_button(tracked.lib_device);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec: systime_to_usec(SystemTime::now()),
                    button: button as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device: tracked.lib_device,
            });
        }

        for tracked in self.devices.values_mut() {
            if !tracked
                .drag_3fg_release_since
                .is_some_and(|since| since.elapsed() >= drag_3fg_release_timeout)
            {
                continue;
            }
            tracked.drag_3fg_release_since = None;
            tracked.drag_3fg_active = false;
            if !std::mem::take(&mut tracked.drag_3fg_button_down) {
                continue;
            }
            let seat_button_count = release_seat_button(tracked.lib_device);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec: systime_to_usec(SystemTime::now()),
                    button: KeyCode::BTN_LEFT.0 as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device: tracked.lib_device,
            });
        }

        // A tablet may become active after a paired touchscreen or touchpad
        // frame was already read in this dispatch cycle. Preserve the
        // arbitration decision on the existing contact, otherwise a touch
        // that began underneath a pen can resume once the pen leaves
        // proximity.
        for td in self.devices.values_mut() {
            let lib_dev = td.lib_device;
            let is_gesture_touchpad = (*lib_dev).has_gesture;
            let tablet_is_active = ((*lib_dev).has_touch || is_gesture_touchpad)
                && active_tablet_for_touch(ctx, lib_dev).is_some();
            let tablet_just_activated = tablet_is_active && !td.touch_arbitration_tablet_was_active;
            td.touch_arbitration_tablet_was_active = tablet_is_active;
            if !tablet_just_activated
                || !td.mt_slots.iter().any(|slot| {
                    slot.active && touch_arbitration_active(ctx, lib_dev, Some((slot.x, slot.y)))
                })
            {
                continue;
            }
            td.touch_arbitration_suppressed = true;
            let time_usec = systime_to_usec(SystemTime::now());
            if is_gesture_touchpad {
                Self::stop_touchpad_actions(td, time_usec, ctx, out);
                for slot in &mut td.mt_slots {
                    slot.palm_suppressed = slot.active;
                    slot.reported = false;
                    slot.seat_slot = None;
                    slot.dirty = false;
                }
                td.touch_active = td.mt_slots.iter().any(|slot| slot.active);
                continue;
            }
            let mut cancelled = false;
            for (slot_index, slot) in td.mt_slots.iter_mut().enumerate() {
                if !slot.reported {
                    continue;
                }
                let seat_slot = slot.seat_slot.take().unwrap_or(-1);
                release_touch_seat_slot(ctx, seat_slot);
                slot.reported = false;
                slot.palm_suppressed = slot.active;
                slot.dirty = false;
                cancelled = true;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TOUCH_CANCEL,
                    payload: EventPayload::TouchCancel(TouchEvent {
                        time_usec,
                        slot: slot_index as i32,
                        seat_slot,
                        x: slot.x,
                        y: slot.y,
                    }),
                    context: ctx,
                    device: lib_dev,
                });
            }
            if cancelled {
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TOUCH_FRAME,
                    payload: EventPayload::TouchFrame { time_usec },
                    context: ctx,
                    device: lib_dev,
                });
            }
        }

        // Process queued kernel frames before expiring tablet proximity.
        // A real BTN_TOOL_PEN=0 arriving at the deadline wins over the
        // fallback timer, just as it does in upstream's frame plugin.
        self.emit_forced_tablet_proximity_out(ctx, out);

        // Remove dead devices
        for fd in dead_fds {
            self.unpair_removed_dwt_device(fd);
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
        let next_tablet_timeout = self
            .devices
            .values()
            .filter(|tracked| {
                tracked.tablet_proximity_timer_enabled
                    && tracked.tablet_buttons_down == 0
                    && !tracked.tablet_tip_down
                    && (!tracked.tablet_tool.is_null() || tracked.tablet_area_sequence_suppressed)
            })
            .filter_map(|tracked| tracked.tablet_zero_pressure_since)
            .map(|since| Duration::from_millis(150).saturating_sub(since.elapsed()))
            .min();
        let next_eraser_timeout = self
            .devices
            .values()
            .filter_map(|tracked| tracked.tablet_eraser_pen_out_since)
            .map(|since| Duration::from_millis(30).saturating_sub(since.elapsed()))
            .min();
        let next_hold_timeout = self
            .devices
            .values()
            .filter(|tracked| !tracked.hold_active && !tracked.hold_blocked)
            .filter_map(|tracked| {
                tracked.hold_started_at.map(|since| {
                    let timeout = if tracked.hold_fingers <= 2 {
                        quick_hold_timeout
                    } else {
                        hold_timeout
                    };
                    timeout.saturating_sub(since.elapsed())
                })
            })
            .min();
        let next_tap_timeout = self
            .devices
            .values()
            .filter_map(|tracked| tracked.tap_release_since)
            .map(|since| tap_release_timeout.saturating_sub(since.elapsed()))
            .min();
        let next_drag_3fg_timeout = self
            .devices
            .values()
            .filter_map(|tracked| tracked.drag_3fg_release_since)
            .map(|since| drag_3fg_release_timeout.saturating_sub(since.elapsed()))
            .min();
        let next_edge_scroll_timeout = self
            .devices
            .values()
            .flat_map(|tracked| tracked.mt_slots.iter())
            .filter(|slot| slot.edge_state == EdgeScrollState::New)
            .filter_map(|slot| slot.edge_started_at)
            .map(|since| Duration::from_millis(300).saturating_sub(since.elapsed()))
            .min();
        let timer_now = Instant::now();
        let next_dwt_timeout = self
            .devices
            .values()
            .flat_map(|tracked| [tracked.dwt.deadline, tracked.dwtp.deadline])
            .flatten()
            .map(|deadline| deadline.saturating_duration_since(timer_now))
            .min();
        let next_timeout = [
            next_middle_timeout,
            next_debounce_timeout,
            next_tablet_timeout,
            next_eraser_timeout,
            next_hold_timeout,
            next_tap_timeout,
            next_drag_3fg_timeout,
            next_edge_scroll_timeout,
            next_dwt_timeout,
        ]
        .into_iter()
        .flatten()
        .min();
        (*ctx).arm_timer(next_timeout);
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
            self.unpair_removed_dwt_device(fd);
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
        self.unpair_removed_dwt_device(fd);
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

    #[allow(clippy::too_many_arguments)]
    unsafe fn process_device_batch(
        fd: RawFd,
        batch: &[InputEvent],
        pointing_stick_device: Option<*mut LibinputDevice>,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
        followups: &mut Vec<DwtFollowup>,
    ) {
        let lib_dev = td.lib_device;
        let is_abs = td.is_absolute;
        let is_kbd = td.is_keyboard;
        let is_ptr = td.is_pointer;
        let is_tablet_tool = (*lib_dev).has_tablet
            && !(*lib_dev).has_touch
            && !(*lib_dev).has_gesture
            && !(*lib_dev).has_tablet_pad;
        let is_tablet_pad = (*lib_dev).has_tablet_pad;
        let cfg_tap = (*lib_dev).tap_enabled;
        let cfg_nat = (*lib_dev).natural_scroll;
        let cfg_accel = (*lib_dev).accel_speed as f32 + 1.0;

        if (*lib_dev).send_events_mode == 1 {
            if td.is_topbuttonpad {
                for event in batch {
                    Self::process_disabled_topbutton_event(
                        event,
                        systime_to_usec(event.timestamp()),
                        pointing_stick_device,
                        ctx,
                        td,
                        out,
                    );
                }
            }
            return;
        }

        for event in batch {
            let ts_usec = systime_to_usec(event.timestamp());

            if is_tablet_pad {
                Self::process_tablet_pad_event(event, ts_usec, lib_dev, ctx, td, out);
                continue;
            }

            if is_tablet_tool {
                Self::process_tablet_tool_event(event, ts_usec, lib_dev, ctx, td, out);
                continue;
            }

            // Composite gaming mice and receiver nodes may expose both
            // relative pointer axes and ordinary keyboard keys. Route the
            // key range through the keyboard engine without stealing the
            // device's BTN_* events from pointer handling.
            if is_kbd && is_ptr && event.event_type() == EventType::KEY && event.code() < 0x100 {
                Self::process_keyboard_event(event, ts_usec, lib_dev, ctx, td, out);
                if matches!(event.value(), 0 | 1) {
                    followups.push(DwtFollowup::Keyboard {
                        source_fd: fd,
                        code: event.code(),
                        value: event.value(),
                        at: Instant::now(),
                        time_usec: ts_usec,
                    });
                }
                continue;
            }

            if td.is_absolute_pointer {
                Self::process_absolute_pointer_event(event, ts_usec, lib_dev, ctx, td, out);
                continue;
            }

            if is_kbd && !is_abs && !is_ptr {
                Self::process_keyboard_event(event, ts_usec, lib_dev, ctx, td, out);
                if matches!(event.value(), 0 | 1) {
                    followups.push(DwtFollowup::Keyboard {
                        source_fd: fd,
                        code: event.code(),
                        value: event.value(),
                        at: Instant::now(),
                        time_usec: ts_usec,
                    });
                }
                continue;
            }

            if !is_abs && is_ptr {
                // The upstream listener receives one logical pointer event
                // per SYN frame. Count a trackpoint frame once, never each
                // raw REL_X/REL_Y record.
                let trackpoint_motion_frame = td.is_pointing_stick
                    && event.event_type() == EventType::SYNCHRONIZATION
                    && event.code() == 0
                    && (td.pending_rel_x != 0 || td.pending_rel_y != 0);
                Self::process_relative_event(event, ts_usec, lib_dev, ctx, td, out);
                if trackpoint_motion_frame {
                    followups.push(DwtFollowup::TrackpointActivity {
                        source_fd: fd,
                        at: Instant::now(),
                        time_usec: ts_usec,
                    });
                }
                continue;
            }

            if is_abs {
                let touch_arbitrated = touch_arbitration_active(ctx, lib_dev, None);
                Self::process_absolute_event(
                    event,
                    ts_usec,
                    lib_dev,
                    pointing_stick_device,
                    ctx,
                    td,
                    out,
                    cfg_tap,
                    cfg_nat,
                    cfg_accel,
                    touch_arbitrated,
                );
            }
        }
    }

    unsafe fn stop_touchpad_actions(
        td: &mut TrackedDevice,
        time_usec: u64,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let lib_dev = td.lib_device;
        if td.finger_scroll_axes != 0 {
            Self::emit_finger_axis(
                time_usec,
                lib_dev,
                ctx,
                td.finger_scroll_axes,
                [0.0; 2],
                out,
            );
            td.finger_scroll_axes = 0;
        }
        let edge_axes = td.stop_edge_scroll();
        Self::emit_finger_axis(time_usec, lib_dev, ctx, edge_axes, [0.0; 2], out);

        if td.pinch_active {
            td.pinch_active = false;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_END,
                payload: EventPayload::GesturePinchEnd(GestureEvent {
                    time_usec,
                    finger_count: td.pinch_fingers,
                    dx: 0.0,
                    dy: 0.0,
                    scale: 1.0,
                    angle: 0.0,
                    cancelled: true,
                }),
                context: ctx,
                device: lib_dev,
            });
        }
        if td.swipe_active {
            td.swipe_active = false;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_END,
                payload: EventPayload::GestureSwipeEnd(GestureEvent {
                    time_usec,
                    finger_count: td.swipe_fingers,
                    dx: 0.0,
                    dy: 0.0,
                    scale: 1.0,
                    angle: 0.0,
                    cancelled: true,
                }),
                context: ctx,
                device: lib_dev,
            });
        }
        if td.hold_active {
            td.hold_active = false;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_END,
                payload: EventPayload::GestureHoldEnd(GestureEvent {
                    time_usec,
                    finger_count: td.hold_fingers,
                    dx: 0.0,
                    dy: 0.0,
                    scale: 1.0,
                    angle: 0.0,
                    cancelled: true,
                }),
                context: ctx,
                device: lib_dev,
            });
        }
        if let Some(button) = td.tap_button_down.take() {
            td.tap_release_since = None;
            td.tap_drag_active = false;
            let seat_button_count = release_seat_button(lib_dev);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec,
                    button: button as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device: lib_dev,
            });
        }
        if std::mem::take(&mut td.drag_3fg_button_down) {
            td.drag_3fg_active = false;
            td.drag_3fg_release_since = None;
            let seat_button_count = release_seat_button(lib_dev);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                payload: EventPayload::PointerButton(PointerButtonEvent {
                    time_usec,
                    button: KeyCode::BTN_LEFT.0 as u32,
                    state: 0,
                    seat_button_count,
                }),
                context: ctx,
                device: lib_dev,
            });
        }
        td.hold_started_at = None;
        td.hold_blocked = true;
        td.hold_contact_changed = false;
        td.gesture_fingers_last = 0;
        td.drag_3fg_candidate_since = None;
        td.drag_3fg_candidate_time_usec = 0;
        td.current_dx = 0;
        td.current_dy = 0;
        td.remainder_x = 0.0;
        td.remainder_y = 0.0;
        td.finger_scroll_constraint.reset();
        // tp_tap_suspend() prevents a touch that was active at the start of
        // DWT/DWTP from turning into a delayed tap. Physical button events
        // deliberately take a separate path and remain usable.
        td.tap_emitted = true;
    }

    fn reset_touchpad_sequence_after_arbitration(td: &mut TrackedDevice) {
        // Arbitration ends the current physical contact sequence. Keep none
        // of its cancellation latches for the next contact.
        td.touch_fingers = 0;
        td.last_x = None;
        td.last_y = None;
        td.current_dx = 0;
        td.current_dy = 0;
        td.remainder_x = 0.0;
        td.remainder_y = 0.0;
        td.finger_scroll_axes = 0;
        td.finger_scroll_constraint.reset();
        td.tap_emitted = false;
        td.tap_fingers = 0;
        td.touch_start_time = None;
        td.last_movement_time = None;
        td.pinch_base_centroid = None;
        td.gesture_last_centroid = None;
        td.pinch_base_dist = 0.0;
        td.pinch_base_angle = 0.0;
        td.pinch_fingers = 0;
        td.swipe_fingers = 0;
        td.hold_started_at = None;
        td.hold_active = false;
        td.hold_fingers = 0;
        td.hold_blocked = false;
        td.hold_contact_changed = false;
        td.gesture_fingers_last = 0;
        td.mt_contact_count_changed = false;
        td.drag_3fg_candidate_since = None;
        td.drag_3fg_candidate_time_usec = 0;
    }

    // This dispatch boundary carries the event fields and output queue needed
    // to update every paired touchpad atomically.
    #[allow(clippy::too_many_arguments)]
    unsafe fn apply_dwt_keyboard_event(
        &mut self,
        source_fd: RawFd,
        code: u16,
        value: i32,
        at: Instant,
        time_usec: u64,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let targets: Vec<RawFd> = self
            .devices
            .iter()
            .filter_map(|(fd, touchpad)| {
                touchpad
                    .dwt
                    .paired_keyboards
                    .contains(&source_fd)
                    .then_some(*fd)
            })
            .collect();

        for target_fd in targets {
            let Some(touchpad) = self.devices.get_mut(&target_fd) else {
                continue;
            };
            if time_usec != 0 {
                touchpad.dwt.event_clock_anchor_at = Some(at);
                touchpad.dwt.event_clock_anchor_usec = Some(time_usec);
            }
            if value == 0 {
                // Releases clear both masks even while DWT is disabled.
                touchpad.dwt.pressed_keys.remove(&code);
                touchpad.dwt.modifier_keys.remove(&code);
                continue;
            }
            if value != 1 || !(*touchpad.lib_device).dwt_enabled || dwt_key_is_ignored(code) {
                continue;
            }
            if dwt_key_is_modifier(code) {
                if !dwt_key_is_shift(code) {
                    touchpad.dwt.modifier_keys.insert(code);
                }
                continue;
            }
            // Modifier chords such as Ctrl+click must remain responsive when
            // DWT is not already active. Shift intentionally does not enter
            // this mask, so Shift+letter is treated as typing.
            if !touchpad.dwt.active && !touchpad.dwt.modifier_keys.is_empty() {
                continue;
            }

            let first_key = !touchpad.dwt.active;
            touchpad.dwt.active = true;
            if first_key {
                touchpad.dwt.active_started_usec = (time_usec != 0).then_some(time_usec);
            }
            touchpad.dwt.pressed_keys.insert(code);
            touchpad.dwt.last_key_press_usec = (time_usec != 0).then_some(time_usec);
            let timeout_ms = if first_key {
                200
            } else {
                (*touchpad.lib_device).dwt_timeout as u64
            };
            touchpad.dwt.deadline = Some(at + Duration::from_millis(timeout_ms));
            touchpad.dwt.deadline_usec = (time_usec != 0)
                .then_some(time_usec.saturating_add(timeout_ms.saturating_mul(1_000)));
            if first_key {
                let mut suspension_events = VecDeque::new();
                Self::stop_touchpad_actions(touchpad, time_usec, ctx, &mut suspension_events);
                // A paired keyboard listener interrupts touchpad actions
                // before the key event reaches clients. The source keyboard
                // batch was already decoded, so prepend the cancellation
                // sequence to preserve that observable ordering.
                while let Some(event) = suspension_events.pop_back() {
                    out.push_front(event);
                }
            }
        }
    }

    unsafe fn apply_dwtp_activity(
        &mut self,
        source_fd: RawFd,
        at: Instant,
        time_usec: u64,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let targets: Vec<RawFd> = self
            .devices
            .iter()
            .filter_map(|(fd, touchpad)| {
                (touchpad.dwtp.paired_trackpoint == Some(source_fd)).then_some(*fd)
            })
            .collect();

        for target_fd in targets {
            let Some(touchpad) = self.devices.get_mut(&target_fd) else {
                continue;
            };
            if !(*touchpad.lib_device).dwtp_enabled {
                continue;
            }
            let activity_window_expired = (!touchpad.dwtp.active
                && touchpad
                    .dwtp
                    .deadline
                    .is_some_and(|deadline| at >= deadline))
                || (!touchpad.dwtp.active
                    && time_usec != 0
                    && touchpad
                        .dwtp
                        .deadline_usec
                        .is_some_and(|deadline| time_usec > deadline));
            if activity_window_expired {
                touchpad.dwtp.event_count = 0;
                touchpad.dwtp.deadline = None;
                touchpad.dwtp.deadline_usec = None;
            }
            touchpad.dwtp.last_event_usec = (time_usec != 0).then_some(time_usec);
            touchpad.dwtp.event_count = touchpad.dwtp.event_count.saturating_add(1);
            if touchpad.dwtp.event_count < 3 {
                touchpad.dwtp.deadline = Some(at + Duration::from_millis(40));
                touchpad.dwtp.deadline_usec =
                    (time_usec != 0).then_some(time_usec.saturating_add(40_000));
                continue;
            }

            let first_activity = !touchpad.dwtp.active;
            touchpad.dwtp.active = true;
            if first_activity {
                touchpad.dwtp.active_started_usec = (time_usec != 0).then_some(time_usec);
            }
            touchpad.dwtp.deadline =
                Some(at + Duration::from_millis((*touchpad.lib_device).dwtp_timeout as u64));
            touchpad.dwtp.deadline_usec = (time_usec != 0).then_some(
                time_usec.saturating_add((*touchpad.lib_device).dwtp_timeout as u64 * 1_000),
            );
            if first_activity {
                let mut suspension_events = VecDeque::new();
                Self::stop_touchpad_actions(touchpad, time_usec, ctx, &mut suspension_events);
                while let Some(event) = suspension_events.pop_back() {
                    out.push_front(event);
                }
            }
        }
    }

    unsafe fn expire_dwt_timeouts(&mut self) {
        let now = Instant::now();
        for touchpad in self.devices.values_mut() {
            if touchpad
                .dwt
                .deadline
                .is_some_and(|deadline| now >= deadline)
            {
                if touchpad.dwt.active
                    && (*touchpad.lib_device).dwt_enabled
                    && !touchpad.dwt.pressed_keys.is_empty()
                {
                    let now_usec = estimate_event_time(
                        touchpad.dwt.event_clock_anchor_at,
                        touchpad.dwt.event_clock_anchor_usec,
                        now,
                    );
                    touchpad.dwt.deadline = Some(
                        now + Duration::from_millis((*touchpad.lib_device).dwt_timeout as u64),
                    );
                    if let Some(now_usec) = now_usec {
                        touchpad.dwt.deadline_usec = Some(
                            now_usec
                                .saturating_add((*touchpad.lib_device).dwt_timeout as u64 * 1_000),
                        );
                        touchpad.dwt.last_key_press_usec = Some(now_usec);
                    }
                } else {
                    touchpad.dwt.active = false;
                    touchpad.dwt.deadline = None;
                    touchpad.dwt.active_started_usec = None;
                    touchpad.dwt.deadline_usec = None;
                    // Mirrors tp_tap_resume(). The candidate's tap engine is
                    // contact-scoped, so clearing the timer is sufficient;
                    // pre-existing palms stay marked until a later update.
                }
            }
            if touchpad
                .dwtp
                .deadline
                .is_some_and(|deadline| now >= deadline)
            {
                touchpad.dwtp.active = false;
                touchpad.dwtp.event_count = 0;
                touchpad.dwtp.deadline = None;
                touchpad.dwtp.active_started_usec = None;
                touchpad.dwtp.deadline_usec = None;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard event processing
    // -----------------------------------------------------------------------

    unsafe fn process_tablet_pad_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        let payload = |time_usec, device: *mut LibinputDevice| TabletPadEvent {
            time_usec,
            button: 0,
            button_state: 0,
            key: 0,
            key_state: 0,
            dial_delta_v120: 0.0,
            dial_number: 0,
            mode: if (*device).tablet_pad_mode_group.is_null() {
                0
            } else {
                (*(*device).tablet_pad_mode_group).current_mode
            },
            mode_group: (*device).tablet_pad_mode_group,
            ring_number: 0,
            ring_position: 0.0,
            ring_source: 0,
            strip_number: 0,
            strip_position: 0.0,
            strip_source: 0,
        };

        if ev.event_type() == EventType::KEY {
            if ev.value() == 2 || ev.code() == KeyCode::BTN_STYLUS.0 {
                return;
            }
            let state = u32::from(ev.value() != 0);
            if (*lib_dev).vendor_id == 0x056a && matches!(ev.code(), 0x240 | 0x243 | 0x278) {
                let mut event = payload(ts_usec, lib_dev);
                event.key = u32::from(ev.code());
                event.key_state = state;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_KEY,
                    payload: EventPayload::TabletPad(event),
                    context: ctx,
                    device: lib_dev,
                });
                return;
            }
            let Some(button) = (*lib_dev)
                .tablet_pad_button_codes
                .iter()
                .position(|code| *code == ev.code())
            else {
                return;
            };
            if state == 1 && (*lib_dev).vendor_id == 0x056a && (*lib_dev).product_id == 0x034e {
                let mode = match ev.code() {
                    0x109 => Some(0),
                    0x130 => Some(1),
                    0x131 => Some(2),
                    0x132 => Some(3),
                    _ => None,
                };
                if let Some(mode) = mode {
                    (*(*lib_dev).tablet_pad_mode_group).current_mode = mode;
                }
            }
            let mut event = payload(ts_usec, lib_dev);
            event.button = button as u32;
            event.button_state = state;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_BUTTON,
                payload: EventPayload::TabletPad(event),
                context: ctx,
                device: lib_dev,
            });
            return;
        }

        if ev.event_type() == EventType::RELATIVE {
            match ev.code() {
                code if code == RelativeAxisCode::REL_DIAL.0 => {
                    td.pad_dial_values[0] = Some(f64::from(ev.value()) * 120.0);
                }
                code if code == RelativeAxisCode::REL_WHEEL.0 => {
                    if !td.supports_hi_res_vertical {
                        td.pad_dial_values[0] = Some(f64::from(-ev.value()) * 120.0);
                    }
                }
                code if code == RelativeAxisCode::REL_HWHEEL.0 => {
                    if !td.supports_hi_res_horizontal {
                        td.pad_dial_values[1] = Some(f64::from(ev.value()) * 120.0);
                    }
                }
                code if code == RelativeAxisCode::REL_WHEEL_HI_RES.0 => {
                    td.pad_dial_values[0] = Some(f64::from(-ev.value()));
                }
                code if code == RelativeAxisCode::REL_HWHEEL_HI_RES.0 => {
                    td.pad_dial_values[1] = Some(f64::from(ev.value()));
                }
                _ => {}
            }
            return;
        }

        if ev.event_type() == EventType::ABSOLUTE {
            let (axis_bit, slot) = if ev.code() == AbsoluteAxisCode::ABS_WHEEL.0 {
                (Some(1_u8), Some((&mut td.pad_ring_values[0], 0_usize)))
            } else if ev.code() == AbsoluteAxisCode::ABS_THROTTLE.0 {
                (Some(2), Some((&mut td.pad_ring_values[1], 1)))
            } else if ev.code() == AbsoluteAxisCode::ABS_RX.0 {
                (Some(4), Some((&mut td.pad_strip_values[0], 2)))
            } else if ev.code() == AbsoluteAxisCode::ABS_RY.0 {
                (Some(8), Some((&mut td.pad_strip_values[1], 3)))
            } else {
                (None, None)
            };
            if ev.code() == AbsoluteAxisCode::ABS_MISC.0 {
                td.pad_abs_misc_terminator = ev.value() == 0;
            } else if let (Some(bit), Some((value, _))) = (axis_bit, slot) {
                if td.pad_changed_axes & bit != 0 && ev.value() == 0 {
                    td.pad_changed_axes &= !bit;
                } else {
                    *value = ev.value();
                    td.pad_changed_axes |= bit;
                }
            }
            return;
        }

        if ev.event_type() != EventType::SYNCHRONIZATION || ev.code() != 0 {
            return;
        }

        for dial in 0..2 {
            if let Some(delta) = td.pad_dial_values[dial].take() {
                let mut event = payload(ts_usec, lib_dev);
                event.dial_number = dial as u32;
                event.dial_delta_v120 = delta;
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_DIAL,
                    payload: EventPayload::TabletPad(event),
                    context: ctx,
                    device: lib_dev,
                });
            }
        }
        for ring in 0..2 {
            let bit = 1 << ring;
            if td.pad_changed_axes & bit == 0 {
                continue;
            }
            let mut position = td.pad_ring_ranges[ring]
                .filter(|(minimum, maximum)| maximum > minimum)
                .map(|(minimum, maximum)| {
                    let mut normalized = f64::from(td.pad_ring_values[ring] - minimum)
                        / f64::from(maximum - minimum + 1)
                        - 0.25;
                    if normalized < 0.0 {
                        normalized += 1.0;
                    }
                    normalized * 360.0
                })
                .unwrap_or(0.0);
            if td.pad_abs_misc_terminator {
                position = -1.0;
            } else if (*lib_dev).left_handed {
                position = (position + 180.0) % 360.0;
            }
            let mut event = payload(ts_usec, lib_dev);
            event.ring_number = ring as u32;
            event.ring_position = position;
            event.ring_source = 2;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_RING,
                payload: EventPayload::TabletPad(event),
                context: ctx,
                device: lib_dev,
            });
        }
        for strip in 0..2 {
            let bit = 1 << (strip + 2);
            if td.pad_changed_axes & bit == 0 {
                continue;
            }
            let value = td.pad_strip_values[strip];
            let mut position = td.pad_strip_ranges[strip]
                .filter(|(minimum, maximum)| maximum > minimum)
                .map(|(minimum, maximum)| {
                    if value == 0 {
                        0.0
                    } else if (*lib_dev).vendor_id == 0x056a {
                        if maximum <= 1 {
                            0.0
                        } else {
                            f64::from(value).log2() / f64::from(maximum).log2()
                        }
                    } else {
                        f64::from(value - minimum) / f64::from(maximum - minimum)
                    }
                })
                .unwrap_or(0.0);
            if td.pad_abs_misc_terminator {
                position = -1.0;
            } else if (*lib_dev).left_handed {
                position = 1.0 - position;
            }
            let mut event = payload(ts_usec, lib_dev);
            event.strip_number = strip as u32;
            event.strip_position = position;
            event.strip_source = 2;
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_STRIP,
                payload: EventPayload::TabletPad(event),
                context: ctx,
                device: lib_dev,
            });
        }
        td.pad_changed_axes = 0;
        td.pad_abs_misc_terminator = false;
    }

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
            } else if ev.code() == AbsoluteAxisCode::ABS_MT_TOUCH_MAJOR.0 {
                td.tablet_size_major = f64::from(ev.value());
                td.tablet_size_major_changed = true;
            } else if ev.code() == AbsoluteAxisCode::ABS_MT_TOUCH_MINOR.0 {
                td.tablet_size_minor = f64::from(ev.value());
                td.tablet_size_minor_changed = true;
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
                let active = ev.value() >= 0;
                td.tablet_proximity_pending = Some(active);
                if active != td.tablet_tip_down {
                    td.tablet_tip_down = active;
                    td.tablet_tip_pending = Some(active);
                }
            }
            td.tablet_axes_changed = true;
            return;
        }
        if ev.event_type() == EventType::RELATIVE && ev.code() == RelativeAxisCode::REL_WHEEL.0 {
            td.tablet_wheel_discrete = -ev.value();
            td.tablet_wheel_delta =
                f64::from(td.tablet_wheel_discrete) * (*lib_dev).wheel_click_angle_vertical;
            td.tablet_wheel_changed = true;
            td.tablet_axes_changed = true;
            return;
        }
        if ev.event_type() == EventType::KEY {
            if ev.code() == KeyCode::BTN_TOUCH.0 {
                let tip_down = ev.value() != 0;
                if tip_down != td.tablet_tip_down {
                    td.tablet_tip_down = tip_down;
                    td.tablet_tip_pending = Some(tip_down);
                }
                td.tablet_touch_button_changed = true;
                return;
            }
            let is_tool_button = td.tablet_buttons.contains(&u32::from(ev.code()))
                && match td.tablet_tool_type {
                    8 => ev.code() == 0x100,
                    _ => {
                        matches!(ev.code(), 0x149 | 0x14b | 0x14c)
                            || (0x110..=0x117).contains(&ev.code())
                    }
                };
            if is_tool_button {
                let pressed = ev.value() != 0;
                if let Some(index) = td
                    .tablet_ignored_initial_buttons
                    .iter()
                    .position(|button| *button == u32::from(ev.code()))
                {
                    if !pressed {
                        td.tablet_ignored_initial_buttons.remove(index);
                    }
                    return;
                }
                if pressed {
                    if !td.tablet_held_buttons.contains(&u32::from(ev.code())) {
                        td.tablet_held_buttons.push(u32::from(ev.code()));
                    }
                } else {
                    td.tablet_held_buttons
                        .retain(|button| *button != u32::from(ev.code()));
                }
                td.tablet_buttons_down = td.tablet_held_buttons.len() as u32;
                td.tablet_pending_button_events
                    .push((u32::from(ev.code()), pressed));
                return;
            }
            let tool_type_for_key = |code| match code {
                code if code == KeyCode::BTN_TOOL_RUBBER.0 => Some(2),
                code if code == KeyCode::BTN_TOOL_BRUSH.0 => Some(3),
                code if code == KeyCode::BTN_TOOL_PENCIL.0 => Some(4),
                code if code == KeyCode::BTN_TOOL_AIRBRUSH.0 => Some(5),
                code if code == KeyCode::BTN_TOOL_MOUSE.0 => Some(6),
                code if code == KeyCode::BTN_TOOL_LENS.0 => Some(7),
                code if code == KeyCode::BTN_TOOL_PEN.0 => Some(1),
                _ => None,
            };
            let Some(event_tool_type) = tool_type_for_key(ev.code()) else {
                return;
            };
            let pressed = ev.value() != 0;
            if pressed {
                if !td.tablet_active_tool_keys.contains(&ev.code()) {
                    td.tablet_active_tool_keys.push(ev.code());
                }
            } else {
                td.tablet_active_tool_keys.retain(|code| *code != ev.code());
            }
            // Some tablets advertise BTN_TOOL_PEN but omit proximity-out.
            // Keep the watchdog until a real pen-out arrives. Other tool
            // types do not use the pen-only fallback.
            if event_tool_type != 1 || !pressed {
                td.tablet_proximity_timer_enabled = false;
                td.tablet_zero_pressure_since = None;
            }
            if pressed {
                let selected_tool_type = [
                    KeyCode::BTN_TOOL_RUBBER.0,
                    KeyCode::BTN_TOOL_BRUSH.0,
                    KeyCode::BTN_TOOL_PENCIL.0,
                    KeyCode::BTN_TOOL_AIRBRUSH.0,
                    KeyCode::BTN_TOOL_MOUSE.0,
                    KeyCode::BTN_TOOL_LENS.0,
                    KeyCode::BTN_TOOL_PEN.0,
                ]
                .into_iter()
                .find(|code| td.tablet_active_tool_keys.contains(code))
                .and_then(tool_type_for_key)
                .unwrap_or(event_tool_type);
                if selected_tool_type != td.tablet_tool_type
                    || (!(*lib_dev).tablet_in_proximity && td.tablet_tool.is_null())
                {
                    td.tablet_tool_type = selected_tool_type;
                    td.tablet_proximity_pending = Some(true);
                }
            } else if event_tool_type == td.tablet_tool_type {
                // A direct tool switch can leave BTN_TOOL_PEN asserted while
                // BTN_TOOL_RUBBER toggles. Releasing the current tool only
                // sends proximity-out; the still-asserted key does not select
                // a replacement until the kernel updates that tool again.
                td.tablet_proximity_pending = Some(false);
            }
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
        if td.tablet_area_sequence_suppressed
            && td
                .tablet_zero_pressure_since
                .is_some_and(|since| since.elapsed() >= Duration::from_millis(150))
        {
            td.tablet_area_sequence_suppressed = false;
            td.tablet_zero_pressure_since = None;
            (*lib_dev).tablet_in_proximity = false;
            (*lib_dev).area = (*lib_dev).wanted_area;
        }
        if td.tablet_proximity_timer_enabled
            && (!td.tablet_tool.is_null() || td.tablet_area_sequence_suppressed)
        {
            td.tablet_zero_pressure_since = Some(Instant::now());
        }
        let touch_button_changed = std::mem::take(&mut td.tablet_touch_button_changed);
        if matches!(td.tablet_tool_type, 6 | 7) {
            const CURSOR_PROXIMITY_THRESHOLD: f64 = 42.0;
            if td.tablet_distance >= CURSOR_PROXIMITY_THRESHOLD {
                if (*lib_dev).tablet_in_proximity {
                    td.tablet_proximity_pending = Some(false);
                    td.tablet_cursor_out_of_range = true;
                } else {
                    td.tablet_proximity_pending = None;
                    td.tablet_cursor_out_of_range = true;
                    td.tablet_x_changed = false;
                    td.tablet_y_changed = false;
                    td.tablet_pressure_changed = false;
                    td.tablet_distance_changed = false;
                    td.tablet_tilt_x_changed = false;
                    td.tablet_tilt_y_changed = false;
                    td.tablet_rotation_changed = false;
                    td.tablet_slider_changed = false;
                    td.tablet_size_major_changed = false;
                    td.tablet_size_minor_changed = false;
                    td.tablet_wheel_changed = false;
                    td.tablet_axes_changed = false;
                    return;
                }
            } else if td.tablet_distance > 0.0
                && td.tablet_cursor_out_of_range
                && !(*lib_dev).tablet_in_proximity
            {
                td.tablet_cursor_out_of_range = false;
                td.tablet_proximity_pending = Some(true);
            } else if td.tablet_distance == 0.0
                && td.tablet_cursor_out_of_range
                && td.tablet_proximity_pending == Some(false)
            {
                td.tablet_cursor_out_of_range = false;
                td.tablet_proximity_pending = None;
                td.tablet_x_changed = false;
                td.tablet_y_changed = false;
                td.tablet_pressure_changed = false;
                td.tablet_distance_changed = false;
                td.tablet_tilt_x_changed = false;
                td.tablet_tilt_y_changed = false;
                td.tablet_rotation_changed = false;
                td.tablet_slider_changed = false;
                td.tablet_size_major_changed = false;
                td.tablet_size_minor_changed = false;
                td.tablet_wheel_changed = false;
                td.tablet_axes_changed = false;
                return;
            }
        }
        // Tablets handled by the proximity watchdog may never expose a
        // BTN_TOOL_PEN key. For those devices, the first axis frame is the
        // only reliable indication that a pen is in range. The same rule
        // restores proximity after the watchdog synthesized an out event.
        if td.tablet_tool.is_null()
            && (td.tablet_x_changed
                || td.tablet_y_changed
                || (td.tablet_pressure_changed
                    && td
                        .tablet_pressure_range
                        .is_some_and(|(minimum, _)| td.tablet_pressure > f64::from(minimum))))
            && td.tablet_proximity_pending.is_none()
        {
            td.tablet_tool_type = 1;
            td.tablet_proximity_pending = Some(true);
        }
        let Some(in_proximity) = td.tablet_proximity_pending.take() else {
            update_tablet_pressure_offset(td, false);
            update_tablet_tip_from_pressure(td, touch_button_changed);
            let is_tip_event = td.tablet_tip_pending.take().is_some();
            let has_button_events = !td.tablet_pending_button_events.is_empty();
            if td.tablet_tool.is_null()
                || (!td.tablet_axes_changed && !is_tip_event && !has_button_events)
            {
                return;
            }
            let tool = td.tablet_tool;
            if td.tablet_axes_changed || is_tip_event {
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
            }
            for button_event in std::mem::take(&mut td.tablet_pending_button_events) {
                push_tablet_tool_button(td, lib_dev, ctx, out, ts_usec, tool, button_event);
            }
            td.tablet_last_event_x = td.tablet_x;
            td.tablet_last_event_y = td.tablet_y;
            td.tablet_last_event_pressure = td.tablet_pressure;
            td.tablet_last_event_distance = td.tablet_distance;
            td.tablet_last_event_tilt_x = td.tablet_tilt_x;
            td.tablet_last_event_tilt_y = td.tablet_tilt_y;
            td.tablet_last_event_rotation = td.tablet_rotation;
            td.tablet_last_event_slider = td.tablet_slider;
            td.tablet_last_event_size_major = td.tablet_size_major;
            td.tablet_last_event_size_minor = td.tablet_size_minor;
            td.tablet_x_changed = false;
            td.tablet_y_changed = false;
            td.tablet_pressure_changed = false;
            td.tablet_distance_changed = false;
            td.tablet_tilt_x_changed = false;
            td.tablet_tilt_y_changed = false;
            td.tablet_rotation_changed = false;
            td.tablet_slider_changed = false;
            td.tablet_size_major_changed = false;
            td.tablet_size_minor_changed = false;
            td.tablet_wheel_delta = 0.0;
            td.tablet_wheel_discrete = 0;
            td.tablet_wheel_changed = false;
            td.tablet_axes_changed = false;
            return;
        };
        let had_pending_eraser_pen_out =
            in_proximity && td.tablet_eraser_pen_out_since.take().is_some();
        if had_pending_eraser_pen_out && td.tablet_eraser_pending_tip_up {
            td.tablet_tip_down = true;
            td.tablet_tip_pending = None;
            td.tablet_eraser_pending_tip_up = false;
        }
        let was_in_proximity = (*lib_dev).tablet_in_proximity;
        if in_proximity && td.tablet_tool.is_null() && !was_in_proximity {
            (*ctx).touch_arbitration_until = None;
            td.tablet_left_handed_applied = paired_left_handed_rotation_enabled(ctx, lib_dev);
            (*lib_dev).area = (*lib_dev).wanted_area;
            (*lib_dev).tablet_in_proximity = true;
            if (*lib_dev).area_available {
                let area = (*lib_dev).area;
                let (raw_x_min, raw_x_max) = (*lib_dev).abs_x_range.unwrap_or((0, 0));
                let (raw_y_min, raw_y_max) = (*lib_dev).abs_y_range.unwrap_or((0, 0));
                let x_span = f64::from(raw_x_max - raw_x_min);
                let y_span = f64::from(raw_y_max - raw_y_min);
                let area_x_min = f64::from(raw_x_min) + (x_span * area[0]).trunc();
                let area_x_max = f64::from(raw_x_min) + (x_span * area[2]).trunc();
                let area_y_min = f64::from(raw_y_min) + (y_span * area[1]).trunc();
                let area_y_max = f64::from(raw_y_min) + (y_span * area[3]).trunc();
                let x_margin = (area_x_max - area_x_min) * 0.03;
                let y_margin = (area_y_max - area_y_min) * 0.03;
                td.tablet_area_sequence_suppressed = td.tablet_x < area_x_min - x_margin
                    || td.tablet_x > area_x_max + x_margin
                    || td.tablet_y < area_y_min - y_margin
                    || td.tablet_y > area_y_max + y_margin;
            }
        }
        if td.tablet_area_sequence_suppressed {
            if td.tablet_proximity_timer_enabled && td.tablet_zero_pressure_since.is_none() {
                td.tablet_zero_pressure_since = Some(Instant::now());
            }
            if !in_proximity {
                td.tablet_area_sequence_suppressed = false;
                td.tablet_tip_down = false;
                td.tablet_tip_pending = None;
                (*lib_dev).tablet_in_proximity = false;
                (*lib_dev).area = (*lib_dev).wanted_area;
            }
            td.tablet_x_changed = false;
            td.tablet_y_changed = false;
            td.tablet_pressure_changed = false;
            td.tablet_distance_changed = false;
            td.tablet_tilt_x_changed = false;
            td.tablet_tilt_y_changed = false;
            td.tablet_rotation_changed = false;
            td.tablet_slider_changed = false;
            td.tablet_size_major_changed = false;
            td.tablet_size_minor_changed = false;
            td.tablet_wheel_changed = false;
            td.tablet_axes_changed = false;
            return;
        }
        if !in_proximity
            && !td.tablet_tool.is_null()
            && (*td.tablet_tool).tool_type == 1
            && (*td.tablet_tool).eraser_button_mode == 1
        {
            if td.tablet_eraser_button_active {
                push_tablet_tool_button(
                    td,
                    lib_dev,
                    ctx,
                    out,
                    ts_usec,
                    td.tablet_tool,
                    ((*td.tablet_tool).eraser_button, false),
                );
                td.tablet_eraser_button_active = false;
            }
            td.tablet_eraser_pending_tip_up = td.tablet_tip_pending == Some(false);
            if td.tablet_eraser_pending_tip_up {
                td.tablet_tip_down = true;
                td.tablet_tip_pending = None;
            }
            td.tablet_eraser_pen_out_since = Some(Instant::now());
            td.tablet_x_changed = false;
            td.tablet_y_changed = false;
            td.tablet_pressure_changed = false;
            td.tablet_distance_changed = false;
            td.tablet_tilt_x_changed = false;
            td.tablet_tilt_y_changed = false;
            td.tablet_rotation_changed = false;
            td.tablet_slider_changed = false;
            td.tablet_size_major_changed = false;
            td.tablet_size_minor_changed = false;
            td.tablet_wheel_changed = false;
            td.tablet_axes_changed = false;
            (*ctx).arm_timer(Some(Duration::from_millis(30)));
            return;
        }
        let proximity_out_raw_axes = (!in_proximity).then_some((
            td.tablet_x,
            td.tablet_y,
            td.tablet_pressure,
            td.tablet_distance,
            td.tablet_tilt_x,
            td.tablet_tilt_y,
            td.tablet_rotation,
            td.tablet_slider,
            td.tablet_size_major,
            td.tablet_size_minor,
        ));
        let converts_eraser_to_button = in_proximity
            && td.tablet_tool_type == 2
            && !td.tablet_tool.is_null()
            && (*td.tablet_tool).tool_type == 1
            && (*td.tablet_tool).eraser_button_mode == 1;
        if in_proximity
            && !td.tablet_tool.is_null()
            && (*td.tablet_tool).tool_type != td.tablet_tool_type
            && !converts_eraser_to_button
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
            if td.tablet_tool.is_null() {
                td.tablet_left_handed_applied = paired_left_handed_rotation_enabled(ctx, lib_dev);
            }
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
            td.tablet_size_major_changed = td.tablet_has_size;
            td.tablet_size_minor_changed = td.tablet_has_size;
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
            (*tool).pressure_range_minimum = (*tool).wanted_pressure_range_minimum;
            (*tool).pressure_range_maximum = (*tool).wanted_pressure_range_maximum;
            tool
        } else {
            td.tablet_tool
        };
        if tool.is_null() {
            return;
        }
        if in_proximity {
            (*tool).in_proximity = true;
        }
        let eraser_enter = in_proximity
            && td.tablet_tool_type == 2
            && (*tool).tool_type == 1
            && (*tool).eraser_button_mode == 1
            && !td.tablet_eraser_button_active;
        let eraser_return_to_pen = in_proximity
            && td.tablet_tool_type == 1
            && (*tool).tool_type == 1
            && td.tablet_eraser_button_active;
        let suppress_proximity = was_in_proximity
            && (eraser_enter || eraser_return_to_pen || had_pending_eraser_pen_out);
        update_tablet_pressure_offset(td, in_proximity);
        update_tablet_tip_from_pressure(td, touch_button_changed);
        if !in_proximity {
            td.tablet_x = td.tablet_last_event_x;
            td.tablet_y = td.tablet_last_event_y;
            td.tablet_pressure = td.tablet_last_event_pressure;
            td.tablet_distance = td.tablet_last_event_distance;
            td.tablet_tilt_x = td.tablet_last_event_tilt_x;
            td.tablet_tilt_y = td.tablet_last_event_tilt_y;
            td.tablet_rotation = td.tablet_last_event_rotation;
            td.tablet_slider = td.tablet_last_event_slider;
            td.tablet_size_major = td.tablet_last_event_size_major;
            td.tablet_size_minor = td.tablet_last_event_size_minor;
            td.tablet_x_changed = false;
            td.tablet_y_changed = false;
            td.tablet_pressure_changed = false;
            td.tablet_distance_changed = false;
            td.tablet_tilt_x_changed = false;
            td.tablet_tilt_y_changed = false;
            td.tablet_rotation_changed = false;
            td.tablet_slider_changed = false;
            td.tablet_size_major_changed = false;
            td.tablet_size_minor_changed = false;
            td.tablet_wheel_changed = false;
        }
        if eraser_return_to_pen {
            push_tablet_tool_button(
                td,
                lib_dev,
                ctx,
                out,
                ts_usec,
                tool,
                ((*tool).eraser_button, false),
            );
            td.tablet_eraser_button_active = false;
        }
        if !in_proximity {
            for button_event in std::mem::take(&mut td.tablet_pending_button_events) {
                push_tablet_tool_button(td, lib_dev, ctx, out, ts_usec, tool, button_event);
            }
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
        let tip_before_proximity =
            !in_proximity && (td.tablet_tip_down || td.tablet_tip_pending == Some(false));
        if tip_before_proximity {
            td.tablet_tip_down = false;
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
        if !suppress_proximity {
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
        }
        if eraser_enter {
            push_tablet_tool_button(
                td,
                lib_dev,
                ctx,
                out,
                ts_usec,
                tool,
                ((*tool).eraser_button, true),
            );
            td.tablet_eraser_button_active = true;
        }
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
        if in_proximity {
            for button_event in std::mem::take(&mut td.tablet_pending_button_events) {
                push_tablet_tool_button(td, lib_dev, ctx, out, ts_usec, tool, button_event);
            }
        }
        if !in_proximity {
            (*tool).in_proximity = false;
            (*tool).eraser_button_mode = (*tool).wanted_eraser_button_mode;
            (*tool).eraser_button = (*tool).wanted_eraser_button;
            td.tablet_tool = std::ptr::null_mut();
            td.tablet_zero_pressure_since = None;
            (*ctx).touch_arbitration_until = Some(Instant::now() + Duration::from_millis(90));
            (*lib_dev).tablet_in_proximity = false;
            (*lib_dev).area = (*lib_dev).wanted_area;
        } else if td.tablet_proximity_timer_enabled && td.tablet_tool_type == 1 {
            td.tablet_zero_pressure_since = Some(Instant::now());
        }
        if let Some((
            raw_x,
            raw_y,
            raw_pressure,
            raw_distance,
            raw_tilt_x,
            raw_tilt_y,
            raw_rotation,
            raw_slider,
            raw_size_major,
            raw_size_minor,
        )) = proximity_out_raw_axes
        {
            td.tablet_x = raw_x;
            td.tablet_y = raw_y;
            td.tablet_pressure = raw_pressure;
            td.tablet_distance = raw_distance;
            td.tablet_tilt_x = raw_tilt_x;
            td.tablet_tilt_y = raw_tilt_y;
            td.tablet_rotation = raw_rotation;
            td.tablet_slider = raw_slider;
            td.tablet_size_major = raw_size_major;
            td.tablet_size_minor = raw_size_minor;
        }
        td.tablet_last_event_x = td.tablet_x;
        td.tablet_last_event_y = td.tablet_y;
        td.tablet_last_event_pressure = td.tablet_pressure;
        td.tablet_last_event_distance = td.tablet_distance;
        td.tablet_last_event_tilt_x = td.tablet_tilt_x;
        td.tablet_last_event_tilt_y = td.tablet_tilt_y;
        td.tablet_last_event_rotation = td.tablet_rotation;
        td.tablet_last_event_slider = td.tablet_slider;
        td.tablet_last_event_size_major = td.tablet_size_major;
        td.tablet_last_event_size_minor = td.tablet_size_minor;
        td.tablet_x_changed = false;
        td.tablet_y_changed = false;
        td.tablet_pressure_changed = false;
        td.tablet_distance_changed = false;
        td.tablet_tilt_x_changed = false;
        td.tablet_tilt_y_changed = false;
        td.tablet_rotation_changed = false;
        td.tablet_slider_changed = false;
        td.tablet_size_major_changed = false;
        td.tablet_size_minor_changed = false;
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
            // Key down: repeat tracking. DWT is applied after the source
            // batch releases its mutable device borrow, so it can update the
            // correctly paired touchpad rather than a global timestamp.
            td.held_keys.push(HeldKey { code });
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
                                        payload: EventPayload::PointerAxis(
                                            PointerAxisEvent::single(
                                                ts_usec,
                                                axis,
                                                value as f64,
                                                0,
                                                0.0,
                                                3,
                                            ),
                                        ),
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
                                payload: EventPayload::PointerAxis(PointerAxisEvent::single(
                                    ts_usec, axis, value, 0, 0.0, 3,
                                )),
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
                            payload: EventPayload::PointerAxis(PointerAxisEvent::single(
                                ts_usec,
                                axis,
                                value_v120 / 120.0 * click_angle,
                                (value_v120 / 120.0) as i32,
                                value_v120,
                                1,
                            )),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    if low_res != 0 {
                        let discrete = (low_res as f64 * direction) as i32;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
                            payload: EventPayload::PointerAxis(PointerAxisEvent::single(
                                ts_usec,
                                axis,
                                f64::from(discrete) * click_angle,
                                discrete,
                                f64::from(discrete) * 120.0,
                                1,
                            )),
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
                    payload: EventPayload::PointerAxis(PointerAxisEvent::single(
                        ts_usec, axis, 0.0, 0, 0.0, 3,
                    )),
                    context: ctx,
                    device: lib_dev,
                });
            }
        }
    }

    unsafe fn emit_finger_axis(
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        axes: u8,
        values: [f64; 2],
        out: &mut VecDeque<LibinputEvent>,
    ) {
        if axes == 0 {
            return;
        }
        for event_type in [
            LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_FINGER,
            LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
        ] {
            out.push_back(LibinputEvent {
                event_type,
                payload: EventPayload::PointerAxis(PointerAxisEvent::with_axes(
                    ts_usec, axes, values, [0; 2], [0.0; 2], 2,
                )),
                context: ctx,
                device: lib_dev,
            });
        }
    }

    unsafe fn emit_edge_scroll_frame(
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        frame: EdgeScrollFrame,
        natural_scroll: bool,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        for (axis, raw_delta) in frame.scrolls {
            let mut values = [0.0; 2];
            let value = raw_delta * 0.02 * 24.0;
            if axis == AXIS_VERTICAL {
                values[0] = if natural_scroll { -value } else { value };
            } else if axis == AXIS_HORIZONTAL {
                values[1] = if natural_scroll { -value } else { value };
            }
            Self::emit_finger_axis(ts_usec, lib_dev, ctx, axis, values, out);
        }
        Self::emit_finger_axis(ts_usec, lib_dev, ctx, frame.stop_axes, [0.0; 2], out);
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
        touch_arbitrated: bool,
    ) {
        if touch_arbitrated {
            td.touch_arbitration_suppressed = true;
            td.tap_emitted = true;
        } else if td.touch_arbitration_suppressed && td.active_slot_count() == 0 {
            // The delayed post-proximity arbitration window expired between
            // touch sequences. Clear the completed suppressed sequence
            // before the first raw event of a new contact, otherwise that
            // contact would inherit its predecessor's palm suppression.
            td.touch_arbitration_suppressed = false;
            Self::reset_touchpad_sequence_after_arbitration(td);
        }

        match ev.event_type() {
            EventType::KEY => {
                let code = ev.code();
                let value = ev.value();

                if value != 0
                    && [
                        KeyCode::BTN_LEFT.0,
                        KeyCode::BTN_RIGHT.0,
                        KeyCode::BTN_MIDDLE.0,
                    ]
                    .contains(&code)
                {
                    let button_areas = (*lib_dev).click_method == 1;
                    let edge_gesture_contacts_before =
                        td.gesture_tracked_contact_count(button_areas);
                    let edge_axes = td.stop_edge_scroll();
                    Self::emit_finger_axis(ts_usec, lib_dev, ctx, edge_axes, [0.0; 2], out);
                    let n_fingers = td.gesture_finger_count(button_areas);
                    if edge_gesture_contacts_before
                        != td.gesture_tracked_contact_count(button_areas)
                    {
                        td.rebase_gesture_contacts_after_eligibility_change(button_areas);
                        td.sync_gesture_finger_count(n_fingers);
                    }
                    td.gesture_last_centroid = td.gesture_centroid(button_areas);
                    td.pinch_base_centroid = td.gesture_last_centroid;
                    td.finger_scroll_constraint.reset();
                }

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
                    let new_touch_palm = (value != 0)
                        .then(|| td.dwt_palm_for_new_touch(ts_usec))
                        .flatten();
                    td.touch_active = value != 0;
                    // A single-touch direct touchscreen reports its contact
                    // through BTN_TOUCH plus ABS_X/ABS_Y, just like a
                    // single-touch touchpad. Keep a slot for both device
                    // classes so the direct-touch SYN_REPORT path can emit
                    // its touch lifecycle.
                    if !td.has_mt && (td.is_pointer || (*lib_dev).has_touch) {
                        let touch_arbitration_suppressed = td.touch_arbitration_suppressed;
                        let slot = &mut td.mt_slots[0];
                        td.mt_contact_count_changed |= slot.active != (value != 0);
                        slot.active = value != 0;
                        slot.began_this_frame = value != 0;
                        slot.dirty = true;
                        if value != 0 {
                            slot.tracking_id = 0;
                            slot.button_area_classification_pending = true;
                            slot.button_area_excluded = false;
                            slot.edge_classification_pending = true;
                            slot.edge_state = EdgeScrollState::None;
                            slot.edge_mask = 0;
                            slot.edge_axis = 0;
                            slot.edge_started_at = None;
                            // A single-touch slot is reused across contacts.
                            // Clear a prior DWT palm before applying the
                            // classification for this fresh contact.
                            slot.generic_palm = None;
                            slot.dwt_released_this_frame = false;
                            slot.dwt_palm = None;
                            slot.dwt_palm_started_usec = None;
                            slot.palm_suppressed = touch_arbitration_suppressed;
                            TrackedDevice::mark_dwt_palm(slot, new_touch_palm, ts_usec);
                        } else {
                            slot.button_area_classification_pending = false;
                            slot.edge_classification_pending = false;
                            slot.generic_palm = None;
                            slot.dwt_released_this_frame = false;
                            slot.dwt_palm = None;
                            slot.dwt_palm_started_usec = None;
                        }
                    }
                    if td.touch_active {
                        let continues_tap_drag =
                            td.tap_button_down.is_some() && td.tap_release_since.take().is_some();
                        td.tap_drag_active = continues_tap_drag;
                        td.touch_start_time = Some(Instant::now());
                        td.tap_emitted = td.touch_arbitration_suppressed || continues_tap_drag;
                        if new_touch_palm.is_some() {
                            td.tap_emitted = true;
                        }
                        if continues_tap_drag {
                            td.hold_started_at = None;
                            td.hold_blocked = true;
                        }
                        td.touch_fingers = td.active_slot_count().max(1) as u32;
                        td.tap_fingers = td.touch_fingers;
                        td.last_x = None;
                        td.last_y = None;
                    } else {
                        // BTN_TOUCH=0 terminates the complete contact set. End
                        // an active hold before tap-to-click emits its button
                        // pair so clients observe the gesture lifecycle in
                        // chronological order.
                        if td.hold_active {
                            out.push_back(LibinputEvent {
                                event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_END,
                                payload: EventPayload::GestureHoldEnd(GestureEvent {
                                    time_usec: ts_usec,
                                    finger_count: td.hold_fingers,
                                    dx: 0.0,
                                    dy: 0.0,
                                    scale: 1.0,
                                    angle: 0.0,
                                    cancelled: false,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                            td.hold_active = false;
                            td.hold_started_at = None;
                            td.hold_blocked = false;
                        }

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
                                        .gesture_primary_slot_distance((*lib_dev).click_method == 1)
                                        .map(|d| {
                                            if td.pinch_base_dist > 0.0 {
                                                d / td.pinch_base_dist
                                            } else {
                                                1.0
                                            }
                                        })
                                        .unwrap_or(1.0),
                                    angle: td
                                        .gesture_primary_slot_angle((*lib_dev).click_method == 1)
                                        - td.pinch_base_angle,
                                    cancelled: false,
                                }),
                                context: ctx,
                                device: lib_dev,
                            });
                        }

                        // A tap during the drag-lock window terminates the
                        // locked drag before the tap's own button sequence.
                        let resumed_drag_tap = td.drag_3fg_active
                            && cfg_tap
                            && !td.tap_emitted
                            && td.tap_fingers <= 3
                            && td
                                .touch_start_time
                                .is_some_and(|start| start.elapsed() < Duration::from_millis(250));
                        if td.drag_3fg_button_down && (!td.drag_3fg_active || resumed_drag_tap) {
                            td.drag_3fg_button_down = false;
                            td.drag_3fg_active = false;
                            td.drag_3fg_release_since = None;
                            let released_count = release_seat_button(lib_dev);
                            out.push_back(LibinputEvent {
                                event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
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

                        // Tap-to-drag keeps the tap button held across the
                        // second contact and releases it when that contact
                        // lifts. A standalone tap is released by its timer.
                        if td.tap_drag_active {
                            if let Some(button) = td.tap_button_down.take() {
                                let released_count = release_seat_button(lib_dev);
                                out.push_back(LibinputEvent {
                                    event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                    payload: EventPayload::PointerButton(PointerButtonEvent {
                                        time_usec: ts_usec,
                                        button: button as u32,
                                        state: 0,
                                        seat_button_count: released_count,
                                    }),
                                    context: ctx,
                                    device: lib_dev,
                                });
                            }
                            td.tap_release_since = None;
                            td.tap_drag_active = false;
                        } else if cfg_tap
                            && !td.tap_emitted
                            && !td.touch_arbitration_suppressed
                            && td
                                .touch_start_time
                                .is_some_and(|start| start.elapsed() < Duration::from_millis(250))
                        {
                            let button = match td.tap_fingers {
                                1 => Some(KeyCode::BTN_LEFT.0),
                                2 => Some(KeyCode::BTN_RIGHT.0),
                                3 => Some(KeyCode::BTN_MIDDLE.0),
                                _ => None,
                            };
                            if let Some(button) = button {
                                let pressed_count = press_seat_button(lib_dev);
                                out.push_back(LibinputEvent {
                                    event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                    payload: EventPayload::PointerButton(PointerButtonEvent {
                                        time_usec: ts_usec,
                                        button: button as u32,
                                        state: 1,
                                        seat_button_count: pressed_count,
                                    }),
                                    context: ctx,
                                    device: lib_dev,
                                });
                                td.tap_button_down = Some(button);
                                td.tap_release_since = Some(Instant::now());
                            }
                        }
                        td.last_x = None;
                        td.last_y = None;
                        td.current_dx = 0;
                        td.current_dy = 0;
                        td.touch_start_time = None;
                        td.touch_fingers = 0;
                        td.tap_fingers = 0;
                    }
                } else if code == KeyCode::BTN_TOOL_DOUBLETAP.0 {
                    td.touch_fingers = if value != 0 { 2 } else { 1 };
                    td.tap_fingers = td.tap_fingers.max(td.touch_fingers);
                    td.mt_contact_count_changed = true;
                } else if code == KeyCode::BTN_TOOL_TRIPLETAP.0 {
                    td.touch_fingers = if value != 0 { 3 } else { 2 };
                    td.tap_fingers = td.tap_fingers.max(td.touch_fingers);
                    td.mt_contact_count_changed = true;
                } else if code == KeyCode::BTN_TOOL_QUADTAP.0 {
                    td.touch_fingers = if value != 0 { 4 } else { 3 };
                    td.tap_fingers = td.tap_fingers.max(td.touch_fingers);
                    td.mt_contact_count_changed = true;
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
                            let button_areas = (*lib_dev).click_method == 1;
                            let clickfinger = (*lib_dev).click_method == 2;
                            let click_position = td.last_x.zip(td.last_y).or_else(|| {
                                td.mt_slots
                                    .iter()
                                    .find(|slot| slot.active && !slot.palm_suppressed)
                                    .map(|slot| (slot.x.round() as i32, slot.y.round() as i32))
                            });
                            let (in_right_button_area, in_top_button_area) =
                                match (click_position, td.abs_x_range, td.abs_y_range) {
                                    (Some((x, y)), Some((xmin, xmax)), Some((ymin, ymax))) => {
                                        let right_edge = xmin + (xmax - xmin) * 2 / 3;
                                        let button_top = ymin + (ymax - ymin) * 4 / 5;
                                        let top_button_bottom = ymin + (ymax - ymin) / 5;
                                        (
                                            button_areas && x >= right_edge && y >= button_top,
                                            td.is_topbuttonpad && y <= top_button_bottom,
                                        )
                                    }
                                    _ => (false, false),
                                };
                            if in_top_button_area {
                                if let (Some((x, _)), Some((xmin, xmax))) =
                                    (click_position, td.abs_x_range)
                                {
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
                            } else if in_right_button_area {
                                mapped = KeyCode::BTN_RIGHT.0;
                            }
                            if clickfinger {
                                let clickfinger_fingers = td.clickfinger_finger_count(
                                    (*lib_dev).abs_x_resolution,
                                    (*lib_dev).abs_y_resolution,
                                    (*lib_dev).abs_x_range.or(td.abs_x_range),
                                    (*lib_dev).abs_y_range.or(td.abs_y_range),
                                    (*lib_dev).width_mm,
                                    (*lib_dev).height_mm,
                                );
                                mapped = match clickfinger_fingers {
                                    2 if (*lib_dev).clickfinger_button_map == 1 => {
                                        KeyCode::BTN_MIDDLE.0
                                    }
                                    2 => KeyCode::BTN_RIGHT.0,
                                    n if n >= 3 && (*lib_dev).clickfinger_button_map == 1 => {
                                        KeyCode::BTN_RIGHT.0
                                    }
                                    n if n >= 3 => KeyCode::BTN_MIDDLE.0,
                                    _ => KeyCode::BTN_LEFT.0,
                                };
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
                        let new_touch_palm = td.dwt_palm_for_new_touch(ts_usec);
                        let mt_slot = &mut td.mt_slots[slot];
                        let new_contact =
                            val >= 0 && (!mt_slot.active || mt_slot.tracking_id != val);
                        let typing_palm = new_contact && new_touch_palm.is_some();
                        // libevdev absorbs SYN_DROPPED while reconstructing
                        // the current device state. A replacement tracking ID
                        // can consequently arrive on an already-active slot
                        // when a release was lost. Its synthetic SYN_REPORT
                        // can precede the refreshed coordinates, so defer the
                        // gesture rebase until that contact has a position.
                        td.mt_contact_count_changed |= mt_slot.active != (val >= 0) || new_contact;
                        mt_slot.active = val >= 0;
                        mt_slot.began_this_frame = new_contact;
                        mt_slot.tracking_id = val;
                        if new_contact {
                            mt_slot.button_area_classification_pending = true;
                            mt_slot.button_area_excluded = false;
                            mt_slot.button_area_initial_x = 0.0;
                            mt_slot.button_area_initial_y = 0.0;
                            mt_slot.button_area_down_usec = 0;
                            mt_slot.gesture_initial_x = 0.0;
                            mt_slot.gesture_initial_y = 0.0;
                            mt_slot.gesture_rebase_pending = true;
                            mt_slot.gesture_position_seen = false;
                            mt_slot.edge_classification_pending = true;
                            mt_slot.edge_state = EdgeScrollState::None;
                            mt_slot.edge_mask = 0;
                            mt_slot.edge_initial_x = 0.0;
                            mt_slot.edge_initial_y = 0.0;
                            mt_slot.edge_last_x = 0.0;
                            mt_slot.edge_last_y = 0.0;
                            mt_slot.edge_started_at = None;
                            mt_slot.edge_axis = 0;
                            mt_slot.generic_palm = None;
                            mt_slot.dwt_released_this_frame = false;
                            mt_slot.pressure = 0.0;
                            mt_slot.touch_major = 0.0;
                            mt_slot.touch_minor = 0.0;
                            mt_slot.palm_suppressed =
                                mt_slot.tool_type == 2 || td.touch_arbitration_suppressed;
                            TrackedDevice::mark_dwt_palm(mt_slot, new_touch_palm, ts_usec);
                            mt_slot.cancel_pending = false;
                        } else {
                            mt_slot.button_area_classification_pending = false;
                            mt_slot.button_area_excluded = false;
                            mt_slot.button_area_initial_x = 0.0;
                            mt_slot.button_area_initial_y = 0.0;
                            mt_slot.button_area_down_usec = 0;
                            mt_slot.gesture_initial_x = 0.0;
                            mt_slot.gesture_initial_y = 0.0;
                            mt_slot.gesture_rebase_pending = false;
                            mt_slot.gesture_position_seen = false;
                            mt_slot.edge_classification_pending = false;
                            if val < 0 {
                                mt_slot.began_this_frame = false;
                                mt_slot.generic_palm = None;
                                mt_slot.dwt_released_this_frame = false;
                                mt_slot.dwt_palm = None;
                                mt_slot.dwt_palm_started_usec = None;
                            }
                        }
                        mt_slot.dirty = true;
                        if typing_palm {
                            td.tap_emitted = true;
                        }
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
                        mt_slot.gesture_position_seen |= mt_slot.gesture_rebase_pending;
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
                        mt_slot.gesture_position_seen |= mt_slot.gesture_rebase_pending;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_DISTANCE.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        td.mt_slots[slot].distance = val as f64;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_PRESSURE.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        mt_slot.pressure = val as f64;
                        mt_slot.dirty = true;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_TOUCH_MAJOR.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        mt_slot.touch_major = val as f64;
                        mt_slot.dirty = true;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_TOUCH_MINOR.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        let mt_slot = &mut td.mt_slots[slot];
                        mt_slot.touch_minor = val as f64;
                        mt_slot.dirty = true;
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
                } else if code == AbsoluteAxisCode::ABS_PRESSURE.0 {
                    if !td.has_mt {
                        let mt_slot = &mut td.mt_slots[0];
                        mt_slot.pressure = val as f64;
                        mt_slot.dirty = true;
                    }
                }
                // ---- Single-touch ABS_X/Y ----
                else if code == AbsoluteAxisCode::ABS_X.0 {
                    if !td.has_mt && (td.is_pointer || (*lib_dev).has_touch) {
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
                    if !td.has_mt && (td.is_pointer || (*lib_dev).has_touch) {
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

                // A typing/trackpoint palm is released only on a subsequent
                // contact update after its timer has expired. This keeps a
                // resting palm ignored but lets a freshly-started contact
                // resume pointer motion, matching upstream's nuanced rule.
                td.release_expired_dwt_palms();
                td.refresh_generic_palms();

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
                            slot.palm_suppressed |= td.touch_arbitration_suppressed;
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
                    let tablet_is_active = active_tablet_for_touch(ctx, lib_dev).is_some();
                    let tablet_just_activated =
                        tablet_is_active && !td.touch_arbitration_tablet_was_active;
                    td.touch_arbitration_tablet_was_active = tablet_is_active;
                    if td.mt_slots.iter().any(|slot| {
                        slot.active
                            && (!slot.reported || tablet_just_activated)
                            && touch_arbitration_active(ctx, lib_dev, Some((slot.x, slot.y)))
                    }) {
                        td.touch_arbitration_suppressed = true;
                    }
                    if td.touch_arbitration_suppressed {
                        for slot in &mut td.mt_slots {
                            if slot.active {
                                slot.palm_suppressed = true;
                            }
                            if slot.reported {
                                if let Some(seat_slot) = slot.seat_slot.take() {
                                    release_touch_seat_slot(ctx, seat_slot);
                                }
                                slot.reported = false;
                            }
                            if !slot.active {
                                slot.palm_suppressed = false;
                            }
                            slot.dirty = false;
                        }
                        td.touch_active = td.mt_slots.iter().any(|slot| slot.active);
                        if !td.touch_active && !touch_arbitrated {
                            td.touch_arbitration_suppressed = false;
                            Self::reset_touchpad_sequence_after_arbitration(td);
                        }
                        return;
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

                if (*lib_dev).has_gesture {
                    let sequence_active = td.active_slot_count() != 0;
                    if sequence_active && !td.touchpad_left_handed_sequence_active {
                        td.touchpad_left_handed_applied =
                            paired_left_handed_rotation_enabled(ctx, lib_dev);
                    }
                    td.touchpad_left_handed_sequence_active = sequence_active;
                }

                if td.touch_arbitration_suppressed {
                    for slot in &mut td.mt_slots {
                        slot.palm_suppressed = slot.active;
                        slot.reported = false;
                        slot.seat_slot = None;
                        slot.dirty = false;
                    }
                    td.touch_active = td.mt_slots.iter().any(|slot| slot.active);
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.finger_scroll_constraint.reset();
                    if !td.touch_active && !touch_arbitrated {
                        td.touch_arbitration_suppressed = false;
                        Self::reset_touchpad_sequence_after_arbitration(td);
                    }
                    return;
                }

                if td.mt_contact_count_changed && !td.gesture_contact_rebase_ready() {
                    // A synchronized evdev state may report a new tracking
                    // ID in a frame before the corresponding position. Do
                    // not derive motion or gesture direction from that mixed
                    // old/new contact state.
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.finger_scroll_constraint.reset();
                    return;
                }

                // While an active DWT/DWTP interval is in effect, libinput
                // still processes physical buttons but stops touchpad
                // scrolling and gesture/pointer motion for every contact.
                // Individual contacts retain their palm classification so
                // their post-timeout lifecycle can be resolved separately.
                if td.dwt.active || td.dwtp.active {
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.finger_scroll_constraint.reset();
                    return;
                }

                let button_areas = (*lib_dev).click_method == 1;
                let x_range = (*lib_dev).abs_x_range.or(td.abs_x_range);
                let y_range = (*lib_dev).abs_y_range.or(td.abs_y_range);
                let edge_enabled = (*lib_dev).scroll_method == 2;
                td.classify_edge_contacts(
                    edge_enabled,
                    button_areas,
                    (*lib_dev).abs_x_resolution,
                    (*lib_dev).abs_y_resolution,
                    x_range,
                    y_range,
                    (*lib_dev).width_mm,
                    (*lib_dev).height_mm,
                );

                if std::mem::take(&mut td.mt_contact_count_changed) {
                    td.complete_gesture_contact_rebase();
                    let button_top = TrackedDevice::bottom_button_area_top(
                        y_range,
                        (*lib_dev).abs_y_resolution,
                        (*lib_dev).height_mm,
                    );
                    td.classify_gesture_contacts(button_areas, button_top, ts_usec);
                    td.reset_gesture_contact_initials(button_areas);
                    let edge_gesture_contacts_before =
                        td.gesture_tracked_contact_count(button_areas);
                    let edge_frame = td.process_edge_scroll_frame(
                        edge_enabled,
                        (*lib_dev).abs_x_resolution,
                        (*lib_dev).abs_y_resolution,
                        x_range,
                        y_range,
                        (*lib_dev).width_mm,
                        (*lib_dev).height_mm,
                    );
                    if edge_gesture_contacts_before
                        != td.gesture_tracked_contact_count(button_areas)
                    {
                        td.rebase_gesture_contacts_after_eligibility_change(button_areas);
                    }
                    Self::emit_edge_scroll_frame(ts_usec, lib_dev, ctx, edge_frame, cfg_nat, out);
                    let n_fingers = td.gesture_finger_count(button_areas);
                    let eligible_tracked_contacts = td.gesture_tracked_contact_count(button_areas);
                    td.finger_scroll_constraint.reset();
                    if td.touch_active {
                        td.tap_fingers = td.tap_fingers.max(n_fingers as u32);
                    }
                    let drag_fingers = match (*lib_dev).drag_3fg_enabled {
                        1 => 3,
                        2 => 4,
                        _ => 0,
                    };
                    if drag_fingers != 0 && n_fingers == drag_fingers {
                        if td.drag_3fg_button_down && td.drag_3fg_release_since.take().is_some() {
                            td.drag_3fg_active = true;
                            td.drag_3fg_candidate_since = None;
                            td.drag_3fg_candidate_time_usec = 0;
                            td.hold_started_at = None;
                            td.hold_blocked = true;
                        } else if !td.drag_3fg_button_down {
                            if td.drag_3fg_candidate_since.is_none() {
                                td.drag_3fg_candidate_since = Some(Instant::now());
                                td.drag_3fg_candidate_time_usec = ts_usec;
                            }
                            if !(*lib_dev).tap_enabled && !td.swipe_active {
                                td.swipe_active = true;
                                td.swipe_fingers = n_fingers as i32;
                                out.push_back(LibinputEvent {
                                    event_type:
                                        LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_BEGIN,
                                    payload: EventPayload::GestureSwipeBegin(GestureEvent {
                                        time_usec: ts_usec,
                                        finger_count: td.swipe_fingers,
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
                    } else if n_fingers == 0 {
                        td.drag_3fg_candidate_since = None;
                        td.drag_3fg_candidate_time_usec = 0;
                        if td.drag_3fg_active && td.drag_3fg_button_down {
                            td.drag_3fg_active = false;
                            td.drag_3fg_release_since = Some(Instant::now());
                        }
                    } else if !td.drag_3fg_button_down {
                        td.drag_3fg_candidate_since = None;
                        td.drag_3fg_candidate_time_usec = 0;
                    }
                    if td.drag_3fg_button_down {
                        td.hold_started_at = None;
                        td.hold_blocked = true;
                    }
                    td.sync_gesture_finger_count(n_fingers);
                    if ((*lib_dev).scroll_method != 1
                        || n_fingers != 2
                        || eligible_tracked_contacts == 0)
                        && td.finger_scroll_axes != 0
                    {
                        for event_type in [
                            LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_FINGER,
                            LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
                        ] {
                            out.push_back(LibinputEvent {
                                event_type,
                                payload: EventPayload::PointerAxis(PointerAxisEvent::with_axes(
                                    ts_usec,
                                    td.finger_scroll_axes,
                                    [0.0; 2],
                                    [0; 2],
                                    [0.0; 2],
                                    2,
                                )),
                                context: ctx,
                                device: lib_dev,
                            });
                        }
                        td.finger_scroll_axes = 0;
                    }
                    if td.swipe_active && n_fingers < 3 {
                        td.swipe_active = false;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_END,
                            payload: EventPayload::GestureSwipeEnd(GestureEvent {
                                time_usec: ts_usec,
                                finger_count: td.swipe_fingers,
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
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.gesture_last_centroid = td.gesture_centroid(button_areas);
                    td.pinch_base_centroid = td.gesture_last_centroid;
                    if !td.pinch_active && n_fingers >= 2 {
                        if let Some(distance) = td.gesture_primary_slot_distance(button_areas) {
                            td.pinch_base_dist = distance;
                            td.pinch_base_angle = td.gesture_primary_slot_angle(button_areas);
                            td.pinch_fingers = n_fingers as i32;
                        }
                    }
                    return;
                }

                let button_top = TrackedDevice::bottom_button_area_top(
                    y_range,
                    (*lib_dev).abs_y_resolution,
                    (*lib_dev).height_mm,
                );
                if td.update_button_area_contacts(
                    button_areas,
                    button_top,
                    (*lib_dev).abs_x_resolution,
                    (*lib_dev).abs_y_resolution,
                    x_range,
                    y_range,
                    (*lib_dev).width_mm,
                    (*lib_dev).height_mm,
                ) {
                    // The set of gesture-eligible contacts changed even
                    // though the kernel's MT contact count did not. Rebase
                    // before calculating a centroid delta for this frame.
                    let n_fingers = td.gesture_finger_count(button_areas);
                    td.rebase_gesture_contacts_after_eligibility_change(button_areas);
                    td.sync_gesture_finger_count(n_fingers);
                }

                if (*lib_dev).scroll_method != 1 && td.finger_scroll_axes != 0 {
                    Self::emit_finger_axis(
                        ts_usec,
                        lib_dev,
                        ctx,
                        td.finger_scroll_axes,
                        [0.0; 2],
                        out,
                    );
                    td.finger_scroll_axes = 0;
                    td.finger_scroll_constraint.reset();
                }

                let edge_gesture_contacts_before = td.gesture_tracked_contact_count(button_areas);
                let edge_frame = td.process_edge_scroll_frame(
                    edge_enabled,
                    (*lib_dev).abs_x_resolution,
                    (*lib_dev).abs_y_resolution,
                    x_range,
                    y_range,
                    (*lib_dev).width_mm,
                    (*lib_dev).height_mm,
                );
                let edge_gesture_contacts_changed =
                    edge_gesture_contacts_before != td.gesture_tracked_contact_count(button_areas);
                if edge_gesture_contacts_changed {
                    let n_fingers = td.gesture_finger_count(button_areas);
                    td.rebase_gesture_contacts_after_eligibility_change(button_areas);
                    td.sync_gesture_finger_count(n_fingers);
                }
                let edge_suppresses_motion = edge_frame.suppress_normal_motion;
                Self::emit_edge_scroll_frame(ts_usec, lib_dev, ctx, edge_frame, cfg_nat, out);
                if edge_suppresses_motion {
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.finger_scroll_constraint.reset();
                    td.gesture_last_centroid = td.gesture_centroid(button_areas);
                    td.pinch_base_centroid = td.gesture_last_centroid;
                    td.tap_emitted = true;
                    return;
                }

                // ---- Pinch UPDATE on SYN_REPORT ----
                if td.pinch_active {
                    td.finger_scroll_constraint.reset();
                    if let Some(dist) = td.gesture_primary_slot_distance(button_areas) {
                        let scale = if td.pinch_base_dist > 0.0 {
                            dist / td.pinch_base_dist
                        } else {
                            1.0
                        };
                        let angle =
                            td.gesture_primary_slot_angle(button_areas) - td.pinch_base_angle;
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

                if td.has_mt {
                    let centroid = td.gesture_centroid(button_areas);
                    if let (Some((x, y)), Some((last_x, last_y))) =
                        (centroid, td.gesture_last_centroid)
                    {
                        td.current_dx = (x - last_x).round() as i32;
                        td.current_dy = (y - last_y).round() as i32;
                    } else {
                        td.current_dx = 0;
                        td.current_dy = 0;
                    }
                    td.gesture_last_centroid = centroid;
                }
                if td.touchpad_left_handed_applied {
                    td.current_dx = td.current_dx.saturating_neg();
                    td.current_dy = td.current_dy.saturating_neg();
                }
                let has_movement = td.current_dx != 0 || td.current_dy != 0;
                let n_fingers = td.gesture_finger_count(button_areas);

                // A single-touch device has no MT slot path to exclude a
                // typing/trackpoint palm from its accumulated absolute
                // motion.  If every active contact is suppressed, discard
                // that motion before the generic one-finger pointer path.
                if td.active_slot_count() > 0 && n_fingers == 0 {
                    td.current_dx = 0;
                    td.current_dy = 0;
                    td.remainder_x = 0.0;
                    td.remainder_y = 0.0;
                    td.finger_scroll_constraint.reset();
                    return;
                }
                let drag_fingers = match (*lib_dev).drag_3fg_enabled {
                    1 => 3,
                    2 => 4,
                    _ => 0,
                };

                if td.drag_3fg_button_down
                    && !td.drag_3fg_active
                    && td.drag_3fg_release_since.is_some()
                    && n_fingers != drag_fingers
                    && has_movement
                {
                    td.drag_3fg_release_since = None;
                    td.drag_3fg_button_down = false;
                    let seat_button_count = release_seat_button(lib_dev);
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                        payload: EventPayload::PointerButton(PointerButtonEvent {
                            time_usec: ts_usec,
                            button: KeyCode::BTN_LEFT.0 as u32,
                            state: 0,
                            seat_button_count,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                }

                let activates_3fg_drag = drag_fingers != 0
                    && n_fingers == drag_fingers
                    && !td.drag_3fg_button_down
                    && td.drag_3fg_candidate_since.is_some()
                    && (td
                        .drag_3fg_candidate_since
                        .is_some_and(|since| since.elapsed() >= Duration::from_millis(80))
                        || ts_usec.saturating_sub(td.drag_3fg_candidate_time_usec) >= 80_000);
                if activates_3fg_drag {
                    if !td.swipe_active {
                        td.swipe_fingers = n_fingers as i32;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_BEGIN,
                            payload: EventPayload::GestureSwipeBegin(GestureEvent {
                                time_usec: ts_usec,
                                finger_count: td.swipe_fingers,
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
                    td.swipe_active = false;
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_END,
                        payload: EventPayload::GestureSwipeEnd(GestureEvent {
                            time_usec: ts_usec,
                            finger_count: td.swipe_fingers,
                            dx: 0.0,
                            dy: 0.0,
                            scale: 1.0,
                            angle: 0.0,
                            cancelled: true,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                    td.drag_3fg_candidate_since = None;
                    td.drag_3fg_candidate_time_usec = 0;
                    td.drag_3fg_active = true;
                    td.drag_3fg_button_down = true;
                    td.hold_started_at = None;
                    td.hold_blocked = true;
                    td.tap_emitted = true;
                    let seat_button_count = press_seat_button(lib_dev);
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                        payload: EventPayload::PointerButton(PointerButtonEvent {
                            time_usec: ts_usec,
                            button: KeyCode::BTN_LEFT.0 as u32,
                            state: 1,
                            seat_button_count,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                }

                if td.drag_3fg_active {
                    td.finger_scroll_constraint.reset();
                    td.tap_emitted = true;
                    if has_movement {
                        let scale = 0.18;
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION,
                            payload: EventPayload::PointerMotion(PointerMotionEvent {
                                time_usec: ts_usec,
                                dx: td.current_dx as f64 * scale,
                                dy: td.current_dy as f64 * scale,
                                dx_unaccel: td.current_dx as f64 * scale,
                                dy_unaccel: td.current_dy as f64 * scale,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                    td.current_dx = 0;
                    td.current_dy = 0;
                    return;
                }

                let button_areas = (*lib_dev).click_method == 1;
                let eligible_tracked_contacts = td.gesture_tracked_contact_count(button_areas);
                if n_fingers != 2 || eligible_tracked_contacts == 0 {
                    td.finger_scroll_constraint.reset();
                }
                let pinch_is_trackable = eligible_tracked_contacts >= 2
                    && td.touch_fingers as usize <= td.active_slot_count();
                let contact_direction = (n_fingers == 2)
                    .then(|| {
                        td.gesture_contacts_same_direction(
                            button_areas,
                            (*lib_dev).abs_x_resolution,
                            (*lib_dev).abs_y_resolution,
                            x_range,
                            y_range,
                            (*lib_dev).width_mm,
                            (*lib_dev).height_mm,
                        )
                    })
                    .flatten();
                let pinch_direction_pending = n_fingers == 2
                    && pinch_is_trackable
                    && (*lib_dev).has_gesture
                    && contact_direction.is_none();
                let centroid_distance = match (td.gesture_last_centroid, td.pinch_base_centroid) {
                    (Some((x, y)), Some((base_x, base_y))) => (x - base_x).hypot(y - base_y),
                    _ => f64::from(td.current_dx).hypot(f64::from(td.current_dy)),
                };
                let pinch_distance = td.gesture_primary_slot_distance(button_areas);
                let starts_pinch = !td.pinch_active
                    && !td.swipe_active
                    && td.drag_3fg_candidate_since.is_none()
                    && pinch_is_trackable
                    && n_fingers >= 2
                    && (if n_fingers == 2 {
                        contact_direction == Some(false)
                    } else {
                        td.pinch_base_dist > 0.0
                            && pinch_distance.is_some_and(|distance| {
                                let distance_delta = (distance - td.pinch_base_dist).abs();
                                distance_delta > 0.5
                                    && (centroid_distance == 0.0
                                        || distance_delta > centroid_distance * 2.0)
                            })
                    });
                if !has_movement && !starts_pinch {
                    return;
                }

                let motion_ends_hold = n_fingers >= 2 || starts_pinch;
                if td.hold_active && motion_ends_hold {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_END,
                        payload: EventPayload::GestureHoldEnd(GestureEvent {
                            time_usec: ts_usec,
                            finger_count: td.hold_fingers,
                            dx: 0.0,
                            dy: 0.0,
                            scale: 1.0,
                            angle: 0.0,
                            cancelled: true,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                    td.hold_active = false;
                    td.hold_blocked = true;
                    td.hold_started_at = None;
                } else if !td.hold_active && motion_ends_hold && td.hold_started_at.is_some() {
                    td.hold_started_at = None;
                    td.hold_blocked = true;
                }

                if starts_pinch {
                    td.finger_scroll_constraint.reset();
                    td.pinch_active = true;
                    td.pinch_fingers = n_fingers as i32;
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_BEGIN,
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
                    td.current_dx = 0;
                    td.current_dy = 0;
                    return;
                }

                td.tap_emitted = true;
                let hw_scale: f32 = 0.18;

                if n_fingers <= 1 {
                    let inactive_button_area_contact = button_areas
                        && td
                            .mt_slots
                            .iter()
                            .any(|slot| slot.active && slot.button_area_excluded);
                    if !inactive_button_area_contact {
                        let total_x =
                            (td.current_dx as f32 * hw_scale) * cfg_accel + td.remainder_x;
                        let total_y =
                            (td.current_dy as f32 * hw_scale) * cfg_accel + td.remainder_y;
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
                    }
                } else if n_fingers == 2 && (*lib_dev).scroll_method == 1 {
                    // While both contacts are still below the pinch
                    // disambiguation threshold, hold back finger scrolling.
                    // This avoids leaking a scroll event before a valid
                    // pinch begins on coarse-resolution touchpads.
                    if pinch_direction_pending {
                        td.finger_scroll_constraint.reset();
                        return;
                    }
                    let scroll_scale = 0.02;
                    let raw_scroll_x = f64::from(td.current_dx) * scroll_scale;
                    let raw_scroll_y = f64::from(td.current_dy) * scroll_scale;
                    let delta_x_mm = touchpad_delta_mm(
                        f64::from(td.current_dx),
                        (*lib_dev).abs_x_resolution,
                        (*lib_dev).abs_x_range,
                        (*lib_dev).width_mm,
                    );
                    let delta_y_mm = touchpad_delta_mm(
                        f64::from(td.current_dy),
                        (*lib_dev).abs_y_resolution,
                        (*lib_dev).abs_y_range,
                        (*lib_dev).height_mm,
                    );
                    let gesture_distance_mm =
                        match (td.gesture_last_centroid, td.pinch_base_centroid) {
                            (Some((x, y)), Some((base_x, base_y))) => touchpad_delta_mm(
                                x - base_x,
                                (*lib_dev).abs_x_resolution,
                                (*lib_dev).abs_x_range,
                                (*lib_dev).width_mm,
                            )
                            .hypot(touchpad_delta_mm(
                                y - base_y,
                                (*lib_dev).abs_y_resolution,
                                (*lib_dev).abs_y_range,
                                (*lib_dev).height_mm,
                            )),
                            _ => delta_x_mm.hypot(delta_y_mm),
                        };
                    let gesture_moved_at_least_one_mm = td
                        .gesture_contact_moved_at_least(
                            button_areas,
                            (*lib_dev).abs_x_resolution,
                            (*lib_dev).abs_y_resolution,
                            x_range,
                            y_range,
                            (*lib_dev).width_mm,
                            (*lib_dev).height_mm,
                            1.0,
                        )
                        .unwrap_or(gesture_distance_mm >= 1.0);
                    if td.finger_scroll_axes == 0 && !gesture_moved_at_least_one_mm {
                        td.remainder_x = 0.0;
                        td.remainder_y = 0.0;
                        return;
                    }
                    let (scroll_x, scroll_y) = td.finger_scroll_constraint.constrain(
                        ts_usec,
                        raw_scroll_x,
                        raw_scroll_y,
                        delta_x_mm,
                        delta_y_mm,
                    );
                    td.remainder_y = 0.0;
                    td.remainder_x = 0.0;
                    let mut axes = 0;
                    let mut values = [0.0; 2];
                    if scroll_y != 0.0 {
                        axes |= 1;
                        values[0] = if cfg_nat { -scroll_y } else { scroll_y };
                    }
                    if scroll_x != 0.0 {
                        axes |= 2;
                        values[1] = if cfg_nat { -scroll_x } else { scroll_x };
                    }
                    if axes != 0 {
                        values[0] *= 24.0;
                        values[1] *= 24.0;
                        td.finger_scroll_axes |= axes;
                        for event_type in [
                            LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_FINGER,
                            LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS,
                        ] {
                            out.push_back(LibinputEvent {
                                event_type,
                                payload: EventPayload::PointerAxis(PointerAxisEvent::with_axes(
                                    ts_usec, axes, values, [0; 2], [0.0; 2], 2,
                                )),
                                context: ctx,
                                device: lib_dev,
                            });
                        }
                    }
                } else if n_fingers >= 3 {
                    // 3+ fingers = swipe gesture
                    let gscale: f64 = 0.18;
                    let (event_type, event_payload) = if td.swipe_active {
                        (
                            LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_UPDATE,
                            EventPayload::GestureSwipeUpdate(GestureEvent {
                                time_usec: ts_usec,
                                finger_count: td.swipe_fingers,
                                dx: td.current_dx as f64 * gscale,
                                dy: td.current_dy as f64 * gscale,
                                scale: 1.0,
                                angle: 0.0,
                                cancelled: false,
                            }),
                        )
                    } else {
                        td.swipe_active = true;
                        td.swipe_fingers = n_fingers as i32;
                        (
                            LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_BEGIN,
                            EventPayload::GestureSwipeBegin(GestureEvent {
                                time_usec: ts_usec,
                                finger_count: td.swipe_fingers,
                                dx: 0.0,
                                dy: 0.0,
                                scale: 1.0,
                                angle: 0.0,
                                cancelled: false,
                            }),
                        )
                    };
                    out.push_back(LibinputEvent {
                        event_type,
                        payload: event_payload,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn active_slot(edge_state: EdgeScrollState) -> MtSlot {
        MtSlot {
            active: true,
            edge_state,
            ..MtSlot::default()
        }
    }

    #[test]
    fn edge_scroll_contacts_do_not_join_gestures() {
        assert!(TrackedDevice::gesture_slot_eligible(
            &active_slot(EdgeScrollState::Area),
            false
        ));
        assert!(!TrackedDevice::gesture_slot_eligible(
            &active_slot(EdgeScrollState::None),
            false
        ));
        assert!(!TrackedDevice::gesture_slot_eligible(
            &active_slot(EdgeScrollState::New),
            false
        ));
        assert!(!TrackedDevice::gesture_slot_eligible(
            &active_slot(EdgeScrollState::Active),
            false
        ));
    }
}
