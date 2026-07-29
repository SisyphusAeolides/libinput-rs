use evdev_upstream::{AbsoluteAxisCode, Device, KeyCode, RelativeAxisCode};
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

const LIBINPUT_VERSION: &str = "1.31.3";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("libinput: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os();
    let executable = args.next().unwrap_or_else(|| OsString::from("libinput"));
    let executable_name = Path::new(&executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("libinput");
    let remaining: Vec<OsString> = args.collect();

    if executable_name.ends_with("libinput-debug-events") {
        return debug_events(&remaining);
    }
    if executable_name.ends_with("libinput-list-devices") {
        return list_devices(&remaining);
    }

    let Some(command) = remaining.first().and_then(|value| value.to_str()) else {
        print_help();
        return Ok(());
    };
    match command {
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        "--version" | "-V" => {
            println!("libinput {LIBINPUT_VERSION} (libinput-rs)");
            Ok(())
        }
        "debug-events" => debug_events(&remaining[1..]),
        "list-devices" => list_devices(&remaining[1..]),
        "elan-recover" => elan_recover(&remaining[1..]),
        other => exec_compatibility_helper(other, &remaining[1..]),
    }
}

fn print_help() {
    println!(
        "Usage: libinput [--help|--version] <command> [<args>]\n\n\
         Commands:\n  list-devices  List input devices and capabilities\n  \
         debug-events  Print kernel input events\n  \
         elan-recover  Reinitialize a wedged ELAN I2C controller\n\n\
         Additional libinput utility commands are dispatched from /usr/libexec/libinput when installed."
    );
}

fn elan_recover(args: &[OsString]) -> Result<(), String> {
    let mut requested = Vec::new();
    let mut affected_only = false;
    let mut quiet = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--help" | "-h") => {
                println!(
                    "Usage: libinput elan-recover [--device I2C-ID] [--all] [--affected-only] [--quiet]"
                );
                return Ok(());
            }
            Some("--device") => {
                index += 1;
                let Some(id) = args.get(index).and_then(|value| value.to_str()) else {
                    return Err("--device requires an I2C identifier such as 5-0015".to_string());
                };
                requested.push(id.to_string());
            }
            Some("--all") => {}
            Some("--affected-only") => affected_only = true,
            Some("--quiet") => quiet = true,
            Some(option) => return Err(format!("unknown elan-recover option '{option}'")),
            None => return Err("elan-recover arguments must be valid UTF-8".to_string()),
        }
        index += 1;
    }

    let sysfs = Path::new("/sys");
    if affected_only && !input::elan_recover::affected_thinkpad_p53(sysfs) {
        return Ok(());
    }
    if unsafe { libc::geteuid() } != 0 {
        return Err("elan-recover must run as root (try: sudo libinput elan-recover)".to_string());
    }
    if requested.is_empty() {
        requested = input::elan_recover::discover(sysfs)
            .map_err(|error| format!("cannot discover ELAN I2C controllers: {error}"))?;
    }
    if requested.is_empty() {
        return Err("no devices are bound to the elan_i2c kernel driver".to_string());
    }

    for id in requested {
        let recovered = input::elan_recover::recover(sysfs, &id)
            .map_err(|error| format!("failed to recover ELAN {id}: {error}"))?;
        if !quiet {
            println!(
                "Recovered ELAN {} ({})",
                recovered.id,
                recovered.event_nodes.join(", ")
            );
        }
    }
    Ok(())
}

fn exec_compatibility_helper(command: &str, args: &[OsString]) -> Result<(), String> {
    if !command
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid command '{command}'"));
    }
    let helper = PathBuf::from(format!("/usr/libexec/libinput/libinput-{command}"));
    if !helper.is_file() {
        return Err(format!("{command} is not installed"));
    }
    let error = Command::new(helper).args(args).exec();
    Err(format!("failed to execute {command}: {error}"))
}

fn list_devices(args: &[OsString]) -> Result<(), String> {
    if args
        .iter()
        .any(|arg| matches!(arg.to_str(), Some("--help" | "-h")))
    {
        println!("Usage: libinput list-devices");
        return Ok(());
    }
    if !args.is_empty() {
        return Err("list-devices does not accept positional arguments".to_string());
    }

    for (path, _) in input::evdev::enumerate() {
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        print_device(&path, &device);
    }
    Ok(())
}

