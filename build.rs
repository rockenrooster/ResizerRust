fn main() {
    slint_build::compile("ui/main_window.slint").unwrap();
    println!("cargo:rerun-if-changed=ui/main_window.slint");
    println!("cargo:rerun-if-changed=resizerrust.ico");

    set_windows_icon();
}

#[cfg(windows)]
fn set_windows_icon() {
    use std::env;

    let target_is_windows = env::var("CARGO_CFG_TARGET_OS")
        .map(|value| value == "windows")
        .unwrap_or(false);

    if !target_is_windows {
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon("resizerrust.ico");

    let pkg_name = env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "resizerrust".to_string());
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());

    res.set("FileDescription", &pkg_name);
    res.set("ProductName", &pkg_name);
    res.set("OriginalFilename", &format!("{pkg_name}.exe"));
    res.set("FileVersion", &pkg_version);
    res.set("ProductVersion", &pkg_version);

    res.compile().expect("Failed to compile Windows resources");
}

#[cfg(not(windows))]
fn set_windows_icon() {}
