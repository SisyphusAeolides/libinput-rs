use std::env;
use std::path::PathBuf;
use std::process::Command;

fn run(command: &mut Command) {
    let status = command.status().expect("failed to start native build tool");
    assert!(status.success(), "native build tool failed");
}

fn main() {
    println!("cargo:rerun-if-changed=libinput.map");
    println!("cargo:rerun-if-changed=src/log_bridge.c");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let object = out_dir.join("log_bridge.o");
    let archive = out_dir.join("libinput_log_bridge.a");

    let mut compiler = Command::new(env::var_os("CC").unwrap_or_else(|| "cc".into()));
    compiler.args([
        "-c",
        "-fPIC",
        "-fvisibility=hidden",
        "-Wall",
        "-Wextra",
        "-Werror",
        "src/log_bridge.c",
        "-o",
    ]);
    compiler.arg(&object);
    run(&mut compiler);

    let mut archiver = Command::new(env::var_os("AR").unwrap_or_else(|| "ar".into()));
    archiver.arg("crs").arg(&archive).arg(&object);
    run(&mut archiver);

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=input_log_bridge");
}
