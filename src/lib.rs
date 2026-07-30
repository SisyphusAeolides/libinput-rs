//! Rust implementation of the libinput.so.10 ABI.
//!
//! Version 0.3.1 is a tested drop-in replacement for libinput 1.31.3 on the
//! supported x86_64 DNF/RPM targets. The release gate covers the complete
//! public ABI and the pinned upstream public-ABI behavioral corpus.

#![allow(non_snake_case, clippy::missing_safety_doc)]

mod backend;
pub mod capforge;
pub mod chwd_input;
#[doc(hidden)]
pub mod elan_recover;
pub mod evdev;
mod evtrans;
mod ffi_types;
mod hwdetect;
mod motion;
mod quirks;
mod tpad;
mod udev;
#[doc(hidden)]
pub mod udev_callout;

use crate::ffi_types::{
    BackendKind, EventPayload, LibinputContext, LibinputDevice, LibinputDeviceGroup, LibinputEvent,
    LibinputEventType, LibinputInterface, LibinputSeat, LibinputTabletPadModeGroup,
    LibinputTabletTool,
};

use std::ffi::CStr;
use std::os::unix::io::RawFd;

extern "C" {
    fn input_emit_log(
        handler: *mut libc::c_void,
        context: *mut libc::c_void,
        priority: u32,
        format: *const libc::c_char,
        ...
    );
}

