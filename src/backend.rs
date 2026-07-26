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
    LibinputEvent, LibinputEventType, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};

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
    tracking_id: i32,
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
    is_keyboard: bool,
    is_pointer: bool,

    // --- relative / button ---
    remainder_x: f32,
    remainder_y: f32,

    // --- absolute / touchpad ---
    touch_active: bool,
    touch_fingers: u32,
    last_x: Option<i32>,
    last_y: Option<i32>,
    current_dx: i32,
    current_dy: i32,
    tap_emitted: bool,
    touch_start_time: Option<Instant>,
    last_movement_time: Option<Instant>,
    active_click_button: Option<u16>,

    // --- multi-touch slots (for pinch) ---
    mt_slots: Vec<MtSlot>,
    current_slot: usize,
    pinch_active: bool,
    pinch_base_dist: f64,
    pinch_base_angle: f64,
    pinch_fingers: i32,

    // --- keyboard repeat ---
    held_keys: Vec<HeldKey>,

    // --- DWT modifier state ---
    last_typing_time: Option<Instant>,

    // libinput device pointer (context owns the allocation)
    lib_device: *mut LibinputDevice,
}

unsafe impl Send for TrackedDevice {}

impl TrackedDevice {
    fn new(
        device: Device,
        restricted_fd: Option<RawFd>,
        path: PathBuf,
        lib_device: *mut LibinputDevice,
        uses_absolute_events: bool,
        is_keyboard: bool,
        is_pointer: bool,
    ) -> Self {
        let is_absolute = uses_absolute_events;

        // Pre-allocate 10 MT slots (covers every consumer touchpad)
        let mt_slots = vec![MtSlot::default(); 10];

        Self {
            device,
            restricted_fd,
            path,
            is_absolute,
            is_keyboard,
            is_pointer,
            remainder_x: 0.0,
            remainder_y: 0.0,
            touch_active: false,
            touch_fingers: 0,
            last_x: None,
            last_y: None,
            current_dx: 0,
            current_dy: 0,
            tap_emitted: false,
            touch_start_time: None,
            last_movement_time: None,
            active_click_button: None,
            mt_slots,
            current_slot: 0,
            pinch_active: false,
            pinch_base_dist: 0.0,
            pinch_base_angle: 0.0,
            pinch_fingers: 0,
            held_keys: Vec::new(),
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
}

// ---------------------------------------------------------------------------
// Helper: convert a SystemTime to microseconds since UNIX epoch
// ---------------------------------------------------------------------------

fn systime_to_usec(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_micros() as u64
}

// ---------------------------------------------------------------------------
// BackendState
// ---------------------------------------------------------------------------

pub struct BackendState {
    devices: HashMap<RawFd, TrackedDevice>,
    inotify: Option<Inotify>,
    pub global_typing_time: Option<Instant>,
    suspended: bool,
}

unsafe impl Send for BackendState {}

impl BackendState {
    pub fn new() -> Self {
        let inotify = Inotify::init(InitFlags::IN_NONBLOCK).ok().and_then(|ino| {
            ino.add_watch(
                "/dev/input",
                AddWatchFlags::IN_CREATE | AddWatchFlags::IN_ATTRIB,
            )
            .ok()?;
            Some(ino)
        });
        Self {
            devices: HashMap::new(),
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

        let props = device.properties();
        let keys = device.supported_keys();
        let rel = device.supported_relative_axes();
        let abs = device.supported_absolute_axes();
        let udev_device = crate::udev::UdevDevice::from_path(path);
        let udev_pointer = udev_device.as_ptr();
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
        let has_touch = tag_touchscreen
            || (use_evdev_fallback
                && !has_pen
                && !is_touchpad
                && has_touch_button
                && (has_abs_xy || has_mt_xy));
        let has_tablet = tag_tablet || (use_evdev_fallback && has_pen);
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
        let has_switch = tag_switch
            || (use_evdev_fallback
                && device
                    .supported_switches()
                    .is_some_and(|s| s.iter().next().is_some()));
        let has_relative_pointer = rel.is_some_and(|r| {
            r.contains(RelativeAxisCode::REL_X)
                || r.contains(RelativeAxisCode::REL_Y)
                || r.contains(RelativeAxisCode::REL_WHEEL)
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
        (*lib_dev).vendor_id = input_id.vendor() as u32;
        (*lib_dev).product_id = input_id.product() as u32;
        (*lib_dev).bus_type = input_id.bus_type().0 as u32;
        (*lib_dev).udev_device = udev_device.into_raw();
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
            let mut slots = None;
            for (axis, info) in absinfo {
                if axis == AbsoluteAxisCode::ABS_X || axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    if info.resolution() > 0 {
                        x = Some(
                            (info.maximum() - info.minimum()) as f64 / info.resolution() as f64,
                        );
                    }
                } else if axis == AbsoluteAxisCode::ABS_Y
                    || axis == AbsoluteAxisCode::ABS_MT_POSITION_Y
                {
                    if info.resolution() > 0 {
                        y = Some(
                            (info.maximum() - info.minimum()) as f64 / info.resolution() as f64,
                        );
                    }
                } else if axis == AbsoluteAxisCode::ABS_MT_SLOT {
                    slots = Some(info.maximum() - info.minimum() + 1);
                }
            }
            (*lib_dev).width_mm = x;
            (*lib_dev).height_mm = y;
            (*lib_dev).touch_count = if has_touch || is_touchpad {
                slots.unwrap_or(1)
            } else {
                0
            };
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
        let td = TrackedDevice::new(
            device,
            restricted_fd,
            path.to_path_buf(),
            lib_dev,
            is_touchpad || has_touch || has_tablet,
            is_keyboard,
            is_pointer,
        );
        self.devices.insert(fd, td);

        out.push(LibinputEvent {
            event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_ADDED,
            payload: EventPayload::DeviceAdded,
            context: ctx,
            device: lib_dev,
        });
    }

    // -----------------------------------------------------------------------
    // Main drain loop
    // -----------------------------------------------------------------------

    pub unsafe fn drain_into_queue(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        // --- hotplug ---
        self.handle_hotplug(ctx, out);

        // --- synthetic key-repeat before reading new events ---
        self.emit_key_repeats(ctx, out);

        // --- read events from every open device ---
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
            let cfg_tap = unsafe { &*lib_dev }.tap_enabled;
            let cfg_nat = unsafe { &*lib_dev }.natural_scroll;
            let cfg_dwt = unsafe { &*lib_dev }.dwt_enabled;
            let cfg_accel = unsafe { &*lib_dev }.accel_speed as f32 + 1.0;

            if unsafe { &*lib_dev }.send_events_mode == 1 {
                continue;
            }

            let global_typing = self.global_typing_time;

            for ev in &batch {
                let ts_usec = systime_to_usec(ev.timestamp());

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
                        ev, ts_usec, lib_dev, ctx, td, out, cfg_tap, cfg_nat, cfg_accel, dwt_active,
                    );
                }
            }
        }

        // Remove dead devices
        for fd in dead_fds {
            if let Some(td) = self.devices.remove(&fd) {
                (*ctx).unregister_fd(fd);
                let lib_device = td.close((*ctx).interface, (*ctx).user_data);
                out.push_back(LibinputEvent {
                    event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
                    payload: EventPayload::DeviceRemoved,
                    context: ctx,
                    device: lib_device,
                });
            }
        }
    }

    // -----------------------------------------------------------------------
    // Hotplug
    // -----------------------------------------------------------------------

    unsafe fn handle_hotplug(
        &mut self,
        ctx: *mut LibinputContext,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        if self.suspended || (*ctx).backend_kind != BackendKind::Udev {
            return;
        }
        let Some(ref ino) = self.inotify else { return };
        let Ok(ievents) = ino.read_events() else {
            return;
        };
        let mut new_paths: Vec<PathBuf> = Vec::new();
        for iev in ievents {
            if let Some(name) = iev.name {
                let p = PathBuf::from("/dev/input").join(&name);
                if !self.devices.values().any(|d| d.path == p) {
                    new_paths.push(p);
                }
            }
        }
        let mut tmp: Vec<LibinputEvent> = Vec::new();
        for p in new_paths {
            self.try_open(ctx, &p, &mut tmp);
        }
        for ev in tmp {
            out.push_back(ev);
        }
    }

    pub unsafe fn suspend(&mut self, ctx: *mut LibinputContext, out: &mut VecDeque<LibinputEvent>) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        for (fd, td) in self.devices.drain() {
            (*ctx).unregister_fd(fd);
            let lib_device = td.close((*ctx).interface, (*ctx).user_data);
            out.push_back(LibinputEvent {
                event_type: LibinputEventType::LIBINPUT_EVENT_DEVICE_REMOVED,
                payload: EventPayload::DeviceRemoved,
                context: ctx,
                device: lib_device,
            });
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
        let tracked = self
            .devices
            .remove(&fd)
            .expect("tracked device disappeared");
        (*ctx).unregister_fd(fd);
        let lib_device = tracked.close((*ctx).interface, (*ctx).user_data);
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

    pub unsafe fn resume(&mut self, ctx: *mut LibinputContext, out: &mut Vec<LibinputEvent>) {
        self.suspended = false;
        self.scan_and_open(ctx, out);
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

    unsafe fn process_relative_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
        ctx: *mut LibinputContext,
        _td: &mut TrackedDevice,
        out: &mut VecDeque<LibinputEvent>,
    ) {
        match ev.event_type() {
            EventType::RELATIVE => {
                let code = ev.code();
                let val = ev.value();
                if code == RelativeAxisCode::REL_X.0 {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION,
                        payload: EventPayload::PointerMotion(PointerMotionEvent {
                            time_usec: ts_usec,
                            dx: val as f64,
                            dy: 0.0,
                            dx_unaccel: val as f64,
                            dy_unaccel: 0.0,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                } else if code == RelativeAxisCode::REL_Y.0 {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_MOTION,
                        payload: EventPayload::PointerMotion(PointerMotionEvent {
                            time_usec: ts_usec,
                            dx: 0.0,
                            dy: val as f64,
                            dx_unaccel: 0.0,
                            dy_unaccel: val as f64,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                } else if code == RelativeAxisCode::REL_WHEEL.0 {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_WHEEL,
                        payload: EventPayload::PointerAxis(PointerAxisEvent {
                            time_usec: ts_usec,
                            axis: 0,
                            value: val as f64 * 15.0,
                            value_discrete: val,
                            source: 1,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                } else if code == RelativeAxisCode::REL_HWHEEL.0 {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_SCROLL_WHEEL,
                        payload: EventPayload::PointerAxis(PointerAxisEvent {
                            time_usec: ts_usec,
                            axis: 1, // LIBINPUT_POINTER_AXIS_SCROLL_HORIZONTAL
                            value: val as f64 * 15.0,
                            value_discrete: val,
                            source: 1,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                }
            }
            EventType::KEY => {
                let code = ev.code();
                if matches!(code,
                    c if c == KeyCode::BTN_LEFT.0
                      || c == KeyCode::BTN_RIGHT.0
                      || c == KeyCode::BTN_MIDDLE.0
                      || c == KeyCode::BTN_SIDE.0
                      || c == KeyCode::BTN_EXTRA.0
                ) {
                    out.push_back(LibinputEvent {
                        event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                        payload: EventPayload::PointerButton(PointerButtonEvent {
                            time_usec: ts_usec,
                            button: code as u32,
                            state: ev.value() as u32,
                        }),
                        context: ctx,
                        device: lib_dev,
                    });
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Absolute (touchpad) event processing
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    unsafe fn process_absolute_event(
        ev: &InputEvent,
        ts_usec: u64,
        lib_dev: *mut LibinputDevice,
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

                if code == KeyCode::BTN_TOUCH.0 {
                    td.touch_active = value != 0;
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
                                    out.push_back(LibinputEvent {
                                        event_type:
                                            LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                        payload: EventPayload::PointerButton(PointerButtonEvent {
                                            time_usec: ts_usec,
                                            button: KeyCode::BTN_LEFT.0 as u32,
                                            state: 1,
                                        }),
                                        context: ctx,
                                        device: lib_dev,
                                    });
                                    out.push_back(LibinputEvent {
                                        event_type:
                                            LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                                        payload: EventPayload::PointerButton(PointerButtonEvent {
                                            time_usec: ts_usec,
                                            button: KeyCode::BTN_LEFT.0 as u32,
                                            state: 0,
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
                    if code == KeyCode::BTN_LEFT.0 {
                        if value != 0 {
                            if td.touch_fingers == 2 {
                                mapped = KeyCode::BTN_RIGHT.0;
                            } else if td.touch_fingers >= 3 {
                                mapped = KeyCode::BTN_MIDDLE.0;
                            }
                            td.active_click_button = Some(mapped);
                        } else if let Some(active) = td.active_click_button {
                            mapped = active;
                            td.active_click_button = None;
                        }
                    }
                    if matches!(mapped,
                        c if c == KeyCode::BTN_LEFT.0
                          || c == KeyCode::BTN_RIGHT.0
                          || c == KeyCode::BTN_MIDDLE.0
                    ) {
                        out.push_back(LibinputEvent {
                            event_type: LibinputEventType::LIBINPUT_EVENT_POINTER_BUTTON,
                            payload: EventPayload::PointerButton(PointerButtonEvent {
                                time_usec: ts_usec,
                                button: mapped as u32,
                                state: value as u32,
                            }),
                            context: ctx,
                            device: lib_dev,
                        });
                    }
                }
            }

            EventType::ABSOLUTE => {
                let code = ev.code();
                let val = ev.value();

                // ---- MT slot tracking ----
                if code == AbsoluteAxisCode::ABS_MT_SLOT.0 {
                    let slot = val as usize;
                    if slot < td.mt_slots.len() {
                        td.current_slot = slot;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_TRACKING_ID.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        td.mt_slots[slot].active = val >= 0;
                        td.mt_slots[slot].tracking_id = val;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_X.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        td.mt_slots[slot].x = val as f64;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_Y.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        td.mt_slots[slot].y = val as f64;
                    }
                } else if code == AbsoluteAxisCode::ABS_MT_DISTANCE.0 {
                    let slot = td.current_slot;
                    if slot < td.mt_slots.len() {
                        td.mt_slots[slot].distance = val as f64;
                    }
                }
                // ---- Single-touch ABS_X/Y ----
                else if code == AbsoluteAxisCode::ABS_X.0 {
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
                if ev.code() != 0 {
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
