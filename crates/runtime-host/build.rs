use std::path::PathBuf;
use std::process::Command;
fn checked(command: &mut Command) {
    let deployment_target =
        std::env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "15.0".into());
    let status = command
        .env("MACOSX_DEPLOYMENT_TARGET", deployment_target)
        .status()
        .expect("native compiler invocation");
    assert!(status.success(), "native adapter build failed");
}
fn main() {
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    for file in [
        "native/trace.c",
        "native/launch.m",
        "native/launch_guard.h",
        "native/guard_tests.c",
        "native/platform_guard_tests.m",
        "native/ipc.c",
        "native/input_focus.cpp",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    checked(
        Command::new("/usr/bin/clang++")
            .args([
                "-std=c++17",
                "-fno-exceptions",
                "-fno-rtti",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-c",
                "native/input_focus.cpp",
                "-o",
            ])
            .arg(output.join("input_focus.o")),
    );
    checked(
        Command::new("/usr/bin/ar")
            .arg("rcs")
            .arg(output.join("libskinos_input_focus.a"))
            .arg(output.join("input_focus.o")),
    );
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=skinos_input_focus");
    checked(
        Command::new("/usr/bin/clang")
            .args([
                "-c",
                "-arch",
                "arm64",
                "-arch",
                "x86_64",
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "native/trace.c",
                "-o",
            ])
            .arg(output.join("trace.o")),
    );
    checked(
        Command::new("/usr/bin/clang")
            .args([
                "-c",
                "-arch",
                "arm64",
                "-arch",
                "x86_64",
                "-fobjc-arc",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-Wno-deprecated-declarations",
                "native/launch.m",
                "-o",
            ])
            .arg(output.join("launch.o")),
    );
    let mut link = Command::new("/usr/bin/clang");
    link.args(["-dynamiclib", "-arch", "arm64", "-arch", "x86_64"])
        .arg(output.join("trace.o"))
        .arg(output.join("launch.o"));
    if std::path::Path::new("native/ipc.c").is_file() {
        checked(
            Command::new("/usr/bin/clang")
                .args([
                    "-c",
                    "-arch",
                    "arm64",
                    "-arch",
                    "x86_64",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-Wno-deprecated-declarations",
                    "native/ipc.c",
                    "-o",
                ])
                .arg(output.join("ipc.o")),
        );
        link.arg(output.join("ipc.o"));
    }
    checked(
        link.args([
            "-Wl,-install_name,@rpath/libskinos_trace.dylib",
            "-framework",
            "AppKit",
            "-framework",
            "Foundation",
            "-framework",
            "IOKit",
            "-o",
        ])
        .arg(output.join("libskinos_trace.dylib")),
    );
    checked(
        Command::new("/usr/bin/clang")
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "native/guard_tests.c",
                "-o",
            ])
            .arg(output.join("launch-guard-test")),
    );
    checked(
        Command::new("/usr/bin/clang")
            .args([
                "-fobjc-arc",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-Wno-deprecated-declarations",
                "native/platform_guard_tests.m",
                "-framework",
                "AppKit",
                "-framework",
                "Foundation",
                "-o",
            ])
            .arg(output.join("platform-guard-test")),
    );
    println!(
        "cargo:rustc-env=SKINOS_TRACE_DYLIB={}",
        output.join("libskinos_trace.dylib").display()
    );
    println!(
        "cargo:rustc-env=SKINOS_LAUNCH_GUARD_TEST={}",
        output.join("launch-guard-test").display()
    );
    println!(
        "cargo:rustc-env=SKINOS_PLATFORM_GUARD_TEST={}",
        output.join("platform-guard-test").display()
    );
}