#[repr(C)]
pub struct LibinputConfigAreaRectangle {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

unsafe fn populate_events(ctx: *mut LibinputContext) {
    if ctx.is_null() {
        return;
    }
    let ctx_ref = &mut *ctx;
    let mut tmp: std::collections::VecDeque<LibinputEvent> = std::collections::VecDeque::new();
    if let Ok(mut backend) = ctx_ref.backend.lock() {
        backend.drain_into_queue(ctx, &mut tmp);
    }
    enqueue_events(ctx, tmp);
}

unsafe fn enqueue_event(ctx: *mut LibinputContext, event: LibinputEvent) {
    if !event.device.is_null()
        && event.event_type != LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED
    {
        libinput_device_ref(event.device);
    }
    (*ctx).event_queue.push_back(event);
}

unsafe fn enqueue_events(
    ctx: *mut LibinputContext,
    events: impl IntoIterator<Item = LibinputEvent>,
) {
    for event in events {
        enqueue_event(ctx, event);
    }
}

pub(crate) unsafe fn emit_debug_log(ctx: *mut LibinputContext, message: &str) {
    if ctx.is_null() || (*ctx).log_priority > 10 {
        return;
    }
    let Some(handler) = (*ctx).log_handler else {
        return;
    };
    let Ok(message) = std::ffi::CString::new(format!("{}\n", message.replace('%', "%%"))) else {
        return;
    };
    input_emit_log(
        handler as *mut libc::c_void,
        ctx.cast(),
        10,
        message.as_ptr(),
    );
}

pub(crate) unsafe fn emit_error_log(ctx: *mut LibinputContext, message: &str) {
    if ctx.is_null() || (*ctx).log_priority > 30 {
        return;
    }
    let Some(handler) = (*ctx).log_handler else {
        return;
    };
    let Ok(message) = std::ffi::CString::new(format!("{}\n", message.replace('%', "%%"))) else {
        return;
    };
    input_emit_log(
        handler as *mut libc::c_void,
        ctx.cast(),
        30,
        message.as_ptr(),
    );
}

pub(crate) unsafe fn emit_info_log(ctx: *mut LibinputContext, message: &str) {
    if ctx.is_null() || (*ctx).log_priority > 20 {
        return;
    }
    let Some(handler) = (*ctx).log_handler else {
        return;
    };
    let Ok(message) = std::ffi::CString::new(format!("{}\n", message.replace('%', "%%"))) else {
        return;
    };
    input_emit_log(
        handler as *mut libc::c_void,
        ctx.cast(),
        20,
        message.as_ptr(),
    );
}

// ---------------------------------------------------------------------------
// Context lifecycle
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_udev_create_context(
    interface: *const LibinputInterface,
    user_data: *mut libc::c_void,
    udev: *mut libc::c_void,
) -> *mut LibinputContext {
    if interface.is_null() || udev.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = Box::into_raw(Box::new(LibinputContext::new(
        interface,
        user_data,
        BackendKind::Udev,
    )));
    (*(*ctx).seat).context = ctx;
    ctx
}

#[no_mangle]
pub unsafe extern "C" fn libinput_path_create_context(
    interface: *const LibinputInterface,
    user_data: *mut libc::c_void,
) -> *mut LibinputContext {
    if interface.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = Box::into_raw(Box::new(LibinputContext::new(
        interface,
        user_data,
        BackendKind::Path,
    )));
    (*(*ctx).seat).context = ctx;
    ctx
}

#[no_mangle]
pub unsafe extern "C" fn libinput_ref(ctx: *mut LibinputContext) -> *mut LibinputContext {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    (*ctx).inc_ref();
    ctx
}

#[no_mangle]
pub unsafe extern "C" fn libinput_unref(ctx: *mut LibinputContext) -> *mut LibinputContext {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    if (*ctx).dec_ref() == 0 {
        drop(Box::from_raw(ctx));
        return std::ptr::null_mut();
    }
    ctx
}

#[no_mangle]
pub unsafe extern "C" fn libinput_udev_assign_seat(
    ctx: *mut LibinputContext,
    seat_name: *const libc::c_char,
) -> libc::c_int {
    if ctx.is_null()
        || seat_name.is_null()
        || (*ctx).backend_kind != BackendKind::Udev
        || (*ctx).seat_assigned
    {
        return -1;
    }
    let seat_name = CStr::from_ptr(seat_name);
    if seat_name.to_bytes().len() > 255 {
        return -1;
    }
    let name = seat_name.to_string_lossy().into_owned();
    if let Ok(cname) = std::ffi::CString::new(name) {
        (*(*ctx).seat).physical_name = cname;
    }
    (*ctx).seat_assigned = true;
    (*ctx).plugins_loaded = true;
    let mut tmp: Vec<LibinputEvent> = Vec::new();
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.scan_and_open(ctx, &mut tmp);
    }
    for ev in tmp {
        enqueue_event(ctx, ev);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_path_add_device(
    ctx: *mut LibinputContext,
    path: *const libc::c_char,
) -> *mut LibinputDevice {
    if ctx.is_null() || path.is_null() || (*ctx).backend_kind != BackendKind::Path {
        return std::ptr::null_mut();
    }
    let path = CStr::from_ptr(path);
    if path.to_bytes().len() > libc::PATH_MAX as usize {
        emit_error_log(
            ctx,
            &format!(
                "client bug: Unexpected path, limited to {} characters.",
                libc::PATH_MAX
            ),
        );
        return std::ptr::null_mut();
    }
    let devnode = path.to_string_lossy().into_owned();
    (*ctx).plugins_loaded = true;
    let p = std::path::PathBuf::from(&devnode);
    use std::os::unix::fs::FileTypeExt;
    if !p
        .metadata()
        .is_ok_and(|metadata| metadata.file_type().is_char_device())
    {
        emit_error_log(ctx, "failed to add device");
        return std::ptr::null_mut();
    }
    let mut tmp: Vec<LibinputEvent> = Vec::new();
    let old_len = (*ctx).devices.len();
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.try_open(ctx, &p, &mut tmp);
    }
    for ev in tmp {
        enqueue_event(ctx, ev);
    }
    if (*ctx).devices.len() == old_len + 1 {
        if let Ok(mut backend) = (*ctx).backend.lock() {
            backend.remember_path(&p);
        }
        emit_info_log(ctx, "device added");
        (&(*ctx).devices)[old_len]
    } else {
        emit_error_log(ctx, "failed to add device");
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_path_remove_device(dev: *mut LibinputDevice) {
    if dev.is_null() {
        return;
    }
    let ctx = (*dev).context;
    if ctx.is_null() || (*ctx).backend_kind != BackendKind::Path {
        return;
    }
    let path = std::path::PathBuf::from((*dev).devnode.to_string_lossy().into_owned());
    let mut events = std::collections::VecDeque::new();
    let removed = if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.forget_path(&path);
        backend.remove_device(ctx, dev, &mut events)
    } else {
        false
    };
    if removed {
        (*ctx).devices.retain(|candidate| *candidate != dev);
        enqueue_events(ctx, events);
        libinput_device_unref(dev);
    }
}

// ---------------------------------------------------------------------------
// FD & dispatch
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_get_fd(ctx: *mut LibinputContext) -> RawFd {
    if ctx.is_null() {
        return -1;
    }
    (*ctx).epoll_fd
}

#[no_mangle]
pub unsafe extern "C" fn libinput_dispatch(ctx: *mut LibinputContext) -> libc::c_int {
    if ctx.is_null() {
        return -1;
    }
    let mut events: [libc::epoll_event; 16] = std::mem::zeroed();
    libc::epoll_wait((*ctx).epoll_fd, events.as_mut_ptr(), 16, 0);
    (*ctx).drain_fd();
    populate_events(ctx);
    0
}

// ---------------------------------------------------------------------------
// Event retrieval & destruction
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_get_event(ctx: *mut LibinputContext) -> *mut LibinputEvent {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    match (*ctx).event_queue.pop_front() {
        Some(ev) => Box::into_raw(Box::new(ev)),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_next_event_type(ctx: *mut LibinputContext) -> LibinputEventType {
    if ctx.is_null() {
        return LibinputEventType::LIBINPUT_EVENT_NONE;
    }
    (*ctx)
        .event_queue
        .front()
        .map(|e| e.event_type)
        .unwrap_or(LibinputEventType::LIBINPUT_EVENT_NONE)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_destroy(event: *mut LibinputEvent) {
    if !event.is_null() {
        let mut event = Box::from_raw(event);
        event.release_queued_device_ref();
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_type(event: *const LibinputEvent) -> LibinputEventType {
    if event.is_null() {
        return LibinputEventType::LIBINPUT_EVENT_NONE;
    }
    (*event).event_type
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_context(
    event: *const LibinputEvent,
) -> *mut LibinputContext {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    (*event).context
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_device(
    event: *const LibinputEvent,
) -> *mut LibinputDevice {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    (*event).device
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_device_notify_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match (*event).event_type {
        LibinputEventType::LIBINPUT_EVENT_DEVICE_ADDED
        | LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED => event,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_device_notify_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

// ---------------------------------------------------------------------------
// Pointer event accessors
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_pointer_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match (*event).event_type {
        LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION
        | LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION_ABSOLUTE
        | LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON
        | LibinputEventType::LIBINPUT_EVENT_POINTER_AXIS
        | LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_WHEEL
        | LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_FINGER
        | LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_CONTINUOUS => event,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_time(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::PointerMotion(e) => (e.time_usec / 1000) as u32,
        EventPayload::PointerMotionAbsolute(e) => (e.time_usec / 1000) as u32,
        EventPayload::PointerButton(e) => (e.time_usec / 1000) as u32,
        EventPayload::PointerAxis(e) => (e.time_usec / 1000) as u32,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_time_usec(event: *const LibinputEvent) -> u64 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::PointerMotion(e) => e.time_usec,
        EventPayload::PointerMotionAbsolute(e) => e.time_usec,
        EventPayload::PointerButton(e) => e.time_usec,
        EventPayload::PointerAxis(e) => e.time_usec,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_dx(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotion(e) = &(*event).payload {
        e.dx
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_dy(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotion(e) = &(*event).payload {
        e.dy
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_dx_unaccelerated(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotion(e) = &(*event).payload {
        e.dx_unaccel
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_dy_unaccelerated(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotion(e) = &(*event).payload {
        e.dy_unaccel
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_absolute_x(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotionAbsolute(e) = &(*event).payload {
        e.abs_x
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_absolute_y(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotionAbsolute(e) = &(*event).payload {
        e.abs_y
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_button(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::PointerButton(e) = &(*event).payload {
        e.button
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_button_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::PointerButton(e) = &(*event).payload {
        e.state
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_seat_button_count(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::PointerButton(e) = &(*event).payload {
        e.seat_button_count
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_axis_value(
    event: *const LibinputEvent,
    axis: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerAxis(e) = &(*event).payload {
        e.value(axis)
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_axis_value_discrete(
    event: *const LibinputEvent,
    axis: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerAxis(e) = &(*event).payload {
        e.value_discrete(axis) as f64
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_axis_source(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::PointerAxis(e) = &(*event).payload {
        e.source
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_has_axis(
    event: *const LibinputEvent,
    axis: u32,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    matches!(&(*event).payload, EventPayload::PointerAxis(e) if e.has_axis(axis)) as libc::c_int
}

// ---------------------------------------------------------------------------
// Keyboard event accessors
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_keyboard_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    if (*event).event_type == LibinputEventType::LIBINPUT_EVENT_KEYBOARD_KEY {
        event
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_keyboard_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_keyboard_get_time(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::KeyboardKey(e) = &(*event).payload {
        (e.time_usec / 1000) as u32
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_keyboard_get_time_usec(event: *const LibinputEvent) -> u64 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::KeyboardKey(e) = &(*event).payload {
        e.time_usec
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_keyboard_get_key(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::KeyboardKey(e) = &(*event).payload {
        e.key
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_keyboard_get_key_state(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::KeyboardKey(e) = &(*event).payload {
        e.state
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_keyboard_get_seat_key_count(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::KeyboardKey(e) = &(*event).payload {
        e.seat_key_count
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Touch event accessors
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_touch_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match (*event).event_type {
        LibinputEventType::LIBINPUT_EVENT_TOUCH_DOWN
        | LibinputEventType::LIBINPUT_EVENT_TOUCH_UP
        | LibinputEventType::LIBINPUT_EVENT_TOUCH_MOTION
        | LibinputEventType::LIBINPUT_EVENT_TOUCH_CANCEL
        | LibinputEventType::LIBINPUT_EVENT_TOUCH_FRAME => event,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_time(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e)
        | EventPayload::TouchUp(e)
        | EventPayload::TouchMotion(e)
        | EventPayload::TouchCancel(e) => (e.time_usec / 1000) as u32,
        EventPayload::TouchFrame { time_usec } => (*time_usec / 1000) as u32,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_time_usec(event: *const LibinputEvent) -> u64 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e)
        | EventPayload::TouchUp(e)
        | EventPayload::TouchMotion(e)
        | EventPayload::TouchCancel(e) => e.time_usec,
        EventPayload::TouchFrame { time_usec } => *time_usec,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_slot(event: *const LibinputEvent) -> i32 {
    if event.is_null() {
        return -1;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) | EventPayload::TouchUp(e) => {
            e.slot
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_seat_slot(event: *const LibinputEvent) -> i32 {
    if event.is_null() {
        return -1;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) | EventPayload::TouchUp(e) => {
            e.seat_slot
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_x(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => {
            let device = (*event).device;
            if !device.is_null() {
                if let (Some((minimum, _)), Some(resolution)) =
                    ((*device).abs_x_range, (*device).abs_x_resolution)
                {
                    return (e.x - minimum as f64) / resolution as f64;
                }
            }
            e.x
        }
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_y(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => {
            let device = (*event).device;
            if !device.is_null() {
                if let (Some((minimum, _)), Some(resolution)) =
                    ((*device).abs_y_range, (*device).abs_y_resolution)
                {
                    return (e.y - minimum as f64) / resolution as f64;
                }
            }
            e.y
        }
        _ => 0.0,
    }
}

unsafe fn transformed_touch_coordinates(event: *const LibinputEvent) -> Option<(f64, f64)> {
    let touch = match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => e,
        _ => return None,
    };
    let device = (*event).device;
    if device.is_null() {
        return None;
    }
    let ((xmin, xmax), (ymin, ymax)) = ((*device).abs_x_range?, (*device).abs_y_range?);
    let x_span = (xmax as i64 - xmin as i64 + 1).max(1) as f64;
    let y_span = (ymax as i64 - ymin as i64 + 1).max(1) as f64;
    let x = (touch.x - xmin as f64) / x_span;
    let y = (touch.y - ymin as f64) / y_span;
    let matrix = (*device).calibration;
    Some((
        matrix[0] as f64 * x + matrix[1] as f64 * y + matrix[2] as f64,
        matrix[3] as f64 * x + matrix[4] as f64 * y + matrix[5] as f64,
    ))
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_x_transformed(
    event: *const LibinputEvent,
    width: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    transformed_touch_coordinates(event)
        .map(|(x, _)| x * width as f64)
        .unwrap_or(0.0)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_y_transformed(
    event: *const LibinputEvent,
    height: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    transformed_touch_coordinates(event)
        .map(|(_, y)| y * height as f64)
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Gesture event accessors
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_gesture_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match (*event).event_type {
        LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_BEGIN
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_UPDATE
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_SWIPE_END
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_BEGIN
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_UPDATE
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_END
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_BEGIN
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_HOLD_END => event,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_time(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeBegin(e)
        | EventPayload::GestureSwipeUpdate(e)
        | EventPayload::GestureSwipeEnd(e)
        | EventPayload::GesturePinchBegin(e)
        | EventPayload::GesturePinchUpdate(e)
        | EventPayload::GesturePinchEnd(e)
        | EventPayload::GestureHoldBegin(e)
        | EventPayload::GestureHoldEnd(e) => (e.time_usec / 1000) as u32,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_time_usec(event: *const LibinputEvent) -> u64 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeBegin(e)
        | EventPayload::GestureSwipeUpdate(e)
        | EventPayload::GestureSwipeEnd(e)
        | EventPayload::GesturePinchBegin(e)
        | EventPayload::GesturePinchUpdate(e)
        | EventPayload::GesturePinchEnd(e)
        | EventPayload::GestureHoldBegin(e)
        | EventPayload::GestureHoldEnd(e) => e.time_usec,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_finger_count(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeBegin(e)
        | EventPayload::GestureSwipeUpdate(e)
        | EventPayload::GestureSwipeEnd(e)
        | EventPayload::GesturePinchBegin(e)
        | EventPayload::GesturePinchUpdate(e)
        | EventPayload::GesturePinchEnd(e)
        | EventPayload::GestureHoldBegin(e)
        | EventPayload::GestureHoldEnd(e) => e.finger_count,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_dx(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeUpdate(e) | EventPayload::GesturePinchUpdate(e) => e.dx,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_dy(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeUpdate(e) | EventPayload::GesturePinchUpdate(e) => e.dy,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_dx_unaccelerated(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeUpdate(e) | EventPayload::GesturePinchUpdate(e) => e.dx_unaccel,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_dy_unaccelerated(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeUpdate(e) | EventPayload::GesturePinchUpdate(e) => e.dy_unaccel,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_scale(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 1.0;
    }
    match &(*event).payload {
        EventPayload::GesturePinchUpdate(e) | EventPayload::GesturePinchEnd(e) => e.scale,
        _ => 1.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_angle_delta(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::GesturePinchUpdate(e) => e.angle,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_cancelled(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::GestureSwipeEnd(e)
        | EventPayload::GesturePinchEnd(e)
        | EventPayload::GestureHoldEnd(e) => e.cancelled as libc::c_int,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Switch event accessors
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_switch_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    if (*event).event_type == LibinputEventType::LIBINPUT_EVENT_SWITCH_TOGGLE {
        event
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_switch_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_switch_get_switch(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::SwitchToggle(e) = &(*event).payload {
        e.switch
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_switch_get_switch_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::SwitchToggle(e) = &(*event).payload {
        e.state
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Device info
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_ref(dev: *mut LibinputDevice) -> *mut LibinputDevice {
    if dev.is_null() {
        return std::ptr::null_mut();
    }
    let current = (*dev)
        .refcount
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    (*dev).abi.refcount = current + 1;
    dev
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_unref(dev: *mut LibinputDevice) -> *mut LibinputDevice {
    if dev.is_null() {
        return std::ptr::null_mut();
    }
    let remaining = (*dev)
        .refcount
        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
        - 1;
    (*dev).abi.refcount = remaining;
    if remaining <= 0 {
        drop(Box::from_raw(dev));
        std::ptr::null_mut()
    } else {
        dev
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_name(
    dev: *const LibinputDevice,
) -> *const libc::c_char {
    if dev.is_null() {
        return std::ptr::null();
    }
    (*dev).name.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_sysname(
    dev: *const LibinputDevice,
) -> *const libc::c_char {
    if dev.is_null() {
        return std::ptr::null();
    }
    (*dev).sysname.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_output_name(
    dev: *const LibinputDevice,
) -> *const libc::c_char {
    if dev.is_null() {
        return std::ptr::null();
    }
    (*dev)
        .output_name
        .as_ref()
        .map_or(std::ptr::null(), |name| name.as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_id_vendor(dev: *const LibinputDevice) -> libc::c_uint {
    if dev.is_null() {
        return 0;
    }
    (*dev).vendor_id
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_id_product(
    dev: *const LibinputDevice,
) -> libc::c_uint {
    if dev.is_null() {
        return 0;
    }
    (*dev).product_id
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_context(
    dev: *const LibinputDevice,
) -> *mut LibinputContext {
    if dev.is_null() {
        return std::ptr::null_mut();
    }
    (*dev).context
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_devnode(
    dev: *const LibinputDevice,
) -> *const libc::c_char {
    if dev.is_null() {
        return std::ptr::null();
    }
    (*dev).devnode.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_touch_get_touch_count(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() || !(*dev).has_touch {
        return -1;
    }
    (*dev).touch_count
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_has_capability(
    dev: *const LibinputDevice,
    capability: u32,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    let has = match capability {
        0 => (*dev).has_keyboard,
        1 => (*dev).has_pointer,
        2 => (*dev).has_touch,
        3 => (*dev).has_tablet,
        4 => (*dev).has_tablet_pad,
        5 => (*dev).has_gesture,
        6 => (*dev).has_switch,
        _ => false,
    };
    has as libc::c_int
}

// ---------------------------------------------------------------------------
// Device configuration — tap
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_finger_count(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).tap_finger_count
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if enabled > 1 {
        return 2;
    }
    if (*dev).tap_finger_count == 0 {
        return if enabled == 0 { 0 } else { 1 };
    }
    (*dev).tap_enabled = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_enabled(dev: *const LibinputDevice) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).tap_enabled as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_default_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).tap_default_enabled as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_drag_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if enabled > 1 {
        return 2;
    }
    if (*dev).tap_finger_count == 0 {
        return if enabled == 0 { 0 } else { 1 };
    }
    (*dev).tap_drag_enabled = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_drag_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).tap_drag_enabled as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_default_drag_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    ((*dev).tap_finger_count != 0) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_drag_lock_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if enabled > 2 {
        return 2;
    }
    if (*dev).tap_finger_count == 0 {
        return if enabled == 0 { 0 } else { 1 };
    }
    (*dev).tap_drag_lock_enabled = if enabled == 1 { 1 } else { enabled };
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_drag_lock_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).tap_drag_lock_enabled
}

/// Button map: 0 = LRM (default), 1 = LMR
#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_button_map(
    dev: *mut LibinputDevice,
    map: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if map > 1 {
        return 2;
    }
    if (*dev).tap_finger_count == 0 {
        return 1;
    }
    (*dev).tap_button_map = map;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_button_map(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).tap_button_map
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_default_button_map(
    _dev: *const LibinputDevice,
) -> u32 {
    0
} // LIBINPUT_CONFIG_TAP_MAP_LRM

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_3fg_drag_get_finger_count(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() || !(*dev).has_gesture {
        return 0;
    }
    (*dev).mt_slot_count
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_3fg_drag_set_enabled(
    dev: *mut LibinputDevice,
    enable: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !matches!(enable, 0..=2) {
        return 2;
    }
    if libinput_device_config_3fg_drag_get_finger_count(dev) < 3 {
        return 1;
    }
    (*dev).drag_3fg_enabled = enable;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_3fg_drag_get_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).drag_3fg_enabled
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_3fg_drag_get_default_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Device configuration — pointer acceleration
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_config_accel_create(profile: u32) -> *mut libc::c_void {
    if !matches!(profile, 1 | 2 | 4) {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(crate::ffi_types::AccelConfig::new(profile))) as *mut libc::c_void
}

#[no_mangle]
pub unsafe extern "C" fn libinput_config_accel_destroy(accel_config: *mut libc::c_void) {
    if !accel_config.is_null() {
        drop(Box::from_raw(
            accel_config as *mut crate::ffi_types::AccelConfig,
        ));
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_config_accel_set_points(
    accel_config: *mut libc::c_void,
    accel_type: u32,
    step: f64,
    npoints: libc::size_t,
    points: *const f64,
) -> u32 {
    if accel_config.is_null()
        || points.is_null()
        || !step.is_finite()
        || step <= 0.0
        || step > 10_000.0
        || !(2..=64).contains(&npoints)
    {
        return 2;
    }
    let config = &mut *(accel_config as *mut crate::ffi_types::AccelConfig);
    if config.profile != 4 || !matches!(accel_type, 0..=2) {
        return 2;
    }
    let points = std::slice::from_raw_parts(points, npoints);
    if points
        .iter()
        .any(|point| !point.is_finite() || *point < 0.0 || *point > 10_000.0)
    {
        return 2;
    }
    let curve = crate::ffi_types::AccelCurve::new(step, points.to_vec());
    match accel_type {
        0 => config.fallback = Some(curve),
        1 => config.motion = Some(curve),
        2 => config.scroll = Some(curve),
        _ => unreachable!(),
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_apply(
    dev: *mut LibinputDevice,
    accel_config: *mut libc::c_void,
) -> u32 {
    if dev.is_null() || accel_config.is_null() {
        return 2;
    }
    if !(*dev).accel_available {
        return 1;
    }
    let config = &*(accel_config as *const crate::ffi_types::AccelConfig);
    let profile = config.profile;
    if profile & libinput_device_config_accel_get_profiles(dev) == 0 {
        return 1;
    }
    (*dev).accel_profile = profile;
    (*dev).accel_speed = 0.0;
    (*dev).accel_custom = (profile == 4).then(|| config.clone());
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).accel_available as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_set_speed(
    dev: *mut LibinputDevice,
    speed: f64,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !speed.is_finite() || !(-1.0..=1.0).contains(&speed) {
        return 2;
    }
    if !(*dev).accel_available {
        return 1;
    }
    (*dev).accel_speed = speed;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_speed(dev: *const LibinputDevice) -> f64 {
    if dev.is_null() {
        return 0.0;
    }
    (*dev).accel_speed
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_default_speed(
    _dev: *const LibinputDevice,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_profiles(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    if (*dev).accel_available
        && !((*dev).has_tablet && !(*dev).has_gesture && !(*dev).has_tablet_pad)
    {
        0b111
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_set_profile(
    dev: *mut LibinputDevice,
    profile: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if profile == 0 || profile & !0b111 != 0 || profile.count_ones() != 1 {
        return 2;
    }
    if profile & libinput_device_config_accel_get_profiles(dev) == 0 {
        return 1;
    }
    (*dev).accel_profile = profile;
    (*dev).accel_custom = if profile == 4 {
        Some(crate::ffi_types::AccelConfig::new(profile))
    } else {
        None
    };
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_profile(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    if (*dev).accel_available
        && !((*dev).has_tablet && !(*dev).has_gesture && !(*dev).has_tablet_pad)
    {
        (*dev).accel_profile
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_default_profile(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null()
        || !(*dev).accel_available
        || ((*dev).has_tablet && !(*dev).has_gesture && !(*dev).has_tablet_pad)
    {
        0
    } else {
        2
    }
}

// ---------------------------------------------------------------------------
// Device configuration — natural scroll
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_has_natural_scroll(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).has_pointer as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_natural_scroll_enabled(
    dev: *mut LibinputDevice,
    enabled: libc::c_int,
) -> u32 {
    if dev.is_null() || !(*dev).has_pointer {
        return 1;
    }
    (*dev).natural_scroll = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_natural_scroll_enabled(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).natural_scroll as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_default_natural_scroll_enabled(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    ((*dev).scroll_methods & 2 != 0 && (*dev).vendor_id == 0x05ac) as libc::c_int
}

// ---------------------------------------------------------------------------
// Device configuration — left-handed
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_left_handed_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).left_handed_available as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_left_handed_set(
    dev: *mut LibinputDevice,
    enabled: libc::c_int,
) -> u32 {
    if dev.is_null() || !(*dev).left_handed_available {
        return 1;
    }
    (*dev).left_handed = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_left_handed_get(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).left_handed as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_left_handed_get_default(
    _dev: *const LibinputDevice,
) -> libc::c_int {
    0
}

// ---------------------------------------------------------------------------
// Device configuration — scroll method
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_methods(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).scroll_methods
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_method(
    dev: *mut LibinputDevice,
    method: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !matches!(method, 0 | 1 | 2 | 4) {
        return 2;
    }
    if method != 0 && method & (*dev).scroll_methods == 0 {
        return 1;
    }
    if (*dev).scroll_method == method {
        return 0;
    }
    let ctx = (*dev).context;
    if !ctx.is_null() {
        let mut events = std::collections::VecDeque::new();
        if let Ok(mut backend) = (*ctx).backend.try_lock() {
            backend.stop_scroll_for_device(ctx, dev, &mut events);
        }
        enqueue_events(ctx, events);
    }
    (*dev).scroll_method = method;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_method(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).scroll_method
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_default_method(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        0
    } else {
        (*dev).scroll_default_method
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_button(
    dev: *mut LibinputDevice,
    button: u32,
) -> u32 {
    if dev.is_null() || !(*dev).supports_button_scroll {
        return 1;
    }
    if button != 0
        && match u16::try_from(button) {
            Ok(button) => !(*dev).event_codes.contains(&button),
            Err(_) => true,
        }
    {
        return 2;
    }
    (*dev).scroll_button = button;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_button(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).supports_button_scroll {
        0
    } else {
        (*dev).scroll_button
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_button_lock(
    dev: *mut LibinputDevice,
    state: u32,
) -> u32 {
    if dev.is_null() || !(*dev).supports_button_scroll {
        return 1;
    }
    if state > 1 {
        return 2;
    }
    (*dev).scroll_button_lock = state;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_button_lock(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).supports_button_scroll {
        0
    } else {
        (*dev).scroll_button_lock
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_default_button_lock(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Device configuration — click method
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_methods(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).click_methods
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_set_method(
    dev: *mut LibinputDevice,
    method: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !matches!(method, 0..=2) {
        return 2;
    }
    if method != 0 && method & (*dev).click_methods == 0 {
        return 1;
    }
    (*dev).click_method = method;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_method(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).click_method
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_default_method(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        0
    } else {
        (*dev).click_default_method
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_set_clickfinger_button_map(
    dev: *mut LibinputDevice,
    map: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !matches!(map, 0 | 1) {
        return 2;
    }
    if (*dev).click_methods & 2 == 0 {
        return 1;
    }
    (*dev).clickfinger_button_map = map;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_clickfinger_button_map(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || (*dev).click_methods & 2 == 0 {
        0
    } else {
        (*dev).clickfinger_button_map
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_default_clickfinger_button_map(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || (*dev).click_methods & 2 == 0 {
        0
    } else {
        (*dev).clickfinger_default_button_map
    }
}

// ---------------------------------------------------------------------------
// Device configuration — middle button emulation
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).middle_emulation_available as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if enabled > 1 {
        return 2;
    }
    if enabled == 1 && !(*dev).middle_emulation_available {
        return 1;
    }
    (*dev).middle_emulation = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_get_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).middle_emulation_available {
        return 0;
    }
    (*dev).middle_emulation as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_get_default_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).middle_emulation_available {
        return 0;
    }
    (*dev).middle_emulation_default as u32
}

// ---------------------------------------------------------------------------
// Device configuration — disable-while-typing
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    (!dev.is_null() && (*dev).dwt_available) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !matches!(enabled, 0 | 1) {
        return 2;
    }
    if !(*dev).dwt_available {
        return if enabled == 0 { 0 } else { 1 };
    }
    (*dev).dwt_enabled = enabled == 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_enabled(dev: *const LibinputDevice) -> u32 {
    if dev.is_null() || !(*dev).dwt_available {
        return 0;
    }
    (*dev).dwt_enabled as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_default_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    (!dev.is_null() && (*dev).dwt_available) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_set_timeout(
    dev: *mut LibinputDevice,
    millis: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if millis == 0 {
        return 2;
    }
    if !(*dev).dwt_available {
        return 1;
    }
    if !(100..=5000).contains(&millis) {
        return 2;
    }
    (*dev).dwt_timeout = millis;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_timeout(dev: *const LibinputDevice) -> u32 {
    if dev.is_null() || !(*dev).dwt_available {
        0
    } else {
        (*dev).dwt_timeout
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_default_timeout(
    dev: *const LibinputDevice,
) -> u32 {
    if !dev.is_null() && (*dev).dwt_available {
        500
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    (!dev.is_null() && (*dev).dwtp_available) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if !matches!(enabled, 0 | 1) {
        return 2;
    }
    if !(*dev).dwtp_available {
        return if enabled == 0 { 0 } else { 1 };
    }
    (*dev).dwtp_enabled = enabled == 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).dwtp_available {
        0
    } else {
        (*dev).dwtp_enabled as u32
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_default_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    (!dev.is_null() && (*dev).dwtp_available) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_set_timeout(
    dev: *mut LibinputDevice,
    millis: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if millis == 0 {
        return 2;
    }
    if !(*dev).dwtp_available {
        return 1;
    }
    if !(100..=5000).contains(&millis) {
        return 2;
    }
    (*dev).dwtp_timeout = millis;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_timeout(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).dwtp_available {
        0
    } else {
        (*dev).dwtp_timeout
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_default_timeout(
    dev: *const LibinputDevice,
) -> u32 {
    if !dev.is_null() && (*dev).dwtp_available {
        300
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Device configuration — calibration matrix
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_calibration_has_matrix(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).calibration_available as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_calibration_set_matrix(
    dev: *mut LibinputDevice,
    matrix: *const f32,
) -> u32 {
    if dev.is_null() || matrix.is_null() || !(*dev).calibration_available {
        return 1;
    }
    (*dev)
        .calibration
        .copy_from_slice(std::slice::from_raw_parts(matrix, 6));
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_calibration_get_matrix(
    dev: *const LibinputDevice,
    matrix: *mut f32,
) -> libc::c_int {
    if dev.is_null() || matrix.is_null() || !(*dev).calibration_available {
        return 0;
    }
    std::slice::from_raw_parts_mut(matrix, 6).copy_from_slice(&(*dev).calibration);
    ((*dev).calibration != [1.0_f32, 0.0, 0.0, 0.0, 1.0, 0.0]) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_calibration_get_default_matrix(
    dev: *const LibinputDevice,
    matrix: *mut f32,
) -> libc::c_int {
    if dev.is_null() || matrix.is_null() || !(*dev).calibration_available {
        return 0;
    }
    std::slice::from_raw_parts_mut(matrix, 6).copy_from_slice(&(*dev).default_calibration);
    ((*dev).default_calibration != [1.0_f32, 0.0, 0.0, 0.0, 1.0, 0.0]) as libc::c_int
}

// ---------------------------------------------------------------------------
// Seat
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_seat(dev: *const LibinputDevice) -> *mut libc::c_void {
    if dev.is_null() {
        return std::ptr::null_mut();
    }
    (*dev).seat as *mut libc::c_void
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_set_seat_logical_name(
    dev: *mut LibinputDevice,
    name: *const libc::c_char,
) -> libc::c_int {
    if dev.is_null() || name.is_null() || (*dev).seat.is_null() {
        return -1;
    }
    let name = CStr::from_ptr(name);
    if name.to_bytes().is_empty() || name.to_bytes().len() > 255 {
        return -1;
    }
    if (*(*dev).seat).logical_name.as_c_str() == name {
        return 0;
    }
    let Ok(logical_name) = std::ffi::CString::new(name.to_bytes()) else {
        return -1;
    };
    let ctx = (*dev).context;
    if ctx.is_null() {
        return -1;
    }
    let physical_name = (*(*dev).seat).physical_name.clone();
    let new_seat = (*ctx)
        .seats
        .iter()
        .copied()
        .find(|seat| {
            !seat.is_null()
                && (**seat).physical_name == physical_name
                && (**seat).logical_name == logical_name
        })
        .unwrap_or_else(|| {
            let seat = Box::into_raw(Box::new(LibinputSeat {
                physical_name,
                logical_name,
                refcount: std::sync::atomic::AtomicI32::new(1),
                user_data: std::ptr::null_mut(),
                context: ctx,
                button_counts: std::sync::Mutex::new(crate::evtrans::empty_seat_code_counts()),
                key_counts: std::sync::Mutex::new(crate::evtrans::empty_seat_code_counts()),
            }));
            (*ctx).seats.push(seat);
            seat
        });

    let path = std::path::PathBuf::from((*dev).devnode.to_string_lossy().into_owned());
    let mut removed = std::collections::VecDeque::new();
    let mut added = Vec::new();
    let replaced = if let Ok(mut backend) = (*ctx).backend.lock() {
        if backend.remove_device(ctx, dev, &mut removed) {
            backend.try_open(ctx, &path, &mut added);
            true
        } else {
            false
        }
    } else {
        false
    };
    let Some(replacement) = added.first().map(|event| event.device) else {
        return -1;
    };
    if !replaced || replacement.is_null() {
        return -1;
    }
    (*ctx).devices.retain(|candidate| *candidate != dev);
    libinput_device_unref(dev);
    let old_seat = (*replacement).seat;
    if old_seat != new_seat {
        libinput_seat_ref(new_seat.cast());
        (*replacement).seat = new_seat;
        (*replacement).abi.seat = new_seat;
        libinput_seat_unref(old_seat.cast());
    }
    enqueue_events(ctx, removed);
    enqueue_events(ctx, added);
    (*ctx).signal_fd();
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_get_physical_name(
    seat: *const libc::c_void,
) -> *const libc::c_char {
    if seat.is_null() {
        return std::ptr::null();
    }
    (*(seat as *const LibinputSeat)).physical_name.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_get_logical_name(
    seat: *const libc::c_void,
) -> *const libc::c_char {
    if seat.is_null() {
        return std::ptr::null();
    }
    (*(seat as *const LibinputSeat)).logical_name.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_get_context(
    seat: *const libc::c_void,
) -> *mut LibinputContext {
    if seat.is_null() {
        return std::ptr::null_mut();
    }
    (*(seat as *const LibinputSeat)).context
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_ref(seat: *mut libc::c_void) -> *mut libc::c_void {
    if !seat.is_null() {
        (*(seat as *mut LibinputSeat))
            .refcount
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    seat
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_unref(seat: *mut libc::c_void) -> *mut libc::c_void {
    if seat.is_null() {
        return std::ptr::null_mut();
    }
    let seat = seat as *mut LibinputSeat;
    if (*seat)
        .refcount
        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
        == 1
    {
        drop(Box::from_raw(seat));
        std::ptr::null_mut()
    } else {
        seat.cast()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_set_user_data(
    seat: *mut libc::c_void,
    data: *mut libc::c_void,
) {
    if !seat.is_null() {
        (*(seat as *mut LibinputSeat)).user_data = data;
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_seat_get_user_data(
    seat: *const libc::c_void,
) -> *mut libc::c_void {
    if seat.is_null() {
        return std::ptr::null_mut();
    }
    (*(seat as *const LibinputSeat)).user_data
}

// ---------------------------------------------------------------------------
// Status strings
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_config_status_to_str(status: u32) -> *const libc::c_char {
    match status {
        0 => b"success\0".as_ptr().cast(),
        1 => b"unsupported\0".as_ptr().cast(),
        2 => b"invalid\0".as_ptr().cast(),
        _ => std::ptr::null(),
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_log_set_priority(ctx: *mut LibinputContext, priority: u32) {
    if !ctx.is_null() {
        (*ctx).log_priority = priority;
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_log_get_priority(ctx: *const LibinputContext) -> u32 {
    if ctx.is_null() {
        return 30;
    }
    (*ctx).log_priority
}

#[no_mangle]
pub unsafe extern "C" fn libinput_log_set_handler(
    ctx: *mut LibinputContext,
    handler: Option<
        unsafe extern "C" fn(
            ctx: *mut LibinputContext,
            priority: u32,
            format: *const libc::c_char,
            args: *mut libc::c_void,
        ),
    >,
) {
    if ctx.is_null() {
        return;
    }
    (*ctx).log_handler = handler;
}

// ---------------------------------------------------------------------------
// User data
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_set_user_data(
    ctx: *mut LibinputContext,
    data: *mut libc::c_void,
) {
    if ctx.is_null() {
        return;
    }
    (*ctx).user_data = data;
}

#[no_mangle]
pub unsafe extern "C" fn libinput_get_user_data(ctx: *const LibinputContext) -> *mut libc::c_void {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    (*ctx).user_data
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_set_user_data(
    dev: *mut LibinputDevice,
    data: *mut libc::c_void,
) {
    if dev.is_null() {
        return;
    }
    (*dev).user_data = data;
    (*dev).abi.user_data = data;
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_user_data(
    dev: *const LibinputDevice,
) -> *mut libc::c_void {
    if dev.is_null() {
        return std::ptr::null_mut();
    }
    (*dev).user_data
}

// ---------------------------------------------------------------------------
// ABI compatibility surface for compositors (KWin/GNOME)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_area_has_rectangle(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).area_available as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_area_set_rectangle(
    dev: *mut LibinputDevice,
    rectangle: *const LibinputConfigAreaRectangle,
) -> u32 {
    if dev.is_null() || !(*dev).area_available {
        return 1;
    }
    if rectangle.is_null() {
        return 2;
    }
    let rectangle = &*rectangle;
    if rectangle.x1 >= rectangle.x2
        || rectangle.y1 >= rectangle.y2
        || rectangle.x1 < 0.0
        || rectangle.x2 > 1.0
        || rectangle.y1 < 0.0
        || rectangle.y2 > 1.0
    {
        return 2;
    }
    (*dev).wanted_area = [rectangle.x1, rectangle.y1, rectangle.x2, rectangle.y2];
    if !(*dev).tablet_in_proximity {
        (*dev).area = (*dev).wanted_area;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_area_get_rectangle(
    dev: *const LibinputDevice,
) -> LibinputConfigAreaRectangle {
    let area = if dev.is_null() || !(*dev).area_available {
        [0.0, 0.0, 1.0, 1.0]
    } else {
        (*dev).area
    };
    LibinputConfigAreaRectangle {
        x1: area[0],
        y1: area[1],
        x2: area[2],
        y2: area[3],
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_area_get_default_rectangle(
    _dev: *const LibinputDevice,
) -> LibinputConfigAreaRectangle {
    LibinputConfigAreaRectangle {
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).rotation_available as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_set_angle(
    dev: *mut LibinputDevice,
    degrees_cw: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    if degrees_cw >= 360 {
        return 2;
    }
    if !(*dev).rotation_available && degrees_cw != 0 {
        return 1;
    }
    (*dev).rotation_angle = degrees_cw;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_get_angle(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        0
    } else {
        (*dev).rotation_angle
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_get_default_angle(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_default_button(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).supports_button_scroll {
        0
    } else {
        (*dev).scroll_default_button
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_send_events_get_modes(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).send_events_modes
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_send_events_set_mode(
    dev: *mut LibinputDevice,
    mode: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    let supported = (*dev).send_events_modes;
    if mode & !supported != 0 {
        return 1;
    }
    let previous = (*dev).send_events_mode;
    let next = if mode & 1 != 0 { 1 } else { mode };
    (*dev).send_events_mode = next;
    if previous != next && matches!(next, 1 | 2) {
        let ctx = (*dev).context;
        if !ctx.is_null() {
            let mut events = std::collections::VecDeque::new();
            if let Ok(mut backend) = (*ctx).backend.lock() {
                if next == 1 || backend.has_external_mouse() {
                    backend.release_active_inputs(ctx, dev, &mut events);
                }
            }
            enqueue_events(ctx, events);
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_send_events_get_mode(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).send_events_mode
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_send_events_get_default_mode(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_default_drag_lock_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_device_group(
    dev: *const LibinputDevice,
) -> *mut libc::c_void {
    if dev.is_null() {
        return std::ptr::null_mut();
    }
    (*dev).group.cast()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_ref(group: *mut libc::c_void) -> *mut libc::c_void {
    if group.is_null() {
        return std::ptr::null_mut();
    }
    let group = group.cast::<LibinputDeviceGroup>();
    (*group)
        .refcount
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    group.cast()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_unref(
    group: *mut libc::c_void,
) -> *mut libc::c_void {
    if group.is_null() {
        return std::ptr::null_mut();
    }
    let group = group.cast::<LibinputDeviceGroup>();
    if (*group)
        .refcount
        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
        == 1
    {
        drop(Box::from_raw(group));
        std::ptr::null_mut()
    } else {
        group.cast()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_set_user_data(
    group: *mut libc::c_void,
    data: *mut libc::c_void,
) {
    if !group.is_null() {
        (*group.cast::<LibinputDeviceGroup>()).user_data = data;
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_get_user_data(
    group: *const libc::c_void,
) -> *mut libc::c_void {
    if group.is_null() {
        return std::ptr::null_mut();
    }
    (*group.cast::<LibinputDeviceGroup>()).user_data
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_id_bustype(dev: *const LibinputDevice) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).bus_type
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_size(
    dev: *const LibinputDevice,
    width: *mut f64,
    height: *mut f64,
) -> libc::c_int {
    if dev.is_null() || width.is_null() || height.is_null() {
        return 0;
    }
    match ((*dev).width_mm, (*dev).height_mm) {
        (Some(w), Some(h)) => {
            *width = w;
            *height = h;
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_get_udev_device(
    dev: *const LibinputDevice,
) -> *mut libc::c_void {
    if dev.is_null() || (*dev).udev_device.is_null() {
        return std::ptr::null_mut();
    }
    udev::udev_device_ref((*dev).udev_device)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_keyboard_has_key(
    dev: *const LibinputDevice,
    key: u32,
) -> libc::c_int {
    if dev.is_null() || !(*dev).has_keyboard {
        return -1;
    }
    if key > u16::MAX as u32 {
        return 0;
    }
    (*dev).event_codes.contains(&(key as u16)) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_led_update(dev: *mut LibinputDevice, leds: u32) {
    if dev.is_null() || (*dev).context.is_null() {
        return;
    }
    let ctx = (*dev).context;
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.update_leds(dev, leds);
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_pointer_has_button(
    dev: *const LibinputDevice,
    button: u32,
) -> libc::c_int {
    if dev.is_null() || button > u16::MAX as u32 {
        return 0;
    }
    if !(*dev).has_pointer {
        return -1;
    }
    (*dev).event_codes.contains(&(button as u16)) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_switch_has_switch(
    dev: *const LibinputDevice,
    sw: u32,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    if !(*dev).has_switch {
        return 0;
    }
    let kernel_code = match sw {
        1 => 0,
        2 => 1,
        3 => 10,
        _ => return 0,
    };
    (*dev).switch_codes.contains(&kernel_code) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_mode_group(
    dev: *const LibinputDevice,
    index: u32,
) -> *mut libc::c_void {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return std::ptr::null_mut();
    }
    (*dev)
        .tablet_pad_mode_groups
        .iter()
        .copied()
        .find(|group| !group.is_null() && (**group).index == index)
        .unwrap_or(std::ptr::null_mut())
        .cast()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_buttons(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return u32::MAX;
    }
    (*dev).tablet_pad_button_codes.len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_dials(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return u32::MAX;
    }
    (*dev).tablet_pad_num_dials
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_mode_groups(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return u32::MAX;
    }
    (*dev).tablet_pad_mode_groups.len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_rings(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return u32::MAX;
    }
    (*dev).tablet_pad_num_rings
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_strips(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return u32::MAX;
    }
    (*dev).tablet_pad_num_strips
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_has_key(
    dev: *const LibinputDevice,
    code: u32,
) -> libc::c_int {
    if dev.is_null() || !(*dev).has_tablet_pad {
        return -1;
    }
    (code <= u16::MAX as u32 && (*dev).event_codes.contains(&(code as u16))) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_tablet_pad_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match (*event).event_type {
        LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_BUTTON
        | LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_RING
        | LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_STRIP
        | LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_KEY
        | LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_DIAL => event,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_get_tablet_tool_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match (*event).event_type {
        LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_AXIS
        | LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_PROXIMITY
        | LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_TIP
        | LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_BUTTON => event,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_base_event(
    event: *mut LibinputEvent,
) -> *mut LibinputEvent {
    event
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_absolute_x_transformed(
    event: *const LibinputEvent,
    width: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotionAbsolute(e) = &(*event).payload {
        let range = e.x_max - e.x_min;
        if range > 0.0 {
            (e.abs_x - e.x_min) * f64::from(width) / range
        } else {
            0.0
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_absolute_y_transformed(
    event: *const LibinputEvent,
    height: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerMotionAbsolute(e) = &(*event).payload {
        let range = e.y_max - e.y_min;
        if range > 0.0 {
            (e.abs_y - e.y_min) * f64::from(height) / range
        } else {
            0.0
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_scroll_value(
    event: *const LibinputEvent,
    axis: u32,
) -> f64 {
    libinput_event_pointer_get_axis_value(event, axis)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_scroll_value_v120(
    event: *const LibinputEvent,
    axis: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerAxis(e) = &(*event).payload {
        return e.value_v120(axis);
    }
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_switch_get_time_usec(event: *const LibinputEvent) -> u64 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::SwitchToggle(e) = &(*event).payload {
        e.time_usec
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_switch_get_time(event: *const LibinputEvent) -> u32 {
    (libinput_event_switch_get_time_usec(event) / 1000) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_button_number(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.button,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_button_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.button_state,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_key(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.key,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_key_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.key_state,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_dial_delta_v120(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.dial_delta_v120,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_dial_number(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.dial_number,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_mode(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.mode,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_mode_group(
    event: *const LibinputEvent,
) -> *mut libc::c_void {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.mode_group.cast(),
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_ring_number(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.ring_number,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_ring_position(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.ring_position,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_ring_source(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.ring_source,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_strip_number(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.strip_number,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_strip_position(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.strip_position,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_strip_source(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.strip_source,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_time_usec(
    event: *const LibinputEvent,
) -> u64 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletPad(pad) => pad.time_usec,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_time(event: *const LibinputEvent) -> u32 {
    (libinput_event_tablet_pad_get_time_usec(event) / 1000) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_button(event: *const LibinputEvent) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.button,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_button_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.button_state,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_seat_button_count(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.seat_button_count,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_distance(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.distance,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_dx(event: *const LibinputEvent) -> f64 {
    if event.is_null() || (*event).event_type != LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_AXIS
    {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.dx,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_dy(event: *const LibinputEvent) -> f64 {
    if event.is_null() || (*event).event_type != LibinputEventType::LIBINPUT_EVENT_TABLET_TOOL_AXIS
    {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.dy,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_pressure(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        let range = tablet.pressure_max - tablet.pressure_min;
        if range > 0.0 {
            ((tablet.pressure - tablet.pressure_min) / range).clamp(0.0, 1.0)
        } else {
            0.0
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_x(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        if tablet.x_resolution > 0.0 {
            (tablet.x - tablet.x_min) / tablet.x_resolution
        } else {
            tablet.x - tablet.x_min
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_y(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        if tablet.y_resolution > 0.0 {
            (tablet.y - tablet.y_min) / tablet.y_resolution
        } else {
            tablet.y - tablet.y_min
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_proximity_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        tablet.proximity_state
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_rotation(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.rotation,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_slider_position(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.slider,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_wheel_delta(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.wheel_delta,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_wheel_delta_discrete(
    event: *const LibinputEvent,
) -> i32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.wheel_discrete,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_size_major(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.size_major,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_size_minor(
    event: *const LibinputEvent,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.size_minor,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tilt_x(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.tilt_x,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tilt_y(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.tilt_y,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_time_usec(
    event: *const LibinputEvent,
) -> u64 {
    if event.is_null() {
        return 0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        tablet.time_usec
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_time(event: *const LibinputEvent) -> u32 {
    (libinput_event_tablet_tool_get_time_usec(event) / 1000) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tip_state(
    event: *const LibinputEvent,
) -> u32 {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.tip_state,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tool(
    event: *const LibinputEvent,
) -> *mut libc::c_void {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        tablet.tool.cast()
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_x_transformed(
    event: *const LibinputEvent,
    width: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        let range = tablet.x_max - tablet.x_min + 1.0;
        if range > 0.0 {
            (tablet.x - tablet.x_min) * f64::from(width) / range
        } else {
            0.0
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_y_transformed(
    event: *const LibinputEvent,
    height: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::TabletTool(tablet) = &(*event).payload {
        let range = tablet.y_max - tablet.y_min + 1.0;
        if range > 0.0 {
            (tablet.y - tablet.y_min) * f64::from(height) / range
        } else {
            0.0
        }
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_x_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.x_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_y_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.y_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_pressure_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.pressure_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_distance_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.distance_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_tilt_x_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.tilt_x_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_tilt_y_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.tilt_y_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_rotation_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.rotation_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_slider_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.slider_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_wheel_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.wheel_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_size_major_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.size_major_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_size_minor_has_changed(
    event: *const LibinputEvent,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    match &(*event).payload {
        EventPayload::TabletTool(tablet) => tablet.size_minor_changed as libc::c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_plugin_system_append_default_paths(ctx: *mut LibinputContext) {
    if ctx.is_null() || (*ctx).plugins_loaded {
        return;
    }
    for path in ["/etc/libinput/plugins", "/usr/lib64/libinput/plugins"] {
        let path = std::ffi::CString::new(path).expect("static plugin path");
        if !(*ctx)
            .plugin_paths
            .iter()
            .any(|candidate| candidate.as_bytes() == path.as_bytes())
        {
            (*ctx).plugin_paths.push(path);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_plugin_system_append_path(
    ctx: *mut LibinputContext,
    path: *const libc::c_char,
) {
    if ctx.is_null() || path.is_null() || (*ctx).plugins_loaded {
        return;
    }
    let path = CStr::from_ptr(path);
    if path.to_bytes().is_empty()
        || (*ctx)
            .plugin_paths
            .iter()
            .any(|candidate| candidate.as_bytes() == path.to_bytes())
    {
        return;
    }
    if let Ok(path) = std::ffi::CString::new(path.to_bytes()) {
        (*ctx).plugin_paths.push(path);
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_plugin_system_load_plugins(
    ctx: *mut LibinputContext,
    flags: libc::c_uint,
) -> libc::c_int {
    if ctx.is_null() || flags != 0 {
        return -libc::EINVAL;
    }
    if (*ctx).plugins_loaded {
        return 0;
    }
    // Built-in stages are always active, but this build has no Lua plugin
    // loader. Match an upstream build configured without plugin support:
    // freeze the plugin system and report ENOSYS instead of claiming success.
    (*ctx).plugins_loaded = true;
    -libc::ENOSYS
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_button_is_toggle(
    group: *const libc::c_void,
    button: u32,
) -> libc::c_int {
    if group.is_null() || button >= u32::BITS {
        return 0;
    }
    ((*group.cast::<LibinputTabletPadModeGroup>()).toggle_button_mask & (1_u32 << button) != 0)
        as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_index(
    group: *const libc::c_void,
) -> u32 {
    if group.is_null() {
        return 0;
    }
    (*group.cast::<LibinputTabletPadModeGroup>()).index
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_mode(
    group: *const libc::c_void,
) -> u32 {
    if group.is_null() {
        return 0;
    }
    (*group.cast::<LibinputTabletPadModeGroup>()).current_mode
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_num_modes(
    group: *const libc::c_void,
) -> u32 {
    if group.is_null() {
        return 0;
    }
    (*group.cast::<LibinputTabletPadModeGroup>()).num_modes
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_button(
    group: *const libc::c_void,
    button: u32,
) -> libc::c_int {
    if group.is_null() {
        return 0;
    }
    if button >= u32::BITS {
        return 0;
    }
    ((*group.cast::<LibinputTabletPadModeGroup>()).button_mask & (1_u32 << button) != 0)
        as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_dial(
    group: *const libc::c_void,
    dial: u32,
) -> libc::c_int {
    if group.is_null() {
        return 0;
    }
    if dial >= u32::BITS {
        return 0;
    }
    ((*group.cast::<LibinputTabletPadModeGroup>()).dial_mask & (1_u32 << dial) != 0) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_ring(
    group: *const libc::c_void,
    ring: u32,
) -> libc::c_int {
    if group.is_null() {
        return 0;
    }
    if ring >= u32::BITS {
        return 0;
    }
    ((*group.cast::<LibinputTabletPadModeGroup>()).ring_mask & (1_u32 << ring) != 0) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_strip(
    group: *const libc::c_void,
    strip: u32,
) -> libc::c_int {
    if group.is_null() {
        return 0;
    }
    if strip >= u32::BITS {
        return 0;
    }
    ((*group.cast::<LibinputTabletPadModeGroup>()).strip_mask & (1_u32 << strip) != 0)
        as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_ref(
    group: *mut libc::c_void,
) -> *mut libc::c_void {
    if !group.is_null() {
        (*group.cast::<LibinputTabletPadModeGroup>())
            .refcount
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    group
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_unref(
    group: *mut libc::c_void,
) -> *mut libc::c_void {
    if group.is_null() {
        return std::ptr::null_mut();
    }
    let group = group.cast::<LibinputTabletPadModeGroup>();
    if (*group)
        .refcount
        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
        == 1
    {
        drop(Box::from_raw(group));
        std::ptr::null_mut()
    } else {
        group.cast()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_set_user_data(
    group: *mut libc::c_void,
    data: *mut libc::c_void,
) {
    if !group.is_null() {
        (*group.cast::<LibinputTabletPadModeGroup>()).user_data = data;
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_user_data(
    group: *const libc::c_void,
) -> *mut libc::c_void {
    if group.is_null() {
        return std::ptr::null_mut();
    }
    (*group.cast::<LibinputTabletPadModeGroup>()).user_data
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_is_available(
    tool: *const LibinputTabletTool,
) -> libc::c_int {
    (!tool.is_null() && (*tool).has_pressure) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_set(
    tool: *mut LibinputTabletTool,
    minimum: f64,
    maximum: f64,
) -> u32 {
    if tool.is_null() || !(*tool).has_pressure {
        return 1;
    }
    if minimum < 0.0 || maximum > 1.0 || minimum >= maximum {
        return 2;
    }
    (*tool).wanted_pressure_range_minimum = minimum;
    (*tool).wanted_pressure_range_maximum = maximum;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_get_minimum(
    tool: *const LibinputTabletTool,
) -> f64 {
    if tool.is_null() {
        0.0
    } else {
        (*tool).wanted_pressure_range_minimum
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_get_maximum(
    tool: *const LibinputTabletTool,
) -> f64 {
    if tool.is_null() {
        1.0
    } else {
        (*tool).wanted_pressure_range_maximum
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_get_default_minimum(
    _tool: *const libc::c_void,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_get_default_maximum(
    _tool: *const libc::c_void,
) -> f64 {
    1.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_modes(
    tool: *const libc::c_void,
) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() {
        0
    } else {
        (*tool).eraser_button_modes
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_set_mode(
    tool: *mut libc::c_void,
    mode: u32,
) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() || (mode != 0 && ((*tool).eraser_button_modes & mode) == 0) {
        return 1;
    }
    if !matches!(mode, 0 | 1) {
        return 2;
    }
    (*tool).wanted_eraser_button_mode = mode;
    if !(*tool).in_proximity {
        (*tool).eraser_button_mode = mode;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_mode(
    tool: *const libc::c_void,
) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() || (*tool).eraser_button_modes == 0 {
        0
    } else {
        (*tool).wanted_eraser_button_mode
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_default_mode(
    _tool: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_set_button(
    tool: *mut libc::c_void,
    button: u32,
) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() || (*tool).eraser_button_modes == 0 {
        return 1;
    }
    let is_button = matches!(button, 0x149 | 0x14b | 0x14c)
        || (0x100..0x140).contains(&button)
        || (0x150..=0x151).contains(&button)
        || (0x220..=0x223).contains(&button)
        || (0x2c0..=0x2e7).contains(&button);
    if !is_button {
        return 2;
    }
    (*tool).wanted_eraser_button = button;
    if !(*tool).in_proximity {
        (*tool).eraser_button = button;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_button(
    tool: *const libc::c_void,
) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() || (*tool).eraser_button_modes == 0 {
        0
    } else {
        (*tool).wanted_eraser_button
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_default_button(
    tool: *const libc::c_void,
) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() || (*tool).eraser_button_modes == 0 {
        0
    } else {
        (*tool).default_eraser_button
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_name(
    tool: *const libc::c_void,
) -> *const libc::c_char {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() {
        return std::ptr::null();
    }
    let tool = tool.cast::<LibinputTabletTool>();
    let tool = tool as *mut LibinputTabletTool;
    if let Some(name) = (*tool).name.as_ref() {
        return name.as_ptr();
    }
    if let Some(name) = crate::backend::tablet_tool_name_for_id((*tool).tool_id) {
        (*tool).name = Some(name);
        (*tool)
            .name
            .as_ref()
            .map_or(std::ptr::null(), |name| name.as_ptr())
    } else {
        std::ptr::null()
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_serial(tool: *const libc::c_void) -> u64 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() {
        0
    } else {
        (*tool).serial
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_tool_id(tool: *const libc::c_void) -> u64 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() {
        0
    } else {
        (*tool).tool_id
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_type(tool: *const libc::c_void) -> u32 {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() {
        0
    } else {
        (*tool).tool_type
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_distance(
    tool: *const libc::c_void,
) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_distance) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_button(
    tool: *const libc::c_void,
    button: u32,
) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).buttons.contains(&button)) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_size(tool: *const libc::c_void) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_size) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_is_unique(tool: *const libc::c_void) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).serial != 0) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_set_user_data(
    tool: *mut libc::c_void,
    data: *mut libc::c_void,
) {
    let tool = tool.cast::<LibinputTabletTool>();
    if !tool.is_null() {
        (*tool).user_data = data;
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_user_data(
    tool: *const libc::c_void,
) -> *mut libc::c_void {
    let tool = tool.cast::<LibinputTabletTool>();
    if tool.is_null() {
        std::ptr::null_mut()
    } else {
        (*tool).user_data
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_pressure(
    tool: *const libc::c_void,
) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_pressure) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_rotation(
    tool: *const libc::c_void,
) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_rotation) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_slider(tool: *const libc::c_void) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_slider) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_tilt(tool: *const libc::c_void) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_tilt) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_wheel(tool: *const libc::c_void) -> libc::c_int {
    let tool = tool.cast::<LibinputTabletTool>();
    (!tool.is_null() && (*tool).has_wheel) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_ref(tool: *mut libc::c_void) -> *mut libc::c_void {
    let tablet_tool = tool.cast::<LibinputTabletTool>();
    if !tablet_tool.is_null() {
        (*tablet_tool)
            .refcount
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    tool
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_unref(tool: *mut libc::c_void) -> *mut libc::c_void {
    let tablet_tool = tool.cast::<LibinputTabletTool>();
    if tablet_tool.is_null() {
        return std::ptr::null_mut();
    }
    if (*tablet_tool)
        .refcount
        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
        == 1
    {
        if !(*tablet_tool).device.is_null() {
            libinput_device_unref((*tablet_tool).device);
            (*tablet_tool).device = std::ptr::null_mut();
        }
        drop(Box::from_raw(tablet_tool));
        std::ptr::null_mut()
    } else {
        tool
    }
}

// ---------------------------------------------------------------------------
// Suspend / resume
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_suspend(ctx: *mut LibinputContext) {
    if ctx.is_null() {
        return;
    }
    let mut events = std::collections::VecDeque::new();
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.suspend(ctx, &mut events);
    }
    enqueue_events(ctx, events);
}

#[no_mangle]
pub unsafe extern "C" fn libinput_resume(ctx: *mut LibinputContext) -> libc::c_int {
    if ctx.is_null() {
        return -1;
    }
    let mut events = std::collections::VecDeque::new();
    let status = if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.resume(ctx, &mut events)
    } else {
        return -1;
    };
    enqueue_events(ctx, events);
    if !(*ctx).event_queue.is_empty() {
        (*ctx).signal_fd();
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn deny_open(
        _path: *const libc::c_char,
        _flags: libc::c_int,
        _user_data: *mut libc::c_void,
    ) -> libc::c_int {
        -libc::EACCES
    }

    unsafe extern "C" fn close_fd(_fd: libc::c_int, _user_data: *mut libc::c_void) {}

    static INTERFACE: LibinputInterface = LibinputInterface {
        open_restricted: Some(deny_open),
        close_restricted: Some(close_fd),
    };

    #[test]
    fn udev_context_requires_interface_and_udev() {
        unsafe {
            let fake_udev = 1usize as *mut libc::c_void;
            assert!(libinput_udev_create_context(
                std::ptr::null(),
                std::ptr::null_mut(),
                fake_udev,
            )
            .is_null());
            assert!(libinput_udev_create_context(
                &INTERFACE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
            .is_null());
        }
    }

    #[test]
    fn seat_assignment_is_udev_only_and_happens_once() {
        unsafe {
            let fake_udev = 1usize as *mut libc::c_void;
            let seat = std::ffi::CString::new("seat0").unwrap();
            let udev_ctx =
                libinput_udev_create_context(&INTERFACE, std::ptr::null_mut(), fake_udev);
            assert!(!udev_ctx.is_null());
            assert_eq!(libinput_udev_assign_seat(udev_ctx, seat.as_ptr()), 0);
            assert_eq!(libinput_udev_assign_seat(udev_ctx, seat.as_ptr()), -1);
            libinput_unref(udev_ctx);

            let path_ctx = libinput_path_create_context(&INTERFACE, std::ptr::null_mut());
            assert!(!path_ctx.is_null());
            assert_eq!(libinput_udev_assign_seat(path_ctx, seat.as_ptr()), -1);
            libinput_unref(path_ctx);
        }
    }

    #[test]
    fn overlong_seat_name_does_not_assign_or_queue_devices() {
        unsafe {
            let fake_udev = 1usize as *mut libc::c_void;
            let seat = std::ffi::CString::new("a".repeat(257)).unwrap();
            let ctx = libinput_udev_create_context(&INTERFACE, std::ptr::null_mut(), fake_udev);
            assert!(!ctx.is_null());

            assert_eq!(libinput_udev_assign_seat(ctx, seat.as_ptr()), -1);
            assert!(!(*ctx).seat_assigned);
            assert!((*ctx).event_queue.is_empty());

            libinput_unref(ctx);
        }
    }

    #[test]
    fn suspend_and_resume_are_null_safe() {
        unsafe {
            libinput_suspend(std::ptr::null_mut());
            assert_eq!(libinput_resume(std::ptr::null_mut()), -1);
        }
    }

    #[test]
    fn shared_device_groups_preserve_identity_and_user_data() {
        unsafe {
            let first = Box::new(LibinputDevice::new(
                "first",
                "/dev/input/event0",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));
            let group = first.group;
            let mut second = Box::new(LibinputDevice::new(
                "second",
                "/dev/input/event1",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));
            second.share_group(group);

            assert_eq!(second.group, group);
            assert_eq!(second.abi.group, group);
            assert_eq!(
                (*group).refcount.load(std::sync::atomic::Ordering::Relaxed),
                2
            );
            let marker = 0x5ausize as *mut libc::c_void;
            libinput_device_group_set_user_data(group.cast(), marker);
            assert_eq!(
                libinput_device_group_get_user_data(second.group.cast()),
                marker
            );

            drop(second);
            assert_eq!(
                (*group).refcount.load(std::sync::atomic::Ordering::Relaxed),
                1
            );
            drop(first);
        }
    }

    #[test]
    fn custom_acceleration_validates_and_copies_curves() {
        unsafe {
            assert!(libinput_config_accel_create(0).is_null());
            let config = libinput_config_accel_create(4);
            assert!(!config.is_null());

            let one_point = [1.0];
            assert_eq!(
                libinput_config_accel_set_points(config, 1, 1.0, 1, one_point.as_ptr()),
                2
            );
            let points = [0.0, 2.0, 6.0];
            assert_eq!(
                libinput_config_accel_set_points(config, 1, 1.0, points.len(), points.as_ptr()),
                0
            );
            let scroll_points = [0.0, 3.0, 9.0];
            assert_eq!(
                libinput_config_accel_set_points(
                    config,
                    2,
                    1.0,
                    scroll_points.len(),
                    scroll_points.as_ptr(),
                ),
                0
            );

            let mut device = Box::new(LibinputDevice::new(
                "pointer",
                "/dev/input/event0",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));
            device.accel_available = true;
            assert_eq!(libinput_device_config_accel_apply(&mut *device, config), 0);
            libinput_config_accel_destroy(config);

            let custom = device.accel_custom.as_mut().expect("custom config copied");
            let curve = custom.curve_mut(1).expect("motion curve copied");
            assert_eq!(curve.points, points);
            assert!((curve.factor(7.0, 0.0, 7_000) - 2.0).abs() < 1e-9);
            let scroll_curve = custom.curve_mut(2).expect("scroll curve copied");
            assert_eq!(scroll_curve.points, scroll_points);
            assert!((scroll_curve.factor(7.0, 0.0, 7_000) - 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn custom_acceleration_uses_fallback_and_extrapolates() {
        let mut config = crate::ffi_types::AccelConfig::new(4);
        config.fallback = Some(crate::ffi_types::AccelCurve::new(1.0, vec![0.0, 1.0]));
        let curve = config.curve_mut(1).expect("fallback curve");
        assert!((curve.factor(14.0, 0.0, 7_000) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plugin_paths_are_ordered_unique_and_frozen_after_load() {
        unsafe {
            let ctx = libinput_path_create_context(&INTERFACE, std::ptr::null_mut());
            let custom = std::ffi::CString::new("/tmp/plugins").unwrap();
            libinput_plugin_system_append_path(ctx, custom.as_ptr());
            libinput_plugin_system_append_path(ctx, custom.as_ptr());
            libinput_plugin_system_append_default_paths(ctx);
            assert_eq!((*ctx).plugin_paths.len(), 3);
            assert_eq!((&(*ctx).plugin_paths)[0].as_bytes(), b"/tmp/plugins");
            assert_eq!(libinput_plugin_system_load_plugins(ctx, 0), -libc::ENOSYS);
            let ignored = std::ffi::CString::new("/ignored").unwrap();
            libinput_plugin_system_append_path(ctx, ignored.as_ptr());
            assert_eq!((*ctx).plugin_paths.len(), 3);
            assert_eq!(libinput_plugin_system_load_plugins(ctx, 0), 0);
            libinput_unref(ctx);
        }
    }

    #[test]
    fn queued_events_retain_devices_until_event_destroy() {
        unsafe {
            let ctx = libinput_path_create_context(&INTERFACE, std::ptr::null_mut());
            let device = Box::into_raw(Box::new(LibinputDevice::new(
                "keyboard",
                "/dev/input/event0",
                std::ptr::null_mut(),
                ctx,
            )));
            enqueue_event(
                ctx,
                LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_KEYBOARD_KEY,
                    payload: EventPayload::KeyboardKey(crate::ffi_types::KeyboardKeyEvent {
                        time_usec: 1,
                        key: 30,
                        state: 1,
                        seat_key_count: 1,
                    }),
                    context: ctx,
                    device,
                },
            );
            assert_eq!(
                (*device).refcount.load(std::sync::atomic::Ordering::SeqCst),
                2
            );
            let event = libinput_get_event(ctx);
            assert_eq!(libinput_event_get_device(event), device);
            libinput_event_destroy(event);
            assert_eq!(
                (*device).refcount.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            assert!(libinput_device_unref(device).is_null());
            libinput_unref(ctx);
        }
    }

    #[test]
    fn ref_and_unref_return_the_upstream_object_lifecycle() {
        unsafe {
            let seat = Box::into_raw(Box::new(LibinputSeat {
                physical_name: std::ffi::CString::new("seat0").unwrap(),
                logical_name: std::ffi::CString::new("default").unwrap(),
                refcount: std::sync::atomic::AtomicI32::new(1),
                user_data: std::ptr::null_mut(),
                context: std::ptr::null_mut(),
                button_counts: std::sync::Mutex::new(crate::evtrans::empty_seat_code_counts()),
                key_counts: std::sync::Mutex::new(crate::evtrans::empty_seat_code_counts()),
            }));
            assert_eq!(libinput_seat_ref(seat.cast()), seat.cast());
            assert_eq!(libinput_seat_unref(seat.cast()), seat.cast());
            assert!(libinput_seat_unref(seat.cast()).is_null());

            let group = Box::into_raw(Box::new(LibinputTabletPadModeGroup {
                refcount: std::sync::atomic::AtomicI32::new(1),
                user_data: std::ptr::null_mut(),
                device: std::ptr::null_mut(),
                index: 0,
                num_modes: 1,
                current_mode: 0,
                button_mask: 0,
                dial_mask: 0,
                ring_mask: 0,
                strip_mask: 0,
                toggle_button_mask: 0,
                toggle_modes: Vec::new(),
            }));
            assert_eq!(
                libinput_tablet_pad_mode_group_ref(group.cast()),
                group.cast()
            );
            assert_eq!(
                libinput_tablet_pad_mode_group_unref(group.cast()),
                group.cast()
            );
            assert!(libinput_tablet_pad_mode_group_unref(group.cast()).is_null());
        }
    }

    #[test]
    fn devices_retain_their_seat_until_device_destruction() {
        unsafe {
            let seat = Box::into_raw(Box::new(LibinputSeat {
                physical_name: std::ffi::CString::new("seat0").unwrap(),
                logical_name: std::ffi::CString::new("default").unwrap(),
                refcount: std::sync::atomic::AtomicI32::new(1),
                user_data: std::ptr::null_mut(),
                context: std::ptr::null_mut(),
                button_counts: std::sync::Mutex::new(crate::evtrans::empty_seat_code_counts()),
                key_counts: std::sync::Mutex::new(crate::evtrans::empty_seat_code_counts()),
            }));
            let device = Box::into_raw(Box::new(LibinputDevice::new(
                "keyboard",
                "/dev/input/event0",
                seat,
                std::ptr::null_mut(),
            )));
            assert_eq!(
                (*seat).refcount.load(std::sync::atomic::Ordering::SeqCst),
                2
            );
            assert!(libinput_device_unref(device).is_null());
            assert_eq!(
                (*seat).refcount.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            assert!(libinput_seat_unref(seat.cast()).is_null());
        }
    }
}
