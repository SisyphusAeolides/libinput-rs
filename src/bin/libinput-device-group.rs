use input::udev_callout::DeviceHandle;
use std::env;
use std::path::Path;

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn main() {
    let arguments: Vec<_> = env::args_os().collect();
    if arguments.len() != 2 {
        std::process::exit(1);
    }
    let Some(handle) = DeviceHandle::from_syspath(Path::new(&arguments[1])) else {
        std::process::exit(1);
    };
    let Some((phys, product)) = handle.first_parent_value("phys") else {
        std::process::exit(1);
    };
    let phys = sanitize(&phys);
    let product = product.unwrap_or_else(|| "00/00/00/00".to_owned());
    let fields: Vec<_> = product.split('/').collect();
    let parsed: Option<Vec<_>> = fields
        .iter()
        .map(|field| u32::from_str_radix(field, 16).ok())
        .collect();
    let mut group = match parsed.as_deref() {
        Some([bus, vendor, product, _version]) => {
            format!("{bus:x}/{vendor:x}/{product:x}:{phys}")
        }
        _ => format!("{product}:{phys}"),
    };

    if let Some(index) = group.find("/input") {
        group.truncate(index);
    }
    if let (Some(dot), Some(dash)) = (group.rfind('.'), group.rfind('-')) {
        if dot > dash {
            group.truncate(dot);
        }
    }
    println!("LIBINPUT_DEVICE_GROUP={group}");
}
