use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn run(command: &mut Command) {
    let status = command.status().expect("failed to start native build tool");
    assert!(status.success(), "native build tool failed");
}

fn main() {
    println!("cargo:rerun-if-changed=libinput.map");
    println!("cargo:rerun-if-changed=src/log_bridge.c");
    println!("cargo:rerun-if-changed=src/capforge.f90");
    println!("cargo:rerun-if-env-changed=FC");

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

    let fortran = env::var_os("FC").unwrap_or_else(|| "gfortran".into());
    let available = Command::new(&fortran)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let bindings = if available {
        let object = out_dir.join("capforge.o");
        let archive = out_dir.join("libcapforge.a");
        let mut compiler = Command::new(&fortran);
        compiler
            .args(["-c", "-fPIC", "-std=f2018", "-Wall", "-Wextra", "-Werror"])
            .arg("-J")
            .arg(&out_dir)
            .arg("src/capforge.f90")
            .arg("-o")
            .arg(&object);
        run(&mut compiler);
        let mut archiver = Command::new(env::var_os("AR").unwrap_or_else(|| "ar".into()));
        archiver.arg("crs").arg(&archive).arg(&object);
        run(&mut archiver);
        println!("cargo:rustc-link-lib=static=capforge");
        println!("cargo:rustc-link-lib=gfortran");
        r#"
extern "C" {
    fn cf_classify(
        ev: *const i64, nev: i32, key: *const i64, nkey: i32,
        rel: *const i64, nrel: i32, absolute: *const i64, nabs: i32,
        prop: *const i64, nprop: i32,
    ) -> i32;
    fn cf_parse_hex_words(input: *const u8, length: i32, output: *mut i64, words: i32);
    fn cf_knn_scores(
        features: *const f64, nfeatures: i32, centroids: *const f64,
        nprofiles: i32, scores: *mut f64,
    );
    fn cf_tiny_mlp_scores(
        features: *const f64, nfeatures: i32, input_weights: *const f64,
        hidden_bias: *const f64, nhidden: i32, output_weights: *const f64,
        output_bias: *const f64, nprofiles: i32, scores: *mut f64,
    );
}

pub(super) fn native_classify(bits: &CapabilityBits) -> Option<i32> {
    Some(unsafe {
        cf_classify(
            bits.ev.as_ptr(), bits.ev.len() as i32,
            bits.key.as_ptr(), bits.key.len() as i32,
            bits.rel.as_ptr(), bits.rel.len() as i32,
            bits.absolute.as_ptr(), bits.absolute.len() as i32,
            bits.properties.as_ptr(), bits.properties.len() as i32,
        )
    })
}

pub(super) fn native_parse(input: &str, output: &mut [i64]) -> bool {
    unsafe {
        cf_parse_hex_words(
            input.as_ptr(), input.len() as i32, output.as_mut_ptr(), output.len() as i32,
        );
    }
    true
}

pub(super) fn native_knn_scores(
    features: &[f64], centroids: &[f64], scores: &mut [f64],
) -> bool {
    if features.is_empty() || scores.is_empty() || centroids.len() != features.len() * scores.len() {
        return false;
    }
    unsafe {
        cf_knn_scores(
            features.as_ptr(), features.len() as i32, centroids.as_ptr(),
            scores.len() as i32, scores.as_mut_ptr(),
        );
    }
    true
}

pub(super) fn native_mlp_scores(
    features: &[f64], input_weights: &[f64], hidden_bias: &[f64],
    output_weights: &[f64], output_bias: &[f64], scores: &mut [f64],
) -> bool {
    if features.is_empty()
        || hidden_bias.is_empty()
        || scores.is_empty()
        || input_weights.len() != features.len() * hidden_bias.len()
        || output_weights.len() != hidden_bias.len() * scores.len()
        || output_bias.len() != scores.len()
    {
        return false;
    }
    unsafe {
        cf_tiny_mlp_scores(
            features.as_ptr(), features.len() as i32, input_weights.as_ptr(),
            hidden_bias.as_ptr(), hidden_bias.len() as i32, output_weights.as_ptr(),
            output_bias.as_ptr(), scores.len() as i32, scores.as_mut_ptr(),
        );
    }
    true
}

#[cfg(test)]
pub const NATIVE_CAPFORGE: bool = true;
"#
    } else {
        r#"
pub(super) fn native_classify(_bits: &CapabilityBits) -> Option<i32> {
    None
}

pub(super) fn native_parse(_input: &str, _output: &mut [i64]) -> bool {
    false
}

pub(super) fn native_knn_scores(
    _features: &[f64], _centroids: &[f64], _scores: &mut [f64],
) -> bool {
    false
}

pub(super) fn native_mlp_scores(
    _features: &[f64], _input_weights: &[f64], _hidden_bias: &[f64],
    _output_weights: &[f64], _output_bias: &[f64], _scores: &mut [f64],
) -> bool {
    false
}

#[cfg(test)]
pub const NATIVE_CAPFORGE: bool = false;
"#
    };
    fs::write(out_dir.join("capforge_bindings.rs"), bindings)
        .expect("failed to write capforge bindings");
}
