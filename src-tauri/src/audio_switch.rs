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
//! 系统状态首次改变（切 profile）后立即落快照，此后任何失败路径都走
//! 统一的幂等恢复 `restore_session()`，不依赖调用方记得清理。
//!
//! 兼容性要点：
//! - pactl 输出会被本地化（中文桌面输出「卡」「名称」等字段），因此所有
//!   pactl 子进程固定 `LC_ALL=C` 并清理 `LANGUAGE`；
//! - 蓝牙录音 source 的命名两代音频服务不同：PulseAudio 为
//!   `bluez_source.<addr>.…`，PipeWire/WirePlumber 为 `bluez_input.<addr>.<n>`，
//!   匹配时两者都接受并排除输出 monitor。
//!
//! macOS/Windows 由系统在应用打开录音流时自动切换蓝牙 profile，
//! 无需（也没有）对应机制；本模块是对 Linux 音频栈策略差异的补偿。
//!
//! 无蓝牙设备、pactl 缺失、用户明确选择了非蓝牙麦克风或切换超时，
//! 都静默降级为"照常录音"，绝不阻断。
//!
//! ## 自定义 hook（`preRecordHook` / `postRecordHook`）
//!
//! 用户可配置任意 shell 命令在录音前后执行（自动模式之外的高级逃生舱），
//! 留空不执行，执行有 5 秒上限。非 Linux 平台全部静默降级。

use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// HFP source 就绪轮询：间隔与总上限。
const READY_POLL_INTERVAL_MS: u64 = 50;
const READY_POLL_TIMEOUT_MS: u64 = 1500;
/// 单条 pactl 命令的执行上限，防止音频服务无响应时阻塞快捷键线程。
const PACTL_TIMEOUT: Duration = Duration::from_secs(2);
/// 自定义 hook 的执行上限。
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

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
}

/// 判断 source 是否属于该蓝牙声卡的录音输入。
///
/// 命名两代音频服务不同：PulseAudio 为 `bluez_source.<addr>.<profile>`，
/// PipeWire/WirePlumber 为 `bluez_input.<addr>.<n>`。输出 monitor
/// （`*.monitor`）不是录音设备，排除。
fn source_belongs_to_card(source: &str, card_name: &str) -> bool {
    let Some(addr) = card_name.strip_prefix("bluez_card.") else {
        return false;
    };
    if source.ends_with(".monitor") {
        return false;
    }
    let pa = format!("bluez_source.{addr}.");
    let pw = format!("bluez_input.{addr}.");
    source.starts_with(&pa) || source.starts_with(&pw)
}

/// 解析 `pactl list cards` 输出（须以 `LC_ALL=C` 执行 pactl）。
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
// 命令执行（带超时；pactl 固定 locale）
// ─────────────────────────────────────────────────────────────