fn print_device(path: &Path, device: &Device) {
    let id = device.input_id();
    let keys = device.supported_keys();
    let relative = device.supported_relative_axes();
    let absolute = device.supported_absolute_axes();
    let keyboard = keys.is_some_and(|codes| codes.contains(KeyCode::KEY_A));
    let pointer = relative.is_some_and(|axes| {
        axes.contains(RelativeAxisCode::REL_X) || axes.contains(RelativeAxisCode::REL_Y)
    }) || keys.is_some_and(|codes| codes.contains(KeyCode::BTN_LEFT));
    let touch = absolute.is_some_and(|axes| {
        axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_X)
            && axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_Y)
    });
    let switches = device
        .supported_switches()
        .is_some_and(|codes| codes.iter().next().is_some());
    let mut capabilities = Vec::new();
    if keyboard {
        capabilities.push("keyboard");
    }
    if pointer {
        capabilities.push("pointer");
    }
    if touch {
        capabilities.push("touch");
    }
    if switches {
        capabilities.push("switch");
    }

    println!(
        "Device:                  {}",
        device.name().unwrap_or("Unknown")
    );
    println!("Kernel:                  {}", path.display());
    println!(
        "Id:                      {:04x}:{:04x}:{:04x}",
        id.bus_type().0,
        id.vendor(),
        id.product()
    );
    println!("Capabilities:            {}", capabilities.join(" "));
    println!();
}

struct DebugDevice {
    path: PathBuf,
    device: Device,
}

fn debug_events(args: &[OsString]) -> Result<(), String> {
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--help" | "-h") => {
                println!("Usage: libinput debug-events [--device /dev/input/eventX]");
                return Ok(());
            }
            Some("--device") => {
                index += 1;
                let Some(path) = args.get(index) else {
                    return Err("--device requires a path".to_string());
                };
                paths.push(PathBuf::from(path));
            }
            Some("--verbose" | "--show-keycodes" | "--quiet") => {}
            Some(option) => return Err(format!("unknown debug-events option '{option}'")),
            None => return Err("debug-events arguments must be valid UTF-8".to_string()),
        }
        index += 1;
    }

    let explicit = !paths.is_empty();
    if !explicit {
        paths.extend(input::evdev::enumerate().map(|(path, _)| path));
    }
    let mut devices = Vec::new();
    let mut opened = HashSet::new();
    open_debug_devices(paths, &mut devices, &mut opened)?;
    if devices.is_empty() {
        return Err("no readable input devices".to_string());
    }

    loop {
        let mut saw_event = false;
        devices.retain_mut(|tracked| match tracked.device.fetch_events() {
            Ok(events) => {
                for event in events {
                    saw_event = true;
                    println!(
                        "{} type={} code={} value={}",
                        tracked.path.display(),
                        event.event_type().0,
                        event.code(),
                        event.value()
                    );
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
            Err(error) if error.raw_os_error() == Some(libc::ENODEV) => {
                opened.remove(&tracked.path);
                false
            }
            Err(error) => {
                eprintln!("{}: {error}", tracked.path.display());
                true
            }
        });
        if !explicit {
            let paths: Vec<PathBuf> = input::evdev::enumerate()
                .map(|(path, _)| path)
                .filter(|path| !opened.contains(path))
                .collect();
            open_debug_devices(paths, &mut devices, &mut opened)?;
        }
        if !saw_event {
            thread::sleep(Duration::from_millis(4));
        } else {
            io::stdout()
                .flush()
                .map_err(|error| format!("failed to flush event output: {error}"))?;
        }
    }
}

fn open_debug_devices(
    paths: impl IntoIterator<Item = PathBuf>,
    devices: &mut Vec<DebugDevice>,
    opened: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    for path in paths {
        if opened.contains(&path) {
            continue;
        }
        match Device::open(&path) {
            Ok(device) => {
                device
                    .set_nonblocking(true)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                println!(
                    "{} DEVICE_ADDED {}",
                    path.display(),
                    device.name().unwrap_or("Unknown")
                );
                opened.insert(path.clone());
                devices.push(DebugDevice { path, device });
            }
            Err(error) if !explicit_permission_error(&error) => {
                eprintln!("{}: {error}", path.display());
            }
            Err(_) => {}
        }
    }
    Ok(())
}

fn explicit_permission_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::explicit_permission_error;
    use std::io;

    #[test]
    fn expected_discovery_errors_remain_quiet() {
        assert!(explicit_permission_error(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(explicit_permission_error(&io::Error::from(
            io::ErrorKind::NotFound
        )));
        assert!(!explicit_permission_error(&io::Error::from(
            io::ErrorKind::InvalidData
        )));
    }
}
