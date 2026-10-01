fn main() {
    tauri_build::build();

    // Linux/X11: main.rs 调用 XInitThreads() 修复 global-hotkey (x11rb) 与 GTK (libX11)
    // 的多线程 X 连接冲突。需要显式链接 libX11，否则链接器找不到 XInitThreads 符号。
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=X11");
}