/// 带总时限地执行命令：超时 kill 并回收子进程，返回 None。
/// 快捷键回调线程上不允许无限期阻塞。
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<std::process::Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// 运行 pactl。固定 `LC_ALL=C` 并清理 `LANGUAGE`：pactl 的输出字段会被
/// 本地化（中文桌面输出「卡/名称/配置文件」），按英文解析会静默失败。
/// 执行失败、超时或非零退出返回 None。
fn run_pactl(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("pactl");
    cmd.args(args).env("LC_ALL", "C").env_remove("LANGUAGE");
    run_with_timeout(cmd, PACTL_TIMEOUT)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// pactl 变更命令（set-*）是否成功。
fn pactl_set(args: &[&str]) -> bool {
    run_pactl(args).is_some()
}

// ─────────────────────────────────────────────────────────────
// 自动模式主流程
// ─────────────────────────────────────────────────────────────

/// 录音开始前的自动切换入口（`bluetoothSwitch == "auto"` 时调用）。
///
/// `mic_config` 为用户配置的录音设备名：明确选择了非蓝牙设备
/// （USB/3.5mm 等）时尊重用户选择，不动蓝牙 profile。
///
/// 返回本次录音实际使用的蓝牙 source 名（仅日志/诊断用途）；任何一步
/// 不满足条件都返回 None 并保持系统原状（或已恢复原状），绝不阻断录音。
pub fn prepare_bluetooth_recording(mic_config: &str) -> Option<String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mic_config;
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        // 用户明确选择了非蓝牙录音设备：蓝牙只用于听音，不打扰其 profile
        if mic_config != "system-default" && !mic_config.starts_with("bluez") {
            return None;
        }

        // 1. 找当前连接的蓝牙声卡
        let cards = parse_cards(&run_pactl(&["list", "cards"])?);
        let card = cards.into_iter().find(|c| c.is_bluetooth())?;

        // 2. 默认录音设备已指向该蓝牙麦克风且 profile 含输入：无需任何切换
        let cur_default = run_pactl(&["get-default-source"])?;
        if source_belongs_to_card(&cur_default, &card.name) && card.active_profile_has_input() {
            return Some(cur_default);
        }

        // 3. 暂存快照（无论是否需要切 profile，都可能改默认录音设备）
        let snapshot = SwitchSession {
            card_name: card.name.clone(),
            prev_profile: card.active_profile.clone(),
            prev_default_source: cur_default,
        };

        // 4. 需要时切 profile。失败则系统未改变，直接放弃（无需恢复）
        if !card.active_profile_has_input() {
            let target = pick_input_profile(&card.profiles)?;
            if !pactl_set(&["set-card-profile", &card.name, &target]) {
                eprintln!("[audio_switch] set-card-profile to {} failed", target);
                return None;
            }
        }

        // 5. 系统状态首次改变后立即落快照：此后任何失败路径都走统一恢复
        *session().lock().unwrap() = Some(snapshot);

        // 6. 轮询等录音 source 就绪并临时设为默认录音设备。
        //    profile 未变（上次停在 HFP）时 source 通常已在，首轮即中；
        //    切换后需等 HFP 握手完成 source 才会出现。
        let deadline = Instant::now() + Duration::from_millis(READY_POLL_TIMEOUT_MS);
        loop {
            if let Some(out) = run_pactl(&["list", "sources", "short"]) {
                if let Some(source) = parse_source_names(&out)
                    .into_iter()
                    .find(|n| source_belongs_to_card(n, &card.name))
                {
                    if pactl_set(&["set-default-source", &source]) {
                        return Some(source);
                    }
                    break; // set 失败 → 统一恢复
                }
            }
            if Instant::now() > deadline {
                eprintln!(
                    "[audio_switch] bluetooth source not ready in {}ms, restoring",
                    READY_POLL_TIMEOUT_MS
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(READY_POLL_INTERVAL_MS));
        }

        // 超时或失败：统一走幂等恢复（成功清快照，失败保留供重试），
        // 不再把回滚结果丢在栈上
        restore_session();
        None
    }
}

/// 恢复自动切换前的系统状态。幂等：无进行中的切换时是 no-op。
///
/// 恢复失败（如音频服务无响应）时保留会话，下次调用或应用退出时重试；
/// 仅在查询成功且明确找不到声卡（耳机拔出/断连）时才视为已恢复并清掉
/// 会话 —— 查询失败不能当作设备消失，否则会永久丢失快照。
pub fn restore_session() {
    #[cfg(target_os = "linux")]
    {
        let mut guard = session().lock().unwrap();
        let Some(s) = guard.clone() else { return };

        let card_gone = match run_pactl(&["list", "cards", "short"]) {
            Some(out) => !out.lines().any(|l| l.contains(&s.card_name)),
            None => false,
        };
        if card_gone {
            // 声卡已不在：PipeWire 自行清理路由，无需恢复
            *guard = None;
            return;
        }

        // 先还原默认录音设备再还原 profile，避免默认指向悬空的 HFP source
        let mut ok = true;
        if !s.prev_default_source.is_empty() {
            ok &= pactl_set(&["set-default-source", &s.prev_default_source]);
        }
        ok &= pactl_set(&["set-card-profile", &s.card_name, &s.prev_profile]);

        if ok {
            *guard = None;
        } else {
            eprintln!("[audio_switch] restore failed, will retry on next finish/exit");
        }
    }
}

