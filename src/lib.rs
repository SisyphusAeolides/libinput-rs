//! Experimental Rust implementation of the libinput.so ABI.
//!
//! The complete C ABI surface is exported, but behavioral compatibility is
//! still gated by the upstream test suite. Do not install this library over a
//! system libinput until every compatibility gate is complete.

#![allow(non_snake_case, clippy::missing_safety_doc)]

mod backend;
mod ffi_types;
mod udev;

use crate::ffi_types::{
    BackendKind, EventPayload, LibinputContext, LibinputDevice, LibinputEvent, LibinputEventType,
    LibinputInterface, LibinputSeat,
};

use std::ffi::CStr;
use std::os::unix::io::RawFd;

unsafe extern "C" {
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
    ctx_ref.event_queue.extend(tmp);
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
    let name = CStr::from_ptr(seat_name).to_string_lossy().into_owned();
    if let Ok(cname) = std::ffi::CString::new(name) {
        (*(*ctx).seat).physical_name = cname;
    }
    (*ctx).seat_assigned = true;
    let mut tmp: Vec<LibinputEvent> = Vec::new();
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.scan_and_open(ctx, &mut tmp);
    }
    for ev in tmp {
        (*ctx).event_queue.push_back(ev);
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
    let devnode = CStr::from_ptr(path).to_string_lossy().into_owned();
    let p = std::path::PathBuf::from(&devnode);
    let mut tmp: Vec<LibinputEvent> = Vec::new();
    let old_len = (*ctx).devices.len();
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.try_open(ctx, &p, &mut tmp);
    }
    for ev in tmp {
        (*ctx).event_queue.push_back(ev);
    }
    if (*ctx).devices.len() == old_len + 1 {
        (&(*ctx).devices)[old_len]
    } else {
        if (*ctx).log_priority <= 30 {
            if let Some(handler) = (*ctx).log_handler {
                input_emit_log(
                    handler as *mut libc::c_void,
                    ctx.cast(),
                    30,
                    c"failed to add device\n".as_ptr(),
                );
            }
        }
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
    let mut events = std::collections::VecDeque::new();
    let removed = if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.remove_device(ctx, dev, &mut events)
    } else {
        false
    };
    if removed {
        (*ctx).devices.retain(|candidate| *candidate != dev);
        (*ctx).event_queue.extend(events);
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
        drop(Box::from_raw(event));
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
        if e.state == 1 {
            1
        } else {
            0
        }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_axis_value(
    event: *const LibinputEvent,
    _axis: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerAxis(e) = &(*event).payload {
        e.value
    } else {
        0.0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_axis_value_discrete(
    event: *const LibinputEvent,
    _axis: u32,
) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    if let EventPayload::PointerAxis(e) = &(*event).payload {
        e.value_discrete as f64
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
    _axis: u32,
) -> libc::c_int {
    if event.is_null() {
        return 0;
    }
    matches!((*event).payload, EventPayload::PointerAxis(_)) as libc::c_int
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
        if e.state >= 1 {
            1
        } else {
            0
        }
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
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => e.slot,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_seat_slot(event: *const LibinputEvent) -> i32 {
    if event.is_null() {
        return -1;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => e.seat_slot,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_x(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => e.x,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_y(event: *const LibinputEvent) -> f64 {
    if event.is_null() {
        return 0.0;
    }
    match &(*event).payload {
        EventPayload::TouchDown(e) | EventPayload::TouchMotion(e) => e.y,
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_x_transformed(
    event: *const LibinputEvent,
    _width: u32,
) -> f64 {
    libinput_event_touch_get_x(event)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_touch_get_y_transformed(
    event: *const LibinputEvent,
    _height: u32,
) -> f64 {
    libinput_event_touch_get_y(event)
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
        | LibinputEventType::LIBINPUT_EVENT_GESTURE_PINCH_END => event,
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
        | EventPayload::GesturePinchEnd(e) => (e.time_usec / 1000) as u32,
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
        | EventPayload::GesturePinchEnd(e) => e.time_usec,
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
        | EventPayload::GesturePinchEnd(e) => e.finger_count,
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
    libinput_event_gesture_get_dx(event)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_gesture_get_dy_unaccelerated(
    event: *const LibinputEvent,
) -> f64 {
    libinput_event_gesture_get_dy(event)
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
        EventPayload::GestureSwipeEnd(e) | EventPayload::GesturePinchEnd(e) => {
            e.cancelled as libc::c_int
        }
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
    (*dev)
        .refcount
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    _dev: *const LibinputDevice,
) -> *const libc::c_char {
    std::ptr::null()
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
        return 0;
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
    if (*dev).has_touch || (*dev).has_pointer {
        3
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
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
    _dev: *const LibinputDevice,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_drag_enabled(
    dev: *mut LibinputDevice,
    _e: u32,
) -> u32 {
    if dev.is_null() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_drag_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_default_drag_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_set_drag_lock_enabled(
    dev: *mut LibinputDevice,
    _e: u32,
) -> u32 {
    if dev.is_null() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_tap_get_drag_lock_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
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
    if dev.is_null() || !(*dev).has_touch {
        return 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_3fg_drag_set_enabled(
    dev: *mut LibinputDevice,
    _enable: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_3fg_drag_get_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
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
    Box::into_raw(Box::new(profile)) as *mut libc::c_void
}

#[no_mangle]
pub unsafe extern "C" fn libinput_config_accel_destroy(accel_config: *mut libc::c_void) {
    if !accel_config.is_null() {
        drop(Box::from_raw(accel_config as *mut u32));
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_config_accel_set_points(
    accel_config: *mut libc::c_void,
    _accel_type: u32,
    step: f64,
    npoints: libc::size_t,
    points: *const f64,
) -> u32 {
    if accel_config.is_null() || points.is_null() || step <= 0.0 || npoints == 0 {
        return 2;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_apply(
    dev: *mut LibinputDevice,
    accel_config: *mut libc::c_void,
) -> u32 {
    if dev.is_null() || accel_config.is_null() {
        return 2;
    }
    (*dev).accel_profile = *(accel_config as *const u32);
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).has_pointer as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_set_speed(
    dev: *mut LibinputDevice,
    speed: f64,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    (*dev).accel_speed = speed.clamp(-1.0, 1.0);
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
    if (*dev).has_pointer {
        0b11
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
    (*dev).accel_profile = profile;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_profile(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).accel_profile
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_accel_get_default_profile(
    _dev: *const LibinputDevice,
) -> u32 {
    1
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
    if dev.is_null() {
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
    _dev: *const LibinputDevice,
) -> libc::c_int {
    0
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
    (*dev).has_pointer as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_left_handed_set(
    dev: *mut LibinputDevice,
    enabled: libc::c_int,
) -> u32 {
    if dev.is_null() {
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
    if (*dev).has_touch || (*dev).has_pointer {
        0b111
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_method(
    dev: *mut LibinputDevice,
    method: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
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
    _dev: *const LibinputDevice,
) -> u32 {
    2
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_button(
    dev: *mut LibinputDevice,
    _button: u32,
) -> u32 {
    if dev.is_null() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_button(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_set_button_lock(
    dev: *mut LibinputDevice,
    _state: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_button_lock(
    _dev: *const LibinputDevice,
) -> u32 {
    0
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
    if (*dev).has_pointer {
        0b11
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_set_method(
    dev: *mut LibinputDevice,
    method: u32,
) -> u32 {
    if dev.is_null() {
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
    _dev: *const LibinputDevice,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_set_clickfinger_button_map(
    dev: *mut LibinputDevice,
    _map: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_clickfinger_button_map(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_click_get_default_clickfinger_button_map(
    _dev: *const LibinputDevice,
) -> u32 {
    0
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
    (*dev).has_pointer as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    (*dev).middle_emulation = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_get_enabled(
    dev: *const LibinputDevice,
) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).middle_emulation as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_middle_emulation_get_default_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Device configuration — disable-while-typing
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_is_available(
    dev: *const LibinputDevice,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    ((*dev).has_pointer || (*dev).has_touch) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_set_enabled(
    dev: *mut LibinputDevice,
    enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    (*dev).dwt_enabled = enabled != 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_enabled(dev: *const LibinputDevice) -> u32 {
    if dev.is_null() {
        return 0;
    }
    (*dev).dwt_enabled as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_default_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_set_timeout(
    dev: *mut LibinputDevice,
    _millis: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_timeout(
    _dev: *const LibinputDevice,
) -> u32 {
    500
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwt_get_default_timeout(
    _dev: *const LibinputDevice,
) -> u32 {
    500
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_is_available(
    _dev: *const LibinputDevice,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_set_enabled(
    dev: *mut LibinputDevice,
    _enabled: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_default_enabled(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_set_timeout(
    dev: *mut LibinputDevice,
    _millis: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_timeout(
    _dev: *const LibinputDevice,
) -> u32 {
    500
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_dwtp_get_default_timeout(
    _dev: *const LibinputDevice,
) -> u32 {
    500
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
    (*dev).has_touch as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_calibration_set_matrix(
    dev: *mut LibinputDevice,
    matrix: *const f32,
) -> u32 {
    if dev.is_null() || matrix.is_null() {
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
    if dev.is_null() || matrix.is_null() {
        return 0;
    }
    std::slice::from_raw_parts_mut(matrix, 6).copy_from_slice(&(*dev).calibration);
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_calibration_get_default_matrix(
    _dev: *const LibinputDevice,
    matrix: *mut f32,
) -> libc::c_int {
    if matrix.is_null() {
        return 0;
    }
    std::slice::from_raw_parts_mut(matrix, 6).copy_from_slice(&[1.0_f32, 0.0, 0.0, 0.0, 1.0, 0.0]);
    1
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
    let name = CStr::from_ptr(name).to_string_lossy().into_owned();
    match std::ffi::CString::new(name) {
        Ok(name) => {
            (*(*dev).seat).logical_name = name;
            0
        }
        Err(_) => -1,
    }
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
    if !seat.is_null() {
        (*(seat as *mut LibinputSeat))
            .refcount
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    std::ptr::null_mut()
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
        0 => c"success".as_ptr(),
        1 => c"unsupported".as_ptr(),
        2 => c"invalid".as_ptr(),
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
    (*dev).has_touch as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_area_set_rectangle(
    dev: *mut LibinputDevice,
    _x1: f64,
    _y1: f64,
    _x2: f64,
    _y2: f64,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_area_get_rectangle(
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
    (*dev).has_touch as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_set_angle(
    dev: *mut LibinputDevice,
    _degrees_cw: u32,
) -> u32 {
    if dev.is_null() {
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_get_angle(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_rotation_get_default_angle(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_config_scroll_get_default_button(
    _dev: *const LibinputDevice,
) -> u32 {
    0
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
    (*dev).send_events_mode = if mode & 1 != 0 { 1 } else { mode };
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
    _dev: *const LibinputDevice,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_ref(group: *mut libc::c_void) -> *mut libc::c_void {
    group
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_unref(
    _group: *mut libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_set_user_data(
    _group: *mut libc::c_void,
    _data: *mut libc::c_void,
) {
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_group_get_user_data(
    _group: *const libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
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
    _key: u32,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).has_keyboard as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_led_update(dev: *mut LibinputDevice, _leds: u32) -> u32 {
    if dev.is_null() {
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_pointer_has_button(
    dev: *const LibinputDevice,
    _button: u32,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).has_pointer as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_switch_has_switch(
    dev: *const LibinputDevice,
    _sw: u32,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).has_switch as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_mode_group(
    _dev: *const LibinputDevice,
    _index: u32,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_buttons(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_dials(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_mode_groups(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_rings(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_get_num_strips(
    _dev: *const LibinputDevice,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_device_tablet_pad_has_key(
    dev: *const LibinputDevice,
    _code: u32,
) -> libc::c_int {
    if dev.is_null() {
        return 0;
    }
    (*dev).has_tablet as libc::c_int
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
        | LibinputEventType::LIBINPUT_EVENT_TABLET_PAD_STRIP => event,
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
    _width: u32,
) -> f64 {
    libinput_event_pointer_get_absolute_x(event)
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_pointer_get_absolute_y_transformed(
    event: *const LibinputEvent,
    _height: u32,
) -> f64 {
    libinput_event_pointer_get_absolute_y(event)
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
    libinput_event_pointer_get_axis_value(event, axis) * 120.0
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
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_button_state(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_key(_event: *const LibinputEvent) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_key_state(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_dial_delta_v120(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_dial_number(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_mode(_event: *const LibinputEvent) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_mode_group(
    _event: *const LibinputEvent,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_ring_number(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_ring_position(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_ring_source(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_strip_number(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_strip_position(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_strip_source(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_time_usec(
    _event: *const LibinputEvent,
) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_pad_get_time(event: *const LibinputEvent) -> u32 {
    (libinput_event_tablet_pad_get_time_usec(event) / 1000) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_button(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_button_state(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_seat_button_count(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_distance(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_dx(_event: *const LibinputEvent) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_dy(_event: *const LibinputEvent) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_pressure(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_x(_event: *const LibinputEvent) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_y(_event: *const LibinputEvent) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_proximity_state(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_rotation(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_slider_position(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_wheel_delta(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_wheel_delta_discrete(
    _event: *const LibinputEvent,
) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_size_major(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_size_minor(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tilt_x(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tilt_y(
    _event: *const LibinputEvent,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_time_usec(
    _event: *const LibinputEvent,
) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_time(event: *const LibinputEvent) -> u32 {
    (libinput_event_tablet_tool_get_time_usec(event) / 1000) as u32
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tip_state(
    _event: *const LibinputEvent,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_tool(
    _event: *const LibinputEvent,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_x_transformed(
    _event: *const LibinputEvent,
    _width: u32,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_get_y_transformed(
    _event: *const LibinputEvent,
    _height: u32,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_x_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_y_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_pressure_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_distance_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_tilt_x_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_tilt_y_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_rotation_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_slider_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_wheel_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_size_major_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_event_tablet_tool_size_minor_has_changed(
    _event: *const LibinputEvent,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_plugin_system_append_default_paths(_ctx: *mut LibinputContext) {}

#[no_mangle]
pub unsafe extern "C" fn libinput_plugin_system_append_path(
    _ctx: *mut LibinputContext,
    _path: *const libc::c_char,
) {
}

#[no_mangle]
pub unsafe extern "C" fn libinput_plugin_system_load_plugins(
    _ctx: *mut LibinputContext,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_button_is_toggle(
    _group: *const libc::c_void,
    _button: u32,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_index(
    _group: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_mode(
    _group: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_num_modes(
    _group: *const libc::c_void,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_button(
    _group: *const libc::c_void,
    _button: u32,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_dial(
    _group: *const libc::c_void,
    _dial: u32,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_ring(
    _group: *const libc::c_void,
    _ring: u32,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_has_strip(
    _group: *const libc::c_void,
    _strip: u32,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_ref(
    group: *mut libc::c_void,
) -> *mut libc::c_void {
    group
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_unref(
    _group: *mut libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_set_user_data(
    _group: *mut libc::c_void,
    _data: *mut libc::c_void,
) {
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_pad_mode_group_get_user_data(
    _group: *const libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_is_available(
    _tool: *const libc::c_void,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_set(
    _tool: *mut libc::c_void,
    _min: f64,
    _max: f64,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_get_minimum(
    _tool: *const libc::c_void,
) -> f64 {
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_pressure_range_get_maximum(
    _tool: *const libc::c_void,
) -> f64 {
    1.0
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
    _tool: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_set_mode(
    _tool: *mut libc::c_void,
    _mode: u32,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_mode(
    _tool: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_default_mode(
    _tool: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_set_button(
    _tool: *mut libc::c_void,
    _button: u32,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_button(
    _tool: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_config_eraser_button_get_default_button(
    _tool: *const libc::c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_name(
    _tool: *const libc::c_void,
) -> *const libc::c_char {
    std::ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_serial(_tool: *const libc::c_void) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_tool_id(_tool: *const libc::c_void) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_type(_tool: *const libc::c_void) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_distance(
    _tool: *const libc::c_void,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_button(
    _tool: *const libc::c_void,
    _button: u32,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_size(_tool: *const libc::c_void) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_is_unique(_tool: *const libc::c_void) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_set_user_data(
    _tool: *mut libc::c_void,
    _data: *mut libc::c_void,
) {
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_get_user_data(
    _tool: *const libc::c_void,
) -> *mut libc::c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_pressure(
    _tool: *const libc::c_void,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_rotation(
    _tool: *const libc::c_void,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_slider(
    _tool: *const libc::c_void,
) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_tilt(_tool: *const libc::c_void) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_has_wheel(_tool: *const libc::c_void) -> libc::c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_ref(tool: *mut libc::c_void) -> *mut libc::c_void {
    tool
}

#[no_mangle]
pub unsafe extern "C" fn libinput_tablet_tool_unref(_tool: *mut libc::c_void) -> *mut libc::c_void {
    std::ptr::null_mut()
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
    (*ctx).event_queue.extend(events);
}

#[no_mangle]
pub unsafe extern "C" fn libinput_resume(ctx: *mut LibinputContext) -> libc::c_int {
    if ctx.is_null() {
        return -1;
    }
    let mut events = Vec::new();
    if let Ok(mut backend) = (*ctx).backend.lock() {
        backend.resume(ctx, &mut events);
    } else {
        return -1;
    }
    (*ctx).event_queue.extend(events);
    0
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
    fn suspend_and_resume_are_null_safe() {
        unsafe {
            libinput_suspend(std::ptr::null_mut());
            assert_eq!(libinput_resume(std::ptr::null_mut()), -1);
        }
    }
}
