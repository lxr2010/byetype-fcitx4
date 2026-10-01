//! Fcitx4 状态管理（Linux 专用）。
//!
//! 通过 shell 调用 `fcitx-remote` 来查询/停用/恢复输入法，避免引入 dbus 重依赖。
//! 兼容 fcitx5（其 `fcitx5-remote` 参数与退出码语义一致）。
//! 当 fcitx 未安装或未运行时，所有操作静默降级，不影响粘贴主流程。

use std::process::Command;
use std::time::Duration;

/// Fcitx 当前状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FcitxState {
    /// 输入法激活（正在使用中文/拼音等）
    Active,
    /// 输入法未激活（英文直出）
    Inactive,
    /// fcitx-remote 未安装或 fcitx 守护进程未运行
    NotInstalled,
}

/// `fcitx-remote` / `fcitx5-remote` 调用超时，避免 fcitx 未运行时阻塞主流程。
const REMOTE_TIMEOUT: Duration = Duration::from_millis(500);

/// 找到可用的 remote 二进制名（优先 fcitx-remote，回退 fcitx5-remote）。
/// 返回 `None` 表示两者都不存在。
fn resolve_remote_bin() -> Option<&'static str> {
    ["fcitx-remote", "fcitx5-remote"]
        .into_iter()
        .find(|bin| which_exists(bin))
}

/// 轻量 `which`：不依赖外部 crate，只检查 PATH 中是否存在可执行文件。
fn which_exists(bin: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    std::env::split_paths(&path).any(|dir| {
        let full = dir.join(bin);
        full.is_file()
    })
}

/// 运行 `<bin> <args>`，带超时。返回退出码；超时或启动失败返回 None。
fn run_remote(bin: &str, args: &[&str]) -> Option<i32> {
    let child = Command::new(bin).args(args).spawn().ok()?;
    // spawn 时已 fork，无法对子进程本身设硬超时；用 try_wait 轮询。
    let deadline = std::time::Instant::now() + REMOTE_TIMEOUT;
    let mut child = child;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// 按退出码解释 fcitx 状态。
/// `fcitx-remote` 无参数退出码：2 = active，1 = inactive，0/其他/超时 = 未连接。
fn state_from_exit_code(code: Option<i32>) -> FcitxState {
    match code {
        Some(2) => FcitxState::Active,
        Some(1) => FcitxState::Inactive,
        _ => FcitxState::NotInstalled,
    }
}

/// 检测 fcitx 当前状态。
pub fn detect() -> FcitxState {
    let bin = match resolve_remote_bin() {
        Some(b) => b,
        None => return FcitxState::NotInstalled,
    };
    state_from_exit_code(run_remote(bin, &[]))
}

/// 停用输入法（切到英文直出）。fcitx 未装时静默成功。
pub fn deactivate() -> Result<(), String> {
    let bin = match resolve_remote_bin() {
        Some(b) => b,
        None => return Ok(()),
    };
    match run_remote(bin, &["-c"]) {
        Some(_) => Ok(()),
        None => Err("fcitx-remote -c timed out or failed".to_string()),
    }
}

/// 激活输入法（切回中文）。fcitx 未装时静默成功。
pub fn reactivate() -> Result<(), String> {
    let bin = match resolve_remote_bin() {
        Some(b) => b,
        None => return Ok(()),
    };
    match run_remote(bin, &["-o"]) {
        Some(_) => Ok(()),
        None => Err("fcitx-remote -o timed out or failed".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_2_means_active() {
        assert_eq!(state_from_exit_code(Some(2)), FcitxState::Active);
    }

    #[test]
    fn exit_code_1_means_inactive() {
        assert_eq!(state_from_exit_code(Some(1)), FcitxState::Inactive);
    }

    #[test]
    fn other_codes_and_timeout_mean_not_installed() {
        assert_eq!(state_from_exit_code(Some(0)), FcitxState::NotInstalled);
        assert_eq!(state_from_exit_code(Some(255)), FcitxState::NotInstalled);
        assert_eq!(state_from_exit_code(None), FcitxState::NotInstalled);
    }
}
