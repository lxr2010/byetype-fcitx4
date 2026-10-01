//! Linux 录音前后 hook 执行器。
//!
//! 用户可在配置里设置 `preRecordHook` 和 `postRecordHook`（shell 命令），
//! 在录音开始前和结束后执行。典型用途是切换蓝牙耳机的音频 profile。
//!
//! 例如，在 config.json 的 advanced 里设置：
//! ```json
//! "preRecordHook": "pactl set-card-profile bluez_card.88_92_CC_E7_A4_48 handsfree_head_unit && pactl set-default-source bluez_source.88_92_CC_E7_A4_48.handsfree_head_unit",
//! "postRecordHook": "pactl set-default-source alsa_input.pci-0000_00_1f.3.analog-stereo && pactl set-card-profile bluez_card.88_92_CC_E7_A4_48 a2dp_sink"
//! ```
//!
//! 留空则不执行任何命令。非 Linux 平台静默降级。

use std::process::Command;

/// 录音开始前执行 hook 命令。
/// 命令通过 `sh -c` 执行，支持管道、&& 等 shell 语法。
/// 留空或非 Linux 时静默成功。
pub fn pre_record(hook: &str) {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = hook;
        return;
    }

    #[cfg(target_os = "linux")]
    {
        if hook.is_empty() {
            return;
        }
        run_hook("pre_record", hook);
    }
}

/// 录音结束后执行 hook 命令。
/// 留空或非 Linux 时静默成功。
pub fn post_record(hook: &str) {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = hook;
        return;
    }

    #[cfg(target_os = "linux")]
    {
        if hook.is_empty() {
            return;
        }
        run_hook("post_record", hook);
    }
}

#[cfg(target_os = "linux")]
fn run_hook(name: &str, hook: &str) {
    let result = Command::new("sh")
        .arg("-c")
        .arg(hook)
        .output();

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("[audio_switch] {} hook succeeded", name);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            eprintln!(
                "[audio_switch] {} hook failed (exit={}): stderr={}",
                name,
                output.status,
                stderr.trim()
            );
            if !stdout.trim().is_empty() {
                eprintln!("[audio_switch] {} hook stdout: {}", name, stdout.trim());
            }
        }
        Err(e) => {
            eprintln!("[audio_switch] {} hook failed to launch: {}", name, e);
        }
    }
}
