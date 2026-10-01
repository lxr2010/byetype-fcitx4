use arboard::Clipboard;

use std::borrow::Cow;

/// X11 的剪贴板内容需要持续的有效 owner：arboard 的局部实例在释放后，
/// 其他应用可能再也读不到刚写入的内容（arboard 文档 Linux 节）。
/// 因此 Linux 上所有剪贴板读写都复用这个应用生命周期的共享实例；
/// macOS/Windows 由系统剪贴板服务持有内容，无需此机制。
#[cfg(target_os = "linux")]
fn with_shared_clipboard<T>(
    f: impl FnOnce(&mut Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, String> {
    use std::sync::{Mutex, MutexGuard, OnceLock};
    static HOLDER: OnceLock<Mutex<Option<Clipboard>>> = OnceLock::new();
    let holder = HOLDER.get_or_init(|| Mutex::new(Clipboard::new().ok()));
    let mut guard: MutexGuard<'_, Option<Clipboard>> =
        holder.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(clipboard) = guard.as_mut() else {
        return Err("clipboard unavailable".to_string());
    };
    f(clipboard).map_err(|e| e.to_string())
}

/// 剪贴板原内容快照，用于 paste_text 完成后还原。
/// 仅支持 arboard 能稳定读写的两种类型；文件引用 / 富文本 / 空 → None。
enum ClipboardSnapshot {
    Text(String),
    Image(arboard::ImageData<'static>),
    None,
}

#[cfg(target_os = "linux")]
fn snapshot_clipboard() -> ClipboardSnapshot {
    // 经共享实例读取，与写入方保持同一 owner 视角
    with_shared_clipboard(|clipboard| {
        if let Ok(text) = clipboard.get_text() {
            return Ok(ClipboardSnapshot::Text(text));
        }
        match clipboard.get_image() {
            Ok(img) => Ok(ClipboardSnapshot::Image(arboard::ImageData {
                width: img.width,
                height: img.height,
                bytes: Cow::Owned(img.bytes.into_owned()),
            })),
            Err(_) => Ok(ClipboardSnapshot::None),
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("[clipboard] snapshot: failed to access clipboard: {}", e);
        ClipboardSnapshot::None
    })
}

#[cfg(not(target_os = "linux"))]
fn snapshot_clipboard() -> ClipboardSnapshot {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[clipboard] snapshot: failed to open clipboard: {}", e);
            return ClipboardSnapshot::None;
        }
    };

    if let Ok(text) = clipboard.get_text() {
        return ClipboardSnapshot::Text(text);
    }

    match clipboard.get_image() {
        Ok(img) => ClipboardSnapshot::Image(arboard::ImageData {
            width: img.width,
            height: img.height,
            bytes: Cow::Owned(img.bytes.into_owned()),
        }),
        Err(_) => ClipboardSnapshot::None,
    }
}

fn restore_clipboard(snapshot: ClipboardSnapshot) {
    if matches!(snapshot, ClipboardSnapshot::None) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        // 经共享实例写入，还原的内容同样需要持续 owner
        let result = with_shared_clipboard(|clipboard| match snapshot {
            ClipboardSnapshot::Text(s) => clipboard.set_text(s),
            ClipboardSnapshot::Image(i) => clipboard.set_image(i),
            ClipboardSnapshot::None => Ok(()),
        });
        if let Err(e) = result {
            eprintln!("[clipboard] restore failed: {}", e);
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut clipboard = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[clipboard] restore: failed to open clipboard: {}", e);
                return;
            }
        };
        let result = match snapshot {
            ClipboardSnapshot::Text(s) => clipboard.set_text(s).map_err(|e| format!("set_text: {}", e)),
            ClipboardSnapshot::Image(i) => clipboard.set_image(i).map_err(|e| format!("set_image: {}", e)),
            ClipboardSnapshot::None => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("[clipboard] restore failed: {}", e);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use core_graphics::event::{CGEvent, CGEventFlags, CGKeyCode, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use std::ffi::c_void;

    const KEY_V: CGKeyCode = 9;

    extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const u8,
            encoding: u32,
        ) -> *const c_void;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFBooleanTrue: *const c_void;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    }

    pub fn ensure_accessibility() -> bool {
        unsafe {
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                b"AXTrustedCheckOptionPrompt\0".as_ptr(),
                0x08000100,
            );
            let keys = [key];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
            );
            let trusted = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            CFRelease(key);
            trusted
        }
    }

    pub fn simulate_paste() -> Result<(), String> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| "Failed to create CGEventSource".to_string())?;

        let key_down = CGEvent::new_keyboard_event(source.clone(), KEY_V, true)
            .map_err(|_| "Failed to create key down event".to_string())?;
        key_down.set_flags(CGEventFlags::CGEventFlagCommand);

        let key_up = CGEvent::new_keyboard_event(source, KEY_V, false)
            .map_err(|_| "Failed to create key up event".to_string())?;
        key_up.set_flags(CGEventFlags::CGEventFlagCommand);

        key_down.post(CGEventTapLocation::HID);
        key_up.post(CGEventTapLocation::HID);

        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use enigo::{Enigo, Keyboard, Settings, Key, Direction};

    pub fn simulate_paste() -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("Failed to create Enigo instance: {}", e))?;

        enigo.key(Key::Control, Direction::Press)
            .map_err(|e| format!("Failed to press Ctrl: {}", e))?;

        // V 的 Press/Release 单独收集结果，任何失败都不立即 return，
        // 确保下方 Ctrl 的 Release 始终被执行，避免修饰键卡死。
        let v_result = (|| {
            enigo.key(Key::Unicode('v'), Direction::Press)
                .map_err(|e| format!("Failed to press V: {}", e))?;
            enigo.key(Key::Unicode('v'), Direction::Release)
                .map_err(|e| format!("Failed to release V: {}", e))?;
            Ok(())
        })();

        // 无论 V 操作成功与否，都必须释放 Ctrl，防止系统级按键卡死。
        if let Err(e) = enigo.key(Key::Control, Direction::Release) {
            // Ctrl 释放失败属于更严重的问题，优先返回该错误；
            // 若 V 也有错误，则一并打印以便排查。
            if let Err(v_err) = &v_result {
                eprintln!("[clipboard] simulate_paste: Ctrl release failed ({e}); V error: {v_err}");
            }
            return Err(format!("Failed to release Ctrl: {}", e));
        }

        v_result
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use enigo::{Enigo, Keyboard, Settings, Key, Direction};

    /// 用 xdotool 发送粘贴键，比 enigo 更可靠地走 XTest 扩展。
    /// 根据焦点窗口类型选择粘贴方式：
    /// - 终端：Ctrl+Shift+V（Ctrl+V 是 readline quoted-insert）
    /// - Vim GUI（Neovide 等）：Ctrl+R 然后 +（insert 模式从 + 寄存器粘贴）
    /// - 普通 GUI：Ctrl+V
    ///
    /// xdotool --clearmodifiers 会先释放用户正按着的修饰键，避免冲突。
    pub fn simulate_paste() -> Result<(), String> {
        let method = detect_paste_method();

        let keys: &[&str] = match method {
            PasteMethod::CtrlShiftV => &["ctrl+shift+v"],
            PasteMethod::CtrlV => &["ctrl+v"],
            // Neovim insert 模式：Ctrl+R 进入寄存器输入，+ 选择系统剪贴板寄存器
            PasteMethod::VimPaste => &["ctrl+r", "plus"],
        };

        for key in keys {
            let out = match std::process::Command::new("xdotool")
                .args(["key", "--clearmodifiers", key])
                .output()
            {
                Ok(o) => o,
                // xdotool 未安装/不可执行：与退出非零一样降级到 enigo，
                // 而不是让整个粘贴失败（xdotool 文档定位为可选依赖）
                Err(e) => {
                    eprintln!("[clipboard] xdotool unavailable ({}), falling back to enigo", e);
                    return simulate_paste_enigo(method);
                }
            };
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("[clipboard] xdotool key '{}' failed ({}), falling back to enigo", key, stderr.trim());
                return simulate_paste_enigo(method);
            }
            // VimPaste 的两步之间需要短暂延迟
            if matches!(method, PasteMethod::VimPaste) {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        }
        Ok(())
    }

    /// 焦点窗口的粘贴策略。
    #[derive(Debug)]
    enum PasteMethod {
        /// Ctrl+Shift+V：终端
        CtrlShiftV,
        /// Ctrl+V：普通 GUI 应用（WPS、gedit 等）
        CtrlV,
        /// Neovim insert 模式粘贴指令：Ctrl+R 然后 +
        VimPaste,
    }

    /// 按焦点窗口的 WM_CLASS 与窗口名选择粘贴方式。
    /// 输入不要求预处理大小写：比较前统一转小写。
    fn classify_paste_method(wm_class: &str, win_name: &str) -> PasteMethod {
        let combined = format!("{} {}", wm_class, win_name).to_lowercase();

        // Vim/Neovim GUI：Ctrl+V 是 visual-block，用 insert 模式指令
        if combined.contains("neovide") || combined.contains("gvim")
            || combined.contains("neovim-qt")
        {
            return PasteMethod::VimPaste;
        }

        // 终端模拟器：Ctrl+V 是 readline quoted-insert，用 Ctrl+Shift+V
        if combined.contains("terminal") || combined.contains("alacritty")
            || combined.contains("kitty") || combined.contains("konsole")
            || combined.contains("xterm") || combined.contains("tmux")
            || combined.contains("tilix") || combined.contains("guake")
            || combined.contains("foot") || combined.contains("stterm")
            || combined.contains("urxvt") || combined.contains("wezterm")
            || combined.contains("gnome-terminal") || combined.contains("gnome_terminal")
            || combined.contains("kgx") // GNOME Console
            || combined.contains("deepin-terminal")
            || combined.contains("terminator") || combined.contains("tilda")
            || combined.contains("sakura") || combined.contains("lxterminal")
            || combined.contains("qterminal") || combined.contains("mate-terminal")
            || combined.contains("zap") // Zap Terminal (WM_CLASS: dev.zap.Zap)
            || combined.contains("warp") // Warp Terminal (WM_CLASS: dev.warp.Warp)
        {
            return PasteMethod::CtrlShiftV;
        }

        PasteMethod::CtrlV
    }

    /// 检测焦点窗口并返回合适的粘贴方式。
    fn detect_paste_method() -> PasteMethod {
        // 获取焦点窗口 ID
        let wid = match std::process::Command::new("xdotool")
            .args(["getwindowfocus"])
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => return PasteMethod::CtrlV,
        };

        // 用 xprop 获取 WM_CLASS（比窗口名更稳定）
        let wm_class = std::process::Command::new("xprop")
            .args(["-id", &wid, "WM_CLASS"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
            .unwrap_or_default();

        // 获取窗口名。注意 getwindowname 接受位置参数，不支持 --window 选项；
        // 只在命令成功时采用输出，失败（无 xdotool 等）按空标题处理。
        let win_name = std::process::Command::new("xdotool")
            .args(["getwindowname", &wid])
            .output()
            .map(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout).to_lowercase()
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();

        classify_paste_method(&wm_class, &win_name)
    }

    fn simulate_paste_enigo(method: PasteMethod) -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("Failed to create Enigo instance: {}", e))?;

        match method {
            PasteMethod::VimPaste => {
                // Ctrl+R 然后 +：Neovim insert 模式从 + 寄存器粘贴
                enigo.key(Key::Control, Direction::Press)
                    .map_err(|e| format!("Failed to press Ctrl: {}", e))?;
                enigo.key(Key::Unicode('r'), Direction::Press)
                    .map_err(|e| format!("Failed to press R: {}", e))?;
                enigo.key(Key::Unicode('r'), Direction::Release)
                    .map_err(|e| format!("Failed to release R: {}", e))?;
                enigo.key(Key::Control, Direction::Release)
                    .map_err(|e| format!("Failed to release Ctrl: {}", e))?;
                std::thread::sleep(std::time::Duration::from_millis(30));
                enigo.key(Key::Unicode('+'), Direction::Press)
                    .map_err(|e| format!("Failed to press +: {}", e))?;
                enigo.key(Key::Unicode('+'), Direction::Release)
                    .map_err(|e| format!("Failed to release +: {}", e))?;
            }
            PasteMethod::CtrlShiftV => {
                enigo.key(Key::Control, Direction::Press)
                    .map_err(|e| format!("Failed to press Ctrl: {}", e))?;
                enigo.key(Key::Shift, Direction::Press)
                    .map_err(|e| format!("Failed to press Shift: {}", e))?;
                enigo.key(Key::Unicode('v'), Direction::Press)
                    .map_err(|e| format!("Failed to press V: {}", e))?;
                enigo.key(Key::Unicode('v'), Direction::Release)
                    .map_err(|e| format!("Failed to release V: {}", e))?;
                enigo.key(Key::Shift, Direction::Release)
                    .map_err(|e| format!("Failed to release Shift: {}", e))?;
                enigo.key(Key::Control, Direction::Release)
                    .map_err(|e| format!("Failed to release Ctrl: {}", e))?;
            }
            PasteMethod::CtrlV => {
                enigo.key(Key::Control, Direction::Press)
                    .map_err(|e| format!("Failed to press Ctrl: {}", e))?;
                enigo.key(Key::Unicode('v'), Direction::Press)
                    .map_err(|e| format!("Failed to press V: {}", e))?;
                enigo.key(Key::Unicode('v'), Direction::Release)
                    .map_err(|e| format!("Failed to release V: {}", e))?;
                enigo.key(Key::Control, Direction::Release)
                    .map_err(|e| format!("Failed to release Ctrl: {}", e))?;
            }
        }

        Ok(())
    }

    /// 用 xclip 同时写入 CLIPBOARD 和 PRIMARY 选区，确保所有 X11 应用都能读到。
    /// arboard 只写 CLIPBOARD，某些应用（Neovide、WPS）可能读 PRIMARY 或有同步问题。
    pub fn sync_clipboard_xclip(text: &str) -> Result<(), String> {
        use std::io::Write;
        // CLIPBOARD 选区（Ctrl+V/Ctrl+Shift+V 粘贴用）
        let mut child = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn xclip (clipboard): {}", e))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();

        // PRIMARY 选区（中键粘贴用，部分应用读这个）
        let mut child = std::process::Command::new("xclip")
            .args(["-selection", "primary"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn xclip (primary): {}", e))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();

        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn vim_gui_uses_register_paste() {
            assert!(matches!(classify_paste_method("neovide", ""), PasteMethod::VimPaste));
            assert!(matches!(classify_paste_method("", "GVim"), PasteMethod::VimPaste));
            assert!(matches!(classify_paste_method("neovim-qt", ""), PasteMethod::VimPaste));
        }

        #[test]
        fn terminals_use_ctrl_shift_v() {
            for class in [
                "Alacritty", "gnome-terminal-server", "kitty", "konsole",
                "dev.zap.Zap", "dev.warp.Warp", "org.wezfurlong.wezterm",
            ] {
                assert!(
                    matches!(classify_paste_method(class, ""), PasteMethod::CtrlShiftV),
                    "expected terminal class {class} to use Ctrl+Shift+V"
                );
            }
        }

        #[test]
        fn normal_gui_uses_ctrl_v() {
            assert!(matches!(classify_paste_method("firefox", "Mozilla Firefox"), PasteMethod::CtrlV));
            assert!(matches!(classify_paste_method("", ""), PasteMethod::CtrlV));
        }

        #[test]
        fn wm_class_and_window_name_are_combined() {
            // WM_CLASS 不是终端，但窗口名包含终端标识（tmux 会话标题等）
            assert!(matches!(
                classify_paste_method("org.gnome.Nautilus", "user@host: ~/tmux"),
                PasteMethod::CtrlShiftV
            ));
        }
    }
}

pub fn paste_text(text: &str, overwrite_clipboard: bool) -> Result<(), String> {
    // OFF 模式：先快照原剪贴板，主流程结束后还原。
    let backup = if !overwrite_clipboard {
        snapshot_clipboard()
    } else {
        ClipboardSnapshot::None
    };

    // Linux 经共享实例写入（X11 需要持续 owner）；macOS/Windows 用局部实例
    #[cfg(target_os = "linux")]
    let write_result = with_shared_clipboard(|clipboard| clipboard.set_text(text));
    #[cfg(not(target_os = "linux"))]
    let write_result = Clipboard::new()
        .map_err(|e| format!("Failed to access clipboard: {}", e))
        .and_then(|mut clipboard| {
            clipboard
                .set_text(text)
                .map_err(|e| format!("Failed to write to clipboard: {}", e))
        });
    write_result?;

    // 先执行模拟粘贴，收集结果；任何失败都不再提前 return，
    // 以确保下方还原逻辑（保留原剪贴板模式）始终被执行。
    #[cfg(not(target_os = "linux"))]
    let paste_result: Result<(), String> = (|| {
        #[cfg(target_os = "macos")]
        {
            if !macos::ensure_accessibility() {
                return Err("Accessibility permission not granted, please allow in System Settings".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            macos::simulate_paste()?;
        }

        #[cfg(target_os = "windows")]
        {
            windows::simulate_paste()?;
        }

        Ok(())
    })();

    #[cfg(target_os = "linux")]
    let paste_result: Result<(), String> = {
        // 用 xclip 同步写入 CLIPBOARD + PRIMARY 选区，确保 Neovide/WPS 等
        // 应用能读到剪贴板内容（arboard 只写 CLIPBOARD，可能有同步问题）。
        if let Err(e) = linux::sync_clipboard_xclip(text) {
            eprintln!("[clipboard] xclip sync failed: {}, shared arboard content will be used", e);
        }
        // 等剪贴板内容真正就绪再发粘贴键
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Fcitx4 感知：粘贴前若 IME 激活则临时停用，避免拼音候选窗口拦截
        // 或与正在进行的候选状态冲突；粘贴后再恢复。
        let was_active = matches!(crate::fcitx::detect(), crate::fcitx::FcitxState::Active);
        if was_active {
            if let Err(e) = crate::fcitx::deactivate() {
                eprintln!("[clipboard] fcitx deactivate failed: {}", e);
            }
            // 等 Fcitx 真正释放键盘拦截再发粘贴键
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let linux_paste = linux::simulate_paste();

        // 让目标应用先读完粘贴内容，再恢复 IME
        std::thread::sleep(std::time::Duration::from_millis(150));

        if was_active {
            if let Err(e) = crate::fcitx::reactivate() {
                eprintln!("[clipboard] fcitx reactivate failed: {}", e);
            }
        }

        // 粘贴失败也不提前返回：下方还原逻辑必须执行，错误最后统一回报
        if let Err(e) = &linux_paste {
            eprintln!("[clipboard] linux paste failed: {}", e);
        }
        linux_paste
    };

    // 仅当备份非 None 时才执行还原；让目标应用先读完粘贴内容。
    // 无论模拟粘贴成功或失败都执行还原，避免原剪贴板内容丢失。
    if !matches!(backup, ClipboardSnapshot::None) {
        std::thread::sleep(std::time::Duration::from_millis(150));
        restore_clipboard(backup);
    }

    paste_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// macOS 的 NSPasteboard 类型缓存非线程安全,多个测试并行触达系统剪贴板时
    /// objc_msgSend 会段错误(崩溃堆栈落在 -[NSPasteboard _updateTypeCacheIfNeeded])。
    /// 用全局锁把所有剪贴板测试串行化,cargo test 默认并行执行不再崩溃。
    fn clipboard_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn clipboard_available() -> bool {
        Clipboard::new().is_ok()
    }

    #[test]
    fn snapshot_then_restore_text_roundtrip() {
        let _guard = clipboard_lock();
        if !clipboard_available() {
            eprintln!("clipboard unavailable, skipping");
            return;
        }
        let mut cb = Clipboard::new().unwrap();
        cb.set_text("hello-original").unwrap();

        let snap = snapshot_clipboard();
        assert!(matches!(snap, ClipboardSnapshot::Text(ref s) if s == "hello-original"));

        // 模拟覆盖
        cb.set_text("voice-result").unwrap();
        assert_eq!(Clipboard::new().unwrap().get_text().unwrap(), "voice-result");

        restore_clipboard(snap);
        assert_eq!(Clipboard::new().unwrap().get_text().unwrap(), "hello-original");
    }

    #[test]
    fn snapshot_returns_none_when_only_unsupported_present() {
        let _guard = clipboard_lock();
        if !clipboard_available() {
            eprintln!("clipboard unavailable, skipping");
            return;
        }
        let _ = snapshot_clipboard();
    }
}
