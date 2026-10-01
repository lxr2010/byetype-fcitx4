//! Linux 录音音频切换（蓝牙 profile 自动模式 + 自定义 hook）。
//!
//! ## 自动模式（`bluetoothSwitch: "auto"`，默认）
//!
//! 蓝牙耳机在 A2DP profile 下只有输出没有麦克风；HFP/HSP profile 才有，
//! 但输出音质会降为电话音质。自动模式在录音前把蓝牙声卡切到录音
//! profile 并暂存原状态，录音结束后（含取消/出错路径与应用退出）恢复：
//!
//! 1. 发现：`pactl list cards` 找当前连接的 bluez 声卡；
//! 2. 暂存：当前 profile 与默认录音设备（存内存，会话作用域）；
//! 3. 切换：切到录音 profile（优先保音质的 `a2dp_sink_hfp_hf`，其次
//!    `handsfree_head_unit` / `headset_head_unit`）；
//! 4. 就绪：轮询 `pactl list sources` 直到录音 source 出现（上限 1.5s），
//!    然后临时把默认录音设备指向它 —— 应用用 system-default 录音，
//!    因为 cpal 走 ALSA 层、看不到 PipeWire 的蓝牙虚拟 source；
//! 5. 恢复：用暂存值还原默认录音设备与 profile（幂等）。
//!
//! macOS/Windows 由系统在应用打开录音流时自动切换蓝牙 profile，
//! 无需（也没有）对应机制；本模块是对 Linux 音频栈策略差异的补偿。
//!
//! 无蓝牙设备、pactl 缺失或切换超时都静默降级为"照常录音"，绝不阻断。
//!
//! ## 自定义 hook（`preRecordHook` / `postRecordHook`）
//!
//! 用户可配置任意 shell 命令在录音前后执行（自动模式之外的高级逃生舱），
//! 留空不执行。非 Linux 平台全部静默降级。

use std::sync::{Mutex, OnceLock};

/// HFP source 就绪轮询：间隔与总上限。
const READY_POLL_INTERVAL_MS: u64 = 50;
const READY_POLL_TIMEOUT_MS: u64 = 1500;

/// 一次自动切换的暂存快照。SESSION 为 None 表示当前未占用蓝牙 profile。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SwitchSession {
    card_name: String,
    prev_profile: String,
    prev_default_source: String,
}

fn session() -> &'static Mutex<Option<SwitchSession>> {
    static SESSION: OnceLock<Mutex<Option<SwitchSession>>> = OnceLock::new();
    SESSION.get_or_init(|| Mutex::new(None))
}

// ─────────────────────────────────────────────────────────────
// pactl 输出解析（纯函数，配单元测试）
// ─────────────────────────────────────────────────────────────

/// `pactl list cards` 中一张声卡的关键信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    /// 如 "bluez_card.88_92_CC_E7_A4_48"
    pub name: String,
    /// 当前激活的 profile 名
    pub active_profile: String,
    /// (profile 名, 是否含录音通道, 是否可用)
    pub profiles: Vec<(String, bool, bool)>,
}

impl CardInfo {
    pub fn is_bluetooth(&self) -> bool {
        self.name.starts_with("bluez_card.")
    }

    /// 当前 profile 是否已含录音通道（如上次异常退出停在 HFP）。
    pub fn active_profile_has_input(&self) -> bool {
        self.profiles
            .iter()
            .any(|(n, has_input, _)| *n == self.active_profile && *has_input)
    }

    /// 对应的录音 source 名前缀（bluez_card.X → bluez_source.X）。
    pub fn source_prefix(&self) -> String {
        format!(
            "bluez_source.{}",
            self.name.strip_prefix("bluez_card.").unwrap_or(&self.name)
        )
    }
}

