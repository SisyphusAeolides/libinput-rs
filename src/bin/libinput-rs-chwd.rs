use input::chwd_input::{detect_device, profile_inventory, scan_event_node, scan_system_facts};
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("libinput-rs-chwd: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("--list-profiles") if arguments.len() == 1 => {
            for profile in profile_inventory() {
                println!("{}\tpriority={}", profile.id, profile.priority);
            }
            Ok(())
        }
        Some("--identify") if arguments.len() == 2 => identify(Path::new(&arguments[1])),
        Some("--auto") if arguments.len() == 1 => auto(),
        Some("--help" | "-h") | None => {
            println!(
                "Usage: libinput-rs-chwd --auto | --list-profiles | --identify /dev/input/eventN"
            );
            Ok(())
        }
        _ => Err("invalid arguments (use --help)".to_string()),
    }
}

fn identify(path: &Path) -> Result<(), String> {
    let system = scan_system_facts();
    let device = scan_event_node(path).map_err(|error| format!("{}: {error}", path.display()))?;
    print_plan(&system, &device);
    Ok(())
}

fn auto() -> Result<(), String> {
    let system = scan_system_facts();
    let mut nodes = std::fs::read_dir("/dev/input")
        .map_err(|error| format!("cannot scan /dev/input: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.strip_prefix("event").is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                    })
                })
        })
        .collect::<Vec<PathBuf>>();
    nodes.sort();
    for path in nodes {
        match scan_event_node(&path) {
            Ok(device) => print_plan(&system, &device),
            Err(error) => eprintln!("{}: {error}", path.display()),
        }
    }
    Ok(())
}

fn print_plan(system: &input::chwd_input::SystemFacts, device: &input::chwd_input::ScannedDevice) {
    let result = detect_device(system, &device.facts(), true);
    println!(
        "{}\t{}\t{:?}\tprofile={}\tpriority={}\tconfidence={:.3}\tsource={:?}",
        device.path.display(),
        device.name,
        result.class,
        result.profile_id,
        result.priority,
        result.confidence,
        result.source
    );
}
