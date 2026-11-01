fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=ui/qdm.slint");
        slint_build::compile_with_config(
            "ui/qdm.slint",
            slint_build::CompilerConfiguration::new().with_style("fluent".into()),
        ).expect("The QDM Slint interface must compile");
    }
    println!("cargo:rerun-if-changed=resources/qdm.rc");
    println!("cargo:rerun-if-changed=resources/qdm.ico");
    println!("cargo:rerun-if-changed=resources/qdm.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || !cfg!(windows) { return; }
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("qdm.res");
    let status = std::process::Command::new("rc.exe")
        .args(["/nologo", "/I", "resources", "/fo"]).arg(&output)
        .arg("resources/qdm.rc").status().expect("Run this build from the Visual Studio Developer Command Prompt (rc.exe required)");
    assert!(status.success(), "Windows resource compiler failed");
    println!("cargo:rustc-link-arg={}", output.display());
}