/// 解析 `pactl list cards` 输出。
pub fn parse_cards(out: &str) -> Vec<CardInfo> {
    let mut cards = Vec::new();
    let mut current: Option<CardInfo> = None;
    let mut in_profiles = false;

    for line in out.lines() {
        if line.starts_with("Card #") {
            if let Some(c) = current.take() {
                cards.push(c);
            }
            current = Some(CardInfo {
                name: String::new(),
                active_profile: String::new(),
                profiles: Vec::new(),
            });
            in_profiles = false;
            continue;
        }
        let Some(card) = current.as_mut() else { continue };
        let t = line.trim_start();
        if let Some(name) = t.strip_prefix("Name:") {
            if !in_profiles {
                card.name = name.trim().to_string();
            }
        } else if let Some(p) = t.strip_prefix("Active Profile:") {
            card.active_profile = p.trim().to_string();
            in_profiles = false;
        } else if t == "Profiles:" {
            in_profiles = true;
        } else if t.starts_with("Ports:") || t.starts_with("Devices:") {
            in_profiles = false;
        } else if in_profiles && line.starts_with("\t\t") {
            if let Some(entry) = parse_profile_line(t) {
                card.profiles.push(entry);
            }
        }
    }
    if let Some(c) = current.take() {
        cards.push(c);
    }
    cards
}

/// 解析 Profiles 列表中的一行：
/// `name: 描述 (sinks: N, sources: N, priority: P, available: yes)`
///
/// profile 名本身可能含冒号（ALSA 的 `output:analog-stereo+input:analog-stereo`），
/// 因此以「冒号+空格」作为名字与描述的分界。
fn parse_profile_line(t: &str) -> Option<(String, bool, bool)> {
    let (name, rest) = t.split_once(": ")?;
    let sources = int_field(rest, "sources:").unwrap_or(0);
    let available = rest.contains("available: yes");
    Some((name.trim().to_string(), sources > 0, available))
}

