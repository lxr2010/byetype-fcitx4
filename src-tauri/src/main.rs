// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Linux/X11: global-hotkey (x11rb) 与 GTK (libX11) 共享 X 连接但走不同协议栈，
    // 必须在 GTK 打开 X 连接之前调用 XInitThreads()，否则多线程下 XCB 序列号会
    // 混乱，触发 `xcb_xlib_threads_sequence_lost` 断言崩溃。
    #[cfg(target_os = "linux")]
    unsafe {
        extern "C" {
            fn XInitThreads() -> std::ffi::c_int;
        }
        XInitThreads();
    }

    byetype_lib::run()
}
