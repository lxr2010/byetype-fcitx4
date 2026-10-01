//! Fcitx4/fcitx5 状态管理（Linux 专用）。
//!
//! 通过 shell 调用 `fcitx-remote` / `fcitx5-remote` 来查询/停用/恢复输入法，
//! 避免引入 dbus 重依赖。
//!
//! 状态查询的输出约定（关键兼容点）：
//! - fcitx4：无参数时把状态数字（0/1/2）打印到 stdout，**同时以它作为退出码**；
//! - fcitx5：无参数时把状态数字（0/1/2）打印到 stdout，但**退出码恒为 0**。
//!
//! 因此状态判断必须解析 stdout，退出码只在 stdout 意外为空时兜底（fcitx4 语义）。
//!
//! 当 fcitx 未安装或未运行时，所有操作静默降级，不影响粘贴主流程。

use std::io::Read;
use std::process::{Command, Stdio};
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

/// 运行 `<bin> <args>`，带超时。成功结束时返回 `(退出码, stdout)`；
/// 超时或启动失败返回 None。stdout 被 pipe 捕获，不混入应用日志。
fn run_remote(bin: &str, args: &[&str]) -> Option<(i32, String)> {
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // spawn 时已 fork，无法对子进程本身设硬超时；用 try_wait 轮询。
    let deadline = std::time::Instant::now() + REMOTE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut out);
                }
                return Some((status.code().unwrap_or(-1), out));
            }
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

/// 把 0/1/2 状态数字解释为 FcitxState（0 = 未连接，1 = 未激活，2 = 激活）。
fn state_from_number(n: i32) -> FcitxState {
    match n {
        2 => FcitxState::Active,
        1 => FcitxState::Inactive,
        _ => FcitxState::NotInstalled,
    }
}

/// 按无参数查询的结果解析状态。
/// stdout 优先（fcitx4/fcitx5 都会打印状态数字）；stdout 意外为空时
/// 回退退出码（fcitx4 的退出码也携带状态，fcitx5 恒为 0 会落到 NotInstalled）。
fn state_from_query(result: Option<(i32, String)>) -> FcitxState {
    let Some((code, stdout)) = result else {
        return FcitxState::NotInstalled;
    };
    let first = stdout.split_whitespace().next().unwrap_or("");
    match first.parse::<i32>() {
        Ok(n) => state_from_number(n),
        Err(_) => state_from_number(code),
    }
}

/// 检测 fcitx 当前状态。
pub fn detect() -> FcitxState {
    let Some(bin) = resolve_remote_bin() else {
        return FcitxState::NotInstalled;
    };
    state_from_query(run_remote(bin, &[]))
}

/// 停用输入法（切到英文直出）。fcitx 未装时静默成功。
pub fn deactivate() -> Result<(), String> {
    let Some(bin) = resolve_remote_bin() else {
        return Ok(());
    };
    match run_remote(bin, &["-c"]) {
        Some(_) => Ok(()),
        None => Err("fcitx-remote -c timed out or failed".to_string()),
    }
}

/// 激活输入法（切回中文）。fcitx 未装时静默成功。
pub fn reactivate() -> Result<(), String> {
    let Some(bin) = resolve_remote_bin() else {
        return Ok(());
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
    fn stdout_carries_state_for_both_generations() {
        // fcitx5：状态在 stdout，退出码恒为 0
        assert_eq!(state_from_query(Some((0, "2\n".to_string()))), FcitxState::Active);
        assert_eq!(state_from_query(Some((0, "1\n".to_string()))), FcitxState::Inactive);
        assert_eq!(state_from_query(Some((0, "0\n".to_string()))), FcitxState::NotInstalled);
        // fcitx4：stdout 与退出码都携带状态
        assert_eq!(state_from_query(Some((2, "2\n".to_string()))), FcitxState::Active);
        assert_eq!(state_from_query(Some((1, "1\n".to_string()))), FcitxState::Inactive);
    }

    #[test]
    fn empty_stdout_falls_back_to_exit_code() {
        // 理论上不发生（两代都打印），兜底走 fcitx4 的退出码语义
        assert_eq!(state_from_query(Some((2, String::new()))), FcitxState::Active);
        assert_eq!(state_from_query(Some((1, String::new()))), FcitxState::Inactive);
        assert_eq!(state_from_query(Some((0, String::new()))), FcitxState::NotInstalled);
    }

    #[test]
    fn non_numeric_stdout_falls_back_to_exit_code() {
        assert_eq!(state_from_query(Some((2, "weird\n".to_string()))), FcitxState::Active);
    }

    #[test]
    fn timeout_or_launch_failure_means_not_installed() {
        assert_eq!(state_from_query(None), FcitxState::NotInstalled);
    }
}
