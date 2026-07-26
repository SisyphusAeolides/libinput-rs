//! Opaque C-compatible types exposed through the libinput ABI.

use std::collections::VecDeque;
use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::backend::BackendState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Udev,
    Path,
}

// ---------------------------------------------------------------------------
// libinput_interface
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct LibinputInterface {
    pub open_restricted: Option<
        unsafe extern "C" fn(
            path: *const libc::c_char,
            flags: libc::c_int,
            user_data: *mut libc::c_void,
        ) -> libc::c_int,
    >,
    pub close_restricted:
        Option<unsafe extern "C" fn(fd: libc::c_int, user_data: *mut libc::c_void)>,
}

// ---------------------------------------------------------------------------
// libinput_event_type
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types, dead_code)]
pub enum LibinputEventType {
    LIBINPUT_EVENT_NONE = 0,
    LIBINPUT_EVENT_DEVICE_ADDED = 1,
    LIBINPUT_EVENT_DEVICE_REMOVED = 2,
    LIBINPUT_EVENT_KEYBOARD_KEY = 300,
    LIBINPUT_EVENT_POINTER_MOTION = 400,
    LIBINPUT_EVENT_POINTER_MOTION_ABSOLUTE = 401,
    LIBINPUT_EVENT_POINTER_BUTTON = 402,
    LIBINPUT_EVENT_POINTER_AXIS = 403,
    LIBINPUT_EVENT_POINTER_SCROLL_WHEEL = 404,
    LIBINPUT_EVENT_POINTER_SCROLL_FINGER = 405,
    LIBINPUT_EVENT_POINTER_SCROLL_CONTINUOUS = 406,
    LIBINPUT_EVENT_TOUCH_DOWN = 500,
    LIBINPUT_EVENT_TOUCH_UP = 501,
    LIBINPUT_EVENT_TOUCH_MOTION = 502,
    LIBINPUT_EVENT_TOUCH_CANCEL = 503,
    LIBINPUT_EVENT_TOUCH_FRAME = 504,
    LIBINPUT_EVENT_GESTURE_SWIPE_BEGIN = 800,
    LIBINPUT_EVENT_GESTURE_SWIPE_UPDATE = 801,
    LIBINPUT_EVENT_GESTURE_SWIPE_END = 802,
    LIBINPUT_EVENT_GESTURE_PINCH_BEGIN = 803,
    LIBINPUT_EVENT_GESTURE_PINCH_UPDATE = 804,
    LIBINPUT_EVENT_GESTURE_PINCH_END = 805,
    LIBINPUT_EVENT_SWITCH_TOGGLE = 900,
    LIBINPUT_EVENT_TABLET_TOOL_AXIS = 600,
    LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY = 601,
    LIBINPUT_EVENT_TABLET_TOOL_TIP = 602,
    LIBINPUT_EVENT_TABLET_TOOL_BUTTON = 603,
    LIBINPUT_EVENT_TABLET_PAD_BUTTON = 700,
    LIBINPUT_EVENT_TABLET_PAD_RING = 701,
    LIBINPUT_EVENT_TABLET_PAD_STRIP = 702,
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PointerMotionEvent {
    pub time_usec: u64,
    pub dx: f64,
    pub dy: f64,
    pub dx_unaccel: f64,
    pub dy_unaccel: f64,
}

#[derive(Debug, Clone)]
pub struct PointerMotionAbsoluteEvent {
    pub time_usec: u64,
    pub abs_x: f64,
    pub abs_y: f64,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

#[derive(Debug, Clone)]
pub struct PointerButtonEvent {
    pub time_usec: u64,
    pub button: u32,
    pub state: u32,
    pub seat_button_count: u32,
}

#[derive(Debug, Clone)]
pub struct PointerAxisEvent {
    pub time_usec: u64,
    pub axis: u32,
    pub value: f64,
    pub value_discrete: i32,
    pub value_v120: f64,
    pub source: u32,
}

#[derive(Debug, Clone)]
pub struct KeyboardKeyEvent {
    pub time_usec: u64,
    pub key: u32,
    pub state: u32,
}

#[derive(Debug, Clone)]
pub struct TouchEvent {
    pub time_usec: u64,
    pub slot: i32,
    pub seat_slot: i32,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone)]
pub struct GestureEvent {
    pub time_usec: u64,
    pub finger_count: i32,
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
    pub angle: f64,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct SwitchEvent {
    pub time_usec: u64,
    pub switch: u32,
    pub state: u32,
}

pub struct LibinputTabletTool {
    pub refcount: AtomicI32,
    pub user_data: *mut libc::c_void,
    pub serial: u64,
    pub tool_id: u64,
    pub name: Option<CString>,
    pub tool_type: u32,
    pub device: *mut LibinputDevice,
    pub has_pressure: bool,
    pub has_distance: bool,
    pub has_tilt: bool,
    pub has_rotation: bool,
    pub has_slider: bool,
    pub has_wheel: bool,
    pub has_size: bool,
    pub pressure_range_minimum: f64,
    pub pressure_range_maximum: f64,
    pub wanted_pressure_range_minimum: f64,
    pub wanted_pressure_range_maximum: f64,
    pub eraser_button_modes: u32,
    pub eraser_button_mode: u32,
    pub wanted_eraser_button_mode: u32,
    pub eraser_button: u32,
    pub wanted_eraser_button: u32,
    pub default_eraser_button: u32,
    pub in_proximity: bool,
    pub buttons: Vec<u32>,
}

unsafe impl Send for LibinputTabletTool {}

#[derive(Debug, Clone)]
pub struct TabletToolEvent {
    pub time_usec: u64,
    pub tool: *mut LibinputTabletTool,
    pub proximity_state: u32,
    pub x: f64,
    pub y: f64,
    pub dx: f64,
    pub dy: f64,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub x_resolution: f64,
    pub y_resolution: f64,
    pub x_changed: bool,
    pub y_changed: bool,
    pub pressure: f64,
    pub pressure_min: f64,
    pub pressure_max: f64,
    pub pressure_changed: bool,
    pub distance: f64,
    pub distance_changed: bool,
    pub tilt_x: f64,
    pub tilt_y: f64,
    pub tilt_x_changed: bool,
    pub tilt_y_changed: bool,
    pub rotation: f64,
    pub rotation_changed: bool,
    pub slider: f64,
    pub slider_changed: bool,
    pub wheel_delta: f64,
    pub wheel_discrete: i32,
    pub wheel_changed: bool,
    pub tip_state: u32,
    pub button: u32,
    pub button_state: u32,
    pub seat_button_count: u32,
}

#[derive(Debug, Clone)]
pub enum EventPayload {
    PointerMotion(PointerMotionEvent),
    PointerMotionAbsolute(PointerMotionAbsoluteEvent),
    PointerButton(PointerButtonEvent),
    PointerAxis(PointerAxisEvent),
    KeyboardKey(KeyboardKeyEvent),
    TouchDown(TouchEvent),
    TouchUp(TouchEvent),
    TouchMotion(TouchEvent),
    TouchCancel(TouchEvent),
    TouchFrame { time_usec: u64 },
    GestureSwipeBegin(GestureEvent),
    GestureSwipeUpdate(GestureEvent),
    GestureSwipeEnd(GestureEvent),
    GesturePinchBegin(GestureEvent),
    GesturePinchUpdate(GestureEvent),
    GesturePinchEnd(GestureEvent),
    SwitchToggle(SwitchEvent),
    TabletTool(TabletToolEvent),
    DeviceAdded,
    DeviceRemoved,
}

// ---------------------------------------------------------------------------
// LibinputEvent
// ---------------------------------------------------------------------------

pub struct LibinputEvent {
    pub event_type: LibinputEventType,
    pub payload: EventPayload,
    pub context: *mut LibinputContext,
    pub device: *mut LibinputDevice,
}

impl Drop for LibinputEvent {
    fn drop(&mut self) {
        if let EventPayload::TabletTool(event) = &mut self.payload {
            if !event.tool.is_null() {
                unsafe {
                    if (*event.tool).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
                        drop(Box::from_raw(event.tool));
                    }
                }
                event.tool = std::ptr::null_mut();
            }
        }
        if self.event_type != LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED
            || self.device.is_null()
        {
            return;
        }
        unsafe {
            if (*self.device).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
                drop(Box::from_raw(self.device));
            }
        }
        self.device = std::ptr::null_mut();
    }
}