/// 提取 "key: N" 形式的整数字段（容忍 N 后随逗号/括号）。
fn int_field(s: &str, key: &str) -> Option<i64> {
    let idx = s.find(key)?;
    let after = s[idx + key.len()..].trim_start();
    let digits: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// 从含录音通道的可用 profile 中选目标，优先保输出音质：
/// 1. `a2dp_sink_hfp_hf`（A2DP 输出 + HFP 麦克风，PipeWire 双向 profile）；
/// 2. `handsfree_head_unit` / `headset_head_unit`；
/// 3. 其他任何含录音通道的 profile。
pub fn pick_input_profile(profiles: &[(String, bool, bool)]) -> Option<String> {
    let usable: Vec<&String> = profiles
        .iter()
        .filter(|(_, has_input, available)| *has_input && *available)
        .map(|(name, _, _)| name)
        .collect();
    usable
        .iter()
        .find(|n| n.contains("a2dp") && n.contains("hfp"))
        .or_else(|| usable.iter().find(|n| n.contains("handsfree") || n.contains("headset")))
        .or_else(|| usable.first())
        .map(|s| (*s).clone())
}

/// 解析 `pactl list sources short` 输出，返回 source 名列表。
pub fn parse_source_names(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split('\t').nth(1))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ─────────────────────────────────────────────────────────────
// pactl 执行
// ─────────────────────────────────────────────────────────────

/// 运行 pactl，成功返回 stdout（trim 过）。pactl 缺失或执行失败返回 None。
fn run_pactl(args: &[&str]) -> Option<String> {
    std::process::Command::new("pactl")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

// ─────────────────────────────────────────────────────────────
// 自动模式主流程
// ─────────────────────────────────────────────────────────────

/// 录音开始前的自动切换入口（`bluetoothSwitch == "auto"` 时调用）。
///
/// 返回本次录音实际使用的蓝牙 source 名（仅日志/诊断用途）；任何一步
/// 不满足条件都返回 None 并保持系统原状，绝不阻断录音。
pub fn prepare_bluetooth_recording() -> Option<String> {
    #[cfg(not(target_os = "linux"))]
    return None;

    #[cfg(target_os = "linux")]
    {
        // 1. 找当前连接的蓝牙声卡
        let cards = parse_cards(&run_pactl(&["list", "cards"])?);
        let card = cards.into_iter().find(|c| c.is_bluetooth())?;

        // 2. 已在录音 profile（如上次未还原）→ 直接复用，不重复切换
        if card.active_profile_has_input() {
            let prefix = card.source_prefix();
            return run_pactl(&["list", "sources", "short"])
                .map(|out| parse_source_names(&out))
                .and_then(|names| names.into_iter().find(|n| n.starts_with(&prefix)));
        }

        // 3. 挑目标 profile
        let target = pick_input_profile(&card.profiles)?;

        // 4. 暂存 + 切换
        let snapshot = SwitchSession {
            prev_profile: card.active_profile.clone(),
            prev_default_source: run_pactl(&["get-default-source"]).unwrap_or_default(),
            card_name: card.name.clone(),
        };
        if !std::process::Command::new("pactl")
            .args(["set-card-profile", &card.name, &target])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            eprintln!("[audio_switch] set-card-profile to {} failed", target);
            return None;
        }

        // 5. 轮询等录音 source 就绪，出现后把默认录音设备临时指向它
        let prefix = card.source_prefix();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(READY_POLL_TIMEOUT_MS);
        loop {
            if let Some(out) = run_pactl(&["list", "sources", "short"]) {
                if let Some(source) = parse_source_names(&out)
                    .into_iter()
                    .find(|n| n.starts_with(&prefix))
                {
                    let _ = run_pactl(&["set-default-source", &source]);
                    *session().lock().unwrap() = Some(snapshot);
                    return Some(source);
                }
            }
            if std::time::Instant::now() > deadline {
                // 超时降级：还原 profile，按原状录音（宁可换麦克风，不阻断）
                eprintln!("[audio_switch] HFP source not ready in {}ms, rolling back", READY_POLL_TIMEOUT_MS);
                let _ = std::process::Command::new("pactl")
                    .args(["set-card-profile", &card.name, &snapshot.prev_profile])
                    .output();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(READY_POLL_INTERVAL_MS));
        }
    }
}

/// 恢复自动切换前的系统状态。幂等：无进行中的切换时是 no-op。
///
/// 恢复失败（如耳机已拔出）时保留会话，下次调用或应用退出时重试；
/// 声卡已消失则视为已恢复，直接清掉会话。
pub fn restore_session() {
    #[cfg(target_os = "linux")]
    {
        let mut guard = session().lock().unwrap();
        let Some(s) = guard.clone() else { return };

        // 声卡已不在（耳机拔出/断连）：PipeWire 自行清理，无需恢复
        let card_gone = run_pactl(&["list", "cards", "short"])
            .map(|out| !out.lines().any(|l| l.contains(&s.card_name)))
            .unwrap_or(true);
        if card_gone {
            *guard = None;
            return;
        }

        // 先还原默认录音设备再还原 profile，避免默认指向悬空的 HFP source
        let mut ok = true;
        if !s.prev_default_source.is_empty() {
            ok &= run_pactl(&["set-default-source", &s.prev_default_source]).is_some();
        }
        ok &= std::process::Command::new("pactl")
            .args(["set-card-profile", &s.card_name, &s.prev_profile])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if ok {
            *guard = None;
        } else {
            eprintln!("[audio_switch] restore failed, will retry on next finish/exit");
        }
    }
}

/// 录音结束的统一收尾：恢复自动切换 + 执行用户 post hook。
/// 所有录音结束路径（正常/手动停/取消/出错）都应调用这里。
pub fn finish_recording(hook: &str) {
    restore_session();
    post_record(hook);
}

// ─────────────────────────────────────────────────────────────
// 自定义 hook（逃生舱）
// ─────────────────────────────────────────────────────────────

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
    let result = std::process::Command::new("sh")
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

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS_SAMPLE: &str = "\
Card #43
\tName: alsa_card.pci-0000_00_1f.3
\tDriver: module-alsa-card.c
\tActive Profile: output:analog-stereo+input:analog-stereo
\tPorts:
\t\tanalog-input-internal-mic: Internal Microphone (type: Mic, priority: 8900, availability unknown)
\tProfiles:
\t\tinput:analog-stereo: Analog Stereo Input (sinks: 0, sources: 1, priority: 60, available: yes)
\t\toutput:analog-stereo+input:analog-stereo: Analog Stereo Duplex (sinks: 1, sources: 1, priority: 60, available: yes)
\t\toff: Off (sinks: 0, sources: 0, priority: 0, available: yes)

Card #45
\tName: bluez_card.88_92_CC_E7_A4_48
\tDriver: module-bluez5-device.c
\tActive Profile: a2dp_sink
\tProfiles:
\t\toff: Off (sinks: 0, sources: 0, priority: 0, available: yes)
\t\ta2dp_sink: High Fidelity Playback (A2DP Sink) (sinks: 1, sources: 0, priority: 40, available: yes)
\t\thandsfree_head_unit: Headset Head Unit (HSP/HFP) (sinks: 1, sources: 1, priority: 30, available: yes)
\t\ta2dp_sink_hfp_hf: High Fidelity Playback with Headset Mic (sinks: 1, sources: 1, priority: 25, available: yes)
\t\tunavailable_profile: Broken (sinks: 1, sources: 1, priority: 10, available: no)
";

    #[test]
    fn parse_cards_extracts_bluetooth_card() {
        let cards = parse_cards(CARDS_SAMPLE);
        assert_eq!(cards.len(), 2);

        let bt = cards.iter().find(|c| c.is_bluetooth()).unwrap();
        assert_eq!(bt.name, "bluez_card.88_92_CC_E7_A4_48");
        assert_eq!(bt.active_profile, "a2dp_sink");
        assert!(!bt.active_profile_has_input());
        assert_eq!(
            bt.source_prefix(),
            "bluez_source.88_92_CC_E7_A4_48"
        );

        let alsa = &cards[0];
        assert!(!alsa.is_bluetooth());
        assert!(alsa.active_profile_has_input());
    }

    #[test]
    fn parse_cards_handles_headset_active_profile() {
        // 上次异常退出停在 HFP：当前 profile 已含录音通道
        let out = CARDS_SAMPLE.replace("Active Profile: a2dp_sink", "Active Profile: handsfree_head_unit");
        let cards = parse_cards(&out);
        let bt = cards.iter().find(|c| c.is_bluetooth()).unwrap();
        assert!(bt.active_profile_has_input());
    }

    #[test]
    fn pick_profile_prefers_a2dp_hfp_over_headset() {
        let cards = parse_cards(CARDS_SAMPLE);
        let bt = cards.iter().find(|c| c.is_bluetooth()).unwrap();
        // 保音质的双向 profile 优先于纯 HFP
        assert_eq!(
            pick_input_profile(&bt.profiles).unwrap(),
            "a2dp_sink_hfp_hf"
        );
    }

    #[test]
    fn pick_profile_falls_back_to_headset_without_dual() {
        let profiles = vec![
            ("off".to_string(), false, true),
            ("a2dp_sink".to_string(), false, true),
            ("headset_head_unit".to_string(), true, true),
        ];
        assert_eq!(pick_input_profile(&profiles).unwrap(), "headset_head_unit");
    }

    #[test]
    fn pick_profile_skips_unavailable_and_requires_input() {
        let profiles = vec![
            ("a2dp_sink".to_string(), false, true),
            ("unavailable_profile".to_string(), true, false),
            ("handsfree_head_unit".to_string(), true, true),
        ];
        assert_eq!(pick_input_profile(&profiles).unwrap(), "handsfree_head_unit");

        let no_input: Vec<(String, bool, bool)> = vec![("a2dp_sink".to_string(), false, true)];
        assert_eq!(pick_input_profile(&no_input), None);
    }

    #[test]
    fn parse_sources_short() {
        let out = "88\tbluez_source.88_92_CC_E7_A4_48.handsfree_head_unit\tmodule-bluez5-device.c\ts16le 1ch 16000Hz\tRUNNING\n\
                   41\talsa_input.pci-0000_00_1f.3.analog-stereo\tmodule-alsa-card.c\ts16le 2ch 44100Hz\tSUSPENDED\n";
        assert_eq!(
            parse_source_names(out),
            vec![
                "bluez_source.88_92_CC_E7_A4_48.handsfree_head_unit".to_string(),
                "alsa_input.pci-0000_00_1f.3.analog-stereo".to_string(),
            ]
        );
    }

    #[test]
    fn int_field_parses_despite_trailing_punctuation() {
        assert_eq!(int_field("(sinks: 1, sources: 10, priority: 30)", "sources:"), Some(10));
        assert_eq!(int_field("(sinks: 1, sources: 0)", "sources:"), Some(0));
        assert_eq!(int_field("(sinks: 1)", "sources:"), None);
    }
}
