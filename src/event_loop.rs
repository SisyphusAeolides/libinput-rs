use crate::config::InputConfig;
use crate::device::DeviceWrapper;
use crate::evdev::InputEvent;
use crate::virtual_device::VirtualDevice;
use log::{error, info, warn};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
use std::error::Error;
use std::os::unix::io::{AsFd, AsRawFd};
use std::path::PathBuf;
use std::time::Instant;

pub fn run(
    initial_devices: Vec<DeviceWrapper>,
    v_device: &mut VirtualDevice,
    config: &InputConfig,
) -> Result<(), Box<dyn Error>> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(128);
    let mut last_global_typing_time: Option<Instant> = None;

    let mut next_token = 0;
    let mut devices_map = std::collections::HashMap::new();

    for wrapper in initial_devices {
        let raw_fd = wrapper.device.as_raw_fd();
        let token = next_token;
        next_token += 1;
        poll.registry()
            .register(&mut SourceFd(&raw_fd), Token(token), Interest::READABLE)?;
        devices_map.insert(token, wrapper);
    }

    let inotify = Inotify::init(InitFlags::IN_NONBLOCK)?;
    inotify.add_watch(
        "/dev/input",
        AddWatchFlags::IN_CREATE | AddWatchFlags::IN_ATTRIB,
    )?;
    let inotify_fd = inotify.as_fd().as_raw_fd();
    let inotify_token = usize::MAX;
    poll.registry().register(
        &mut SourceFd(&inotify_fd),
        Token(inotify_token),
        Interest::READABLE,
    )?;

    loop {
        poll.poll(&mut events, None)?;
        for event in events.iter() {
            let token_id = event.token().0;

            if token_id == inotify_token {
                if let Ok(inotify_events) = inotify.read_events() {
                    for iev in inotify_events {
                        if let Some(name) = iev.name {
                            let path = PathBuf::from("/dev/input").join(&name);
                            let mut already_tracked = false;
                            for w in devices_map.values() {
                                if w.path == path {
                                    already_tracked = true;
                                    break;
                                }
                            }

                            if !already_tracked {
                                if let Some(wrapper) = crate::device::try_open_device(&path) {
                                    let raw_fd = wrapper.device.as_raw_fd();
                                    let token = next_token;
                                    next_token += 1;
                                    if poll
                                        .registry()
                                        .register(
                                            &mut SourceFd(&raw_fd),
                                            Token(token),
                                            Interest::READABLE,
                                        )
                                        .is_ok()
                                    {
                                        devices_map.insert(token, wrapper);
                                        info!("Successfully hotplugged device at {:?}", path);
                                    } else {
                                        warn!("Failed to register hotplugged device: {:?}", path);
                                    }
                                }
                            }
                        }
                    }
                }
                continue;
            }

            let mut device_disconnected = false;
            if let Some(wrapper) = devices_map.get_mut(&token_id) {
                let device_events = match wrapper.device.fetch_events() {
                    Ok(ev_batch) => Some(ev_batch.collect::<Vec<InputEvent>>()),
                    Err(e)
                        if e.raw_os_error() == Some(nix::libc::ENODEV)
                            || e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        info!("Device disconnected: {:?}", wrapper.path);
                        device_disconnected = true;
                        None
                    }
                    Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => {
                        error!("Error fetching events from {:?}: {}", wrapper.path, e);
                        None
                    }
                    Err(_) => None,
                };

                if let Some(evs) = device_events {
                    for ev in evs {
                        if let Err(e) =
                            wrapper.process_event(ev, v_device, config, last_global_typing_time)
                        {
                            return Err(format!(
                                "failed to forward events from {:?}: {e}",
                                wrapper.path
                            )
                            .into());
                        }
                        if wrapper.is_keyboard {
                            if let Some(typing_time) = wrapper.last_typing_time {
                                last_global_typing_time = Some(typing_time);
                            }
                        }
                    }
                }
            }

            if device_disconnected {
                devices_map.remove(&token_id);
            }
        }
    }
}