// ---------------------------------------------------------------------------
// LibinputSeat
// ---------------------------------------------------------------------------

pub struct LibinputSeat {
    pub physical_name: CString,
    pub logical_name: CString,
    pub refcount: AtomicI32,
    pub user_data: *mut libc::c_void,
    pub context: *mut LibinputContext,
    pub button_count: AtomicU32,
}

// ---------------------------------------------------------------------------
// LibinputDeviceGroup
// ---------------------------------------------------------------------------

pub struct LibinputDeviceGroup {
    pub refcount: AtomicI32,
    pub user_data: *mut libc::c_void,
}

unsafe impl Send for LibinputDeviceGroup {}

impl LibinputDeviceGroup {
    fn new() -> Self {
        Self {
            refcount: AtomicI32::new(1),
            user_data: std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// LibinputDevice
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct LibinputDevice {
    pub name: CString,
    pub sysname: CString,
    pub devnode: CString,
    pub output_name: Option<CString>,
    pub vendor_id: u32,
    pub product_id: u32,
    pub bus_type: u32,
    pub has_keyboard: bool,
    pub has_pointer: bool,
    pub has_touch: bool,
    pub has_gesture: bool,
    pub has_switch: bool,
    pub has_tablet: bool,
    pub has_tablet_pad: bool,
    pub accel_available: bool,
    pub supports_button_scroll: bool,
    pub event_codes: Vec<u16>,
    pub touch_count: i32,
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
    pub abs_x_range: Option<(i32, i32)>,
    pub abs_y_range: Option<(i32, i32)>,
    pub abs_x_resolution: Option<i32>,
    pub abs_y_resolution: Option<i32>,
    pub send_events_modes: u32,
    pub send_events_mode: u32,
    pub tap_enabled: bool,
    pub tap_button_map: u32, // 0=LRM 1=LMR
    pub natural_scroll: bool,
    pub accel_speed: f64,
    pub accel_profile: u32,
    pub left_handed: bool,
    pub scroll_method: u32,
    pub scroll_default_method: u32,
    pub scroll_button: u32,
    pub scroll_default_button: u32,
    pub scroll_button_lock: u32,
    pub click_method: u32,
    pub middle_emulation_available: bool,
    pub middle_emulation: bool,
    pub middle_emulation_default: bool,
    pub dwt_enabled: bool,
    pub wheel_click_angle_vertical: f64,
    pub wheel_click_angle_horizontal: f64,
    pub calibration_available: bool,
    pub calibration: [f32; 6],
    pub default_calibration: [f32; 6],
    pub area_available: bool,
    pub area: [f64; 4],
    pub wanted_area: [f64; 4],
    pub tablet_in_proximity: bool,
    pub tablet_current_x: f64,
    pub tablet_current_y: f64,
    pub tablet_current_tilt_x: f64,
    pub refcount: AtomicI32,
    pub user_data: *mut libc::c_void,
    pub seat: *mut LibinputSeat,
    pub context: *mut LibinputContext,
    pub udev_device: *mut libc::c_void,
    pub group: *mut LibinputDeviceGroup,
}

unsafe impl Send for LibinputDevice {}

impl LibinputDevice {
    pub fn new(
        name: &str,
        devnode: &str,
        seat: *mut LibinputSeat,
        context: *mut LibinputContext,
    ) -> Self {
        Self {
            name: CString::new(name).unwrap_or_else(|_| CString::new("Unknown").unwrap()),
            sysname: CString::new("").unwrap(),
            devnode: CString::new(devnode).unwrap_or_else(|_| CString::new("").unwrap()),
            output_name: None,
            vendor_id: 0,
            product_id: 0,
            bus_type: 0,
            has_keyboard: false,
            has_pointer: false,
            has_touch: false,
            has_gesture: false,
            has_switch: false,
            has_tablet: false,
            has_tablet_pad: false,
            accel_available: false,
            supports_button_scroll: false,
            event_codes: Vec::new(),
            touch_count: 0,
            width_mm: None,
            height_mm: None,
            abs_x_range: None,
            abs_y_range: None,
            abs_x_resolution: None,
            abs_y_resolution: None,
            send_events_modes: 1,
            send_events_mode: 0,
            tap_enabled: false,
            tap_button_map: 0,
            natural_scroll: false,
            accel_speed: 0.0,
            accel_profile: 2,
            left_handed: false,
            scroll_method: 2,
            scroll_default_method: 2,
            scroll_button: 0,
            scroll_default_button: 0,
            scroll_button_lock: 0,
            click_method: 1,
            middle_emulation_available: false,
            middle_emulation: false,
            middle_emulation_default: false,
            dwt_enabled: true,
            wheel_click_angle_vertical: 15.0,
            wheel_click_angle_horizontal: 15.0,
            calibration_available: false,
            calibration: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            default_calibration: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            area_available: false,
            area: [0.0, 0.0, 1.0, 1.0],
            wanted_area: [0.0, 0.0, 1.0, 1.0],
            tablet_in_proximity: false,
            tablet_current_x: 0.0,
            tablet_current_y: 0.0,
            tablet_current_tilt_x: 0.0,
            refcount: AtomicI32::new(1),
            user_data: std::ptr::null_mut(),
            seat,
            context,
            udev_device: std::ptr::null_mut(),
            group: Box::into_raw(Box::new(LibinputDeviceGroup::new())),
        }
    }
}

impl Drop for LibinputDevice {
    fn drop(&mut self) {
        if !self.udev_device.is_null() {
            unsafe {
                crate::udev::udev_device_unref(self.udev_device);
            }
            self.udev_device = std::ptr::null_mut();
        }
        if !self.group.is_null() {
            unsafe {
                if (*self.group).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
                    drop(Box::from_raw(self.group));
                }
            }
            self.group = std::ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// LibinputContext
// ---------------------------------------------------------------------------

pub struct LibinputContext {
    pub interface: *const LibinputInterface,
    pub user_data: *mut libc::c_void,
    pub epoll_fd: RawFd,
    pub wake_fd: RawFd,
    pub timer_fd: RawFd,
    pub event_queue: VecDeque<LibinputEvent>,
    pub devices: Vec<*mut LibinputDevice>,
    pub touch_seat_slots: Vec<bool>,
    pub tablet_tools: Vec<*mut LibinputTabletTool>,
    pub seat: *mut LibinputSeat,
    pub seats: Vec<*mut LibinputSeat>,
    pub refcount: AtomicI32,
    pub log_handler: Option<
        unsafe extern "C" fn(
            ctx: *mut LibinputContext,
            priority: u32,
            format: *const libc::c_char,
            args: *mut libc::c_void,
        ),
    >,
    pub log_priority: u32,
    pub touch_arbitration_until: Option<Instant>,
    pub backend: Mutex<BackendState>,
    pub backend_kind: BackendKind,
    pub seat_assigned: bool,
}

unsafe impl Send for LibinputContext {}
unsafe impl Sync for LibinputContext {}

impl LibinputContext {
    pub fn new(
        interface: *const LibinputInterface,
        user_data: *mut libc::c_void,
        backend_kind: BackendKind,
    ) -> Self {
        let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        let wake_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        let timer_fd = unsafe {
            libc::timerfd_create(
                libc::CLOCK_MONOTONIC,
                libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
            )
        };
        let seat = Box::into_raw(Box::new(LibinputSeat {
            physical_name: CString::new("seat0").unwrap(),
            logical_name: CString::new("default").unwrap(),
            refcount: AtomicI32::new(1),
            user_data: std::ptr::null_mut(),
            context: std::ptr::null_mut(),
            button_count: AtomicU32::new(0),
        }));
        let backend = BackendState::new();
        let inotify_fd = backend.inotify_fd();
        let ctx = Self {
            interface,
            user_data,
            epoll_fd,
            wake_fd,
            timer_fd,
            event_queue: VecDeque::new(),
            devices: Vec::new(),
            touch_seat_slots: Vec::new(),
            tablet_tools: Vec::new(),
            seat,
            seats: vec![seat],
            refcount: AtomicI32::new(1),
            log_handler: None,
            log_priority: 30,
            touch_arbitration_until: None,
            backend: Mutex::new(backend),
            backend_kind,
            seat_assigned: false,
        };
        if let Some(fd) = inotify_fd {
            ctx.register_fd(fd);
        }
        if wake_fd >= 0 {
            ctx.register_fd(wake_fd);
        }
        if timer_fd >= 0 {
            ctx.register_fd(timer_fd);
        }
        ctx
    }

    pub fn register_fd(&self, fd: RawFd) {
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: fd as u64,
        };
        unsafe {
            libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut ev);
        }
    }

    pub fn unregister_fd(&self, fd: RawFd) {
        unsafe {
            libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut());
        }
    }

    pub fn signal_fd(&self) {
        if self.wake_fd < 0 {
            return;
        }
        let value: u64 = 1;
        unsafe {
            libc::write(
                self.wake_fd,
                (&value as *const u64).cast(),
                std::mem::size_of::<u64>(),
            );
        }
    }

    pub fn drain_fd(&self) {
        let mut value: u64 = 0;
        for fd in [self.wake_fd, self.timer_fd] {
            if fd < 0 {
                continue;
            }
            unsafe {
                while libc::read(
                    fd,
                    (&mut value as *mut u64).cast(),
                    std::mem::size_of::<u64>(),
                ) > 0
                {}
            }
        }
    }

    pub fn arm_timer(&self, delay: Option<std::time::Duration>) {
        if self.timer_fd < 0 {
            return;
        }
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if let Some(delay) = delay {
            value.tv_sec = delay.as_secs().try_into().unwrap_or(libc::time_t::MAX);
            value.tv_nsec = libc::c_long::from(delay.subsec_nanos().max(1));
        }
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: value,
        };
        unsafe {
            libc::timerfd_settime(self.timer_fd, 0, &spec, std::ptr::null_mut());
        }
    }

    pub fn inc_ref(&self) {
        self.refcount.fetch_add(1, Ordering::SeqCst);
    }
    pub fn dec_ref(&self) -> i32 {
        self.refcount.fetch_sub(1, Ordering::SeqCst) - 1
    }
}

impl Drop for LibinputContext {
    fn drop(&mut self) {
        if let Ok(backend) = self.backend.get_mut() {
            unsafe {
                backend.close_all(self.interface, self.user_data);
            }
        }
        if self.epoll_fd >= 0 {
            unsafe {
                libc::close(self.epoll_fd);
            }
        }
        if self.wake_fd >= 0 {
            unsafe {
                libc::close(self.wake_fd);
            }
        }
        if self.timer_fd >= 0 {
            unsafe {
                libc::close(self.timer_fd);
            }
        }
        for seat in self.seats.drain(..) {
            if !seat.is_null() {
                unsafe {
                    if (*seat).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
                        drop(Box::from_raw(seat));
                    }
                }
            }
        }
        for dev_ptr in self.devices.drain(..) {
            if !dev_ptr.is_null() {
                unsafe {
                    if (*dev_ptr).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
                        drop(Box::from_raw(dev_ptr));
                    }
                }
            }
        }
        for tool in self.tablet_tools.drain(..) {
            if !tool.is_null() {
                unsafe {
                    if (*tool).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
                        drop(Box::from_raw(tool));
                    }
                }
            }
        }
    }
}
