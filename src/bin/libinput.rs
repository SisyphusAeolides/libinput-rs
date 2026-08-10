use std::env;
use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const LIBINPUT_VERSION: &str = "1.31.3";
const SYSTEM_TOOL: &str = "/usr/libexec/libinput/libinput-tool";

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
    let remaining: Vec<OsString> = args.collect();

    if remaining.first().and_then(|value| value.to_str()) == Some("elan-recover") {
        return elan_recover(&remaining[1..]);
    }

    let helper = upstream_tool_path(&executable);
    if helper.is_file() {
        let error = Command::new(&helper).args(&remaining).exec();
        return Err(format!("failed to execute {}: {error}", helper.display()));
    }

    fallback_without_tools(&remaining)
}

fn upstream_tool_path(executable: &OsStr) -> PathBuf {
    let executable = Path::new(executable);
    if executable.is_absolute() {
        if let Some(prefix) = executable.parent().and_then(Path::parent) {
            let adjacent = prefix.join("libexec/libinput/libinput-tool");
            if adjacent.is_file() {
                return adjacent;
            }
        }
    }
    PathBuf::from(SYSTEM_TOOL)
}

fn fallback_without_tools(args: &[OsString]) -> Result<(), String> {
    match args.first().and_then(|value| value.to_str()) {
        Some("--version" | "-V") => {
            println!("{LIBINPUT_VERSION}");
            Ok(())
        }
        None | Some("--help" | "-h" | "help") => {
            print_help();
            Ok(())
        }
        Some(command) => Err(format!(
            "{command} is unavailable because the libinput utility payload is not installed"
        )),
    }
}

fn print_help() {
    println!(
        "Usage: libinput [--help|--version] <command> [<args>]\n\n\
         Global options:\n  --help ...... show this help and exit\n  \
         --version ... show version information and exit\n\n\
         Commands:\n  list-devices\n\tList all devices with their default configuration options\n\n  \
         debug-events\n\tPrint events to stdout\n\n  \
         debug-tablet\n\tPrint tablet tool events to stdout\n\n  \
         debug-tablet-pad\n\tPrint tablet pad events to stdout\n\n  \
         measure <feature>\n\tMeasure various device properties. See the man page for more info\n\n  \
         analyze <feature>\n\tAnalyze device events. See the man page for more info\n\n  \
         record\n\tRecord event stream from a device node. See the man page for more info\n\n  \
         replay\n\tReplay a previously recorded event stream. See the man page for more info"
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

#[cfg(test)]
mod tests {
    use super::upstream_tool_path;
    use std::ffi::OsStr;

    #[test]
    fn nonabsolute_invocation_uses_the_system_tool() {
        assert_eq!(
            upstream_tool_path(OsStr::new("libinput")),
            std::path::Path::new("/usr/libexec/libinput/libinput-tool")
        );
    }
}