/// 录音结束的统一收尾：恢复自动切换 + 执行用户 post hook。
/// 所有录音结束路径（正常/手动停/取消/出错/启动失败）都应调用这里。
pub fn finish_recording(hook: &str) {
    restore_session();
    post_record(hook);
}

// ─────────────────────────────────────────────────────────────
// 自定义 hook（逃生舱）
// ─────────────────────────────────────────────────────────────

/// 录音开始前执行 hook 命令（`sh -c`，5 秒上限，超时终止）。
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

/// 录音结束后执行 hook 命令（`sh -c`，5 秒上限，超时终止）。
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
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(hook);
    let result = run_with_timeout(cmd, HOOK_TIMEOUT);

    match result {
        Some(output) if output.status.success() => {
            eprintln!("[audio_switch] {} hook succeeded", name);
        }
        Some(output) => {
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
        None => {
            eprintln!("[audio_switch] {} hook timed out ({}s) or failed to launch", name, HOOK_TIMEOUT.as_secs());
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
    fn localized_card_output_parses_to_zero_cards() {
        // R1 的教训：pactl 输出随 locale 本地化，英文解析在此输出上得到 0 张卡。
        // 这是运行时固定 LC_ALL=C 的原因；解析器本身只认英文输出。
        let zh = "卡 #45\n\t名称: bluez_card.88_92_CC_E7_A4_48\n\t驱动: module-bluez5-device.c\n\t活动配置文件: a2dp_sink\n";
        assert!(parse_cards(zh).is_empty());
    }

    #[test]
    fn pick_profile_prefers_a2dp_hfp_over_headset() {
        let cards = parse_cards(CARDS_SAMPLE);
        let bt = cards.iter().find(|c| c.is_bluetooth()).unwrap();
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
        let out = "88\tbluez_input.88_92_CC_E7_A4_48.0\tmodule-bluez5-device.c\ts16le 1ch 16000Hz\tRUNNING\n\
                   41\talsa_input.pci-0000_00_1f.3.analog-stereo\tmodule-alsa-card.c\ts16le 2ch 44100Hz\tSUSPENDED\n";
        assert_eq!(
            parse_source_names(out),
            vec![
                "bluez_input.88_92_CC_E7_A4_48.0".to_string(),
                "alsa_input.pci-0000_00_1f.3.analog-stereo".to_string(),
            ]
        );
    }

    #[test]
    fn source_matching_covers_both_generations() {
        let card = "bluez_card.88_92_CC_E7_A4_48";
        // PulseAudio 命名
        assert!(source_belongs_to_card(
            "bluez_source.88_92_CC_E7_A4_48.handsfree_head_unit",
            card
        ));
        // PipeWire / WirePlumber 命名
        assert!(source_belongs_to_card("bluez_input.88_92_CC_E7_A4_48.0", card));
        // 输出 monitor 不是录音设备
        assert!(!source_belongs_to_card("bluez_output.88_92_CC_E7_A4_48.0.monitor", card));
        assert!(!source_belongs_to_card("alsa_input.pci-0000_00_1f.3.analog-stereo", card));
        // 其他蓝牙设备
        assert!(!source_belongs_to_card("bluez_source.AA_BB_CC_DD_EE_FF.handsfree_head_unit", card));
    }

    #[test]
    fn int_field_parses_despite_trailing_punctuation() {
        assert_eq!(int_field("(sinks: 1, sources: 10, priority: 30)", "sources:"), Some(10));
        assert_eq!(int_field("(sinks: 1, sources: 0)", "sources:"), Some(0));
        assert_eq!(int_field("(sinks: 1)", "sources:"), None);
    }
}
