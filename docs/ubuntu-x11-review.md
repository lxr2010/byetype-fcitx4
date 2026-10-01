# Ubuntu X11 移植代码审查报告

本报告审查 ByeType 的 Ubuntu X11 移植，供后续修复和发布验收使用。当前代码可以完成前端构建并通过现有 Rust 测试，但蓝牙自动切换在中文环境和 PipeWire 环境中存在确定的兼容性缺陷，录音异常恢复及 X11 剪贴板处理也有遗漏。建议修复 P1 问题，并完成录音恢复与剪贴板回归后再发布。

审查日期为 2026 年 10 月 1 日。范围为 `ccbc73aebed588a719684fafc47b3bbbeb8ef9c2`（上游 v1.22.4）到 `e456dd42962857c11e76db30af9513f0b402ebf0`，共 15 个提交、20 个文件。重点覆盖 X11 初始化、输入法、粘贴、截图、窗口定位、蓝牙录音、配置及 Linux CI 和打包。以下行号对应被审查提交。本次只新增审查报告，未修改产品代码。

## 审查意见

共确认 12 项问题：2 项 P1、9 项 P2、1 项 P3。P1 表示应优先修复的主要场景缺陷，P2 表示特定条件下的功能或状态错误，P3 表示影响范围较窄的功能遗漏。

| 编号 | 等级 | 问题 | 证据类型 |
| --- | --- | --- | --- |
| R1 | P1 | 中文环境下无法解析声卡 | 本机真实 pactl 输出与原函数验证 |
| R2 | P1 | PipeWire 蓝牙输入名称匹配失败 | 上游源码与模拟 pactl 验证 |
| R3 | P2 | 录音启动失败未执行音频恢复 | 完整调用路径静态确认 |
| R4 | P2 | 已处于 HFP 时未切换默认录音设备 | 模拟 pactl 验证 |
| R5 | P2 | 恢复时查询失败会丢弃快照 | 模拟 pactl 故障验证 |
| R6 | P2 | 就绪超时且回滚失败后无法重试 | 模拟 pactl 故障验证 |
| R7 | P2 | 外部命令阻塞绕过就绪超时 | 模拟慢命令验证 |
| R8 | P2 | 粘贴出错时跳过原剪贴板恢复 | Xvfb 与原函数验证 |
| R9 | P2 | 缺失 xdotool 时未走 enigo 降级 | Xvfb 与原函数验证 |
| R10 | P2 | 剪贴板内容未获得持久的 X11 owner | Xvfb 与原函数验证 |
| R11 | P2 | 已选非蓝牙麦克风仍会切换蓝牙设备 | 配置到设备选择的静态确认 |
| R12 | P3 | 获取窗口标题的 xdotool 参数错误 | 本机命令验证 |

### R1 P1 为 pactl 解析固定语言环境

位置：[audio_switch.rs:182](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:182)，相关解析见 [audio_switch.rs:90](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:90)。

`run_pactl()` 继承桌面语言环境，而 `parse_cards()` 只识别 `Card #`、`Name:`、`Profiles:`、`Active Profile:` 等英文字段；profile 解析也依赖英文的 `sources:` 和 `available: yes`。中文桌面输出的是“卡”“名称”“配置文件”“活动配置”等字段，因此自动切换会把已连接设备当成不存在并静默跳过。

本机使用同一组声卡，以 `LC_ALL=C LANGUAGE=C` 和 `LC_ALL=zh_CN.UTF-8 LANGUAGE=zh_CN` 分别执行 `pactl list cards`，将结果交给原始 `parse_cards()`：前者得到 **2 张声卡**，后者得到 **0 张声卡**。PulseAudio 的输出字段确实经过本地化，见 [pactl 15.99.1 源码](https://github.com/pulseaudio/pulseaudio/blob/v15.99.1/src/utils/pactl.c#L1137)。

建议：对子进程明确设置 `LC_ALL=C`，并清理或固定 `LANGUAGE`；也可在支持版本范围内改用 `pactl --format=json`。验收需覆盖中文会话，确认其设备识别结果与英文环境一致。

### R2 P1 兼容 PipeWire 的蓝牙输入命名

位置：[audio_switch.rs:75](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:75)，匹配调用见 [audio_switch.rs:241](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:241)。

`source_prefix()` 将 `bluez_card.<地址>` 固定转换成 `bluez_source.<地址>`。PipeWire 配合 WirePlumber 使用的输入节点名通常为 `bluez_input.<地址>.<编号>`；WirePlumber 0.4.17 的[节点创建代码](https://github.com/PipeWire/wireplumber/blob/0.4.17/src/scripts/monitors/bluez.lua#L255)明确采用这一命名。于是即使 HFP 麦克风已出现，轮询仍匹配不到它，最终恢复 A2DP，无法完成蓝牙录音准备。

模拟 `pactl` 返回可用的 `bluez_input.11_22_33_44_55_66.0` 后，直接调用原始 `prepare_bluetooth_recording()`，结果为 `None`，约 1.69 秒后打印 `HFP source not ready in 1500ms, rolling back`。同一测试换成 `bluez_source.*` 则成功。这是命名匹配验证，未在真实 PipeWire 蓝牙硬件上录音。

建议：优先通过 source 的声卡索引或设备属性关联声卡，避免推导固定名称；最小修复至少同时支持 PulseAudio 和 PipeWire 命名，并排除输出 monitor。为两种音频服务各保留一份真实输出 fixture。

### R3 P2 录音启动失败也必须执行收尾

位置：[shortcut.rs:274](/home/xxie/code/byetype/src-tauri/src/shortcut.rs:274)，遗漏分支见 [shortcut.rs:363](/home/xxie/code/byetype/src-tauri/src/shortcut.rs:363)。

自动切换和 `pre_record_hook` 已在 `recorder.start(&mic)` 之前执行，但 `start()` 的错误分支仅取消任务并发送错误事件，没有调用 `finish_recording()`。`task::cancel_recording()` 也只清理气泡与任务计数。设备被独占、音频格式不支持、打开音频流失败等情况下，耳机会继续停在 HFP，默认输入设备也不会立即恢复，自定义 post hook 则完全遗漏。

这是静态控制流确认：录音器存在多个可返回错误的设备初始化步骤，错误出口没有音频恢复调用；没有通过改变用户真实麦克风来触发它。建议在启动失败分支执行与停止失败相同的收尾，或使用录音会话守卫统一管理。验收应在蓝牙切换成功后注入 `recorder.start()` 失败，确认 profile、默认输入和 post hook 均被恢复或执行。

### R4 P2 已处于 HFP 时仍须正确选择录音来源

位置：[audio_switch.rs:209](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:209)。

当蓝牙声卡已经处于含输入通道的 profile 时，函数直接返回 source 名，没有保存原默认输入，也没有执行 `set-default-source`。返回值在快捷键调用方只用于日志，不参与 `recorder.start(&mic)` 的设备选择。因此在使用 `system-default`、耳机已处于 HFP、系统默认输入仍为内置麦克风的场景下，日志声称使用蓝牙，实际仍录制内置麦克风。

模拟验证中，准备结果为 `Some(bluez_source...)`，但默认输入始终是 `alsa_input.internal`，整个准备过程只有两次查询。建议将“是否需要切换 profile”和“是否需要切换默认输入”分开处理，即使无需切 profile，也保存并设置默认输入，结束后恢复。

### R5 P2 不要把声卡查询失败当作声卡消失

位置：[audio_switch.rs:275](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:275)。

`run_pactl(["list", "cards", "short"])` 失败时，`.unwrap_or(true)` 将 `card_gone` 设为真，随后清空会话快照。音频服务短暂不可达或查询返回非零状态并不能证明耳机已断开；这里会丢失原 profile 和默认输入，后续停止或退出也无法重试恢复。

故障注入让第一次恢复查询失败、之后允许正常查询。连续调用两次 `restore_session()` 后，profile 仍为 HFP，默认输入仍为蓝牙；第二次调用未再执行任何 `pactl` 命令。建议仅在查询成功且明确找不到声卡时清理快照，查询失败则保留会话并记录错误。

### R6 P2 就绪超时后的回滚失败必须保留快照

位置：[audio_switch.rs:246](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:246) 和 [audio_switch.rs:251](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:251)。

会话快照只在找到输入 source 后才存入 `SESSION`。若切换 HFP 已成功、source 迟迟未出现，超时路径仅尝试一次恢复 profile，忽略命令结果并返回。此时回滚一旦失败，快照随栈变量销毁，正常收尾及退出恢复都变成空操作。

模拟“切到 HFP 成功、source 不出现、恢复 A2DP 失败”后，准备函数返回 `None`；再调用两次恢复，设备仍处于 HFP，且没有新的恢复命令。建议在系统状态首次改变前后立即建立可恢复会话，并在回滚成功前保留快照；超时和正常收尾使用同一恢复路径。

### R7 P2 为 pactl 和录音钩子设置执行超时

位置：[audio_switch.rs:182](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:182)，直接命令调用还见 [audio_switch.rs:226](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:226)、[audio_switch.rs:350](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:350)。

所有 `pactl` 调用和 shell hook 都使用同步 `.output()`，没有进程执行超时。1.5 秒 deadline 只在一次 `list sources` 命令返回后检查，且声卡发现和 profile 切换发生在 deadline 建立之前。因此音频服务或 hook 阻塞时，所谓“超时降级、不阻断录音”并不成立。

模拟单次 source 查询耗时 2.2 秒，原始准备函数耗时约 **2.42 秒**，仍以成功返回而未触发超时。调用来自全局快捷键处理器，因此长期阻塞会阻止该快捷键事件线程继续处理开始、停止或松键事件；这里不能据此断言整个 GTK 界面必然冻结。

建议统一实现带总时限的命令执行器，超时后终止并回收子进程，进入可重试的恢复路径。自定义 hook 也需要有界执行，并处理 shell 子进程残留。验收包括慢查询、无响应命令及超时后的下一次快捷键操作。

### R8 P2 粘贴失败后仍应恢复原剪贴板

位置：[clipboard.rs:465](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:465)。

Linux 分支在公共恢复逻辑之前执行 `paste_result?`。只要模拟粘贴返回错误，函数就提前返回，跳过 `restore_clipboard(backup)`，破坏“关闭覆盖剪贴板后保留原内容”的行为。macOS 和 Windows 的错误被外层 `paste_result` 收集，新增 Linux 分支没有接入这一结构。

在隔离 Xvfb 中保留有效 clipboard owner，先写入 `ORIGINAL_CLIPBOARD`，隐藏 `xdotool` 后调用原始 `paste_text("VOICE_RESULT", false)`：函数返回启动命令失败，剪贴板却保留 `VOICE_RESULT`。保留 owner 是为排除 R10 的生命周期问题。

建议将 Linux 粘贴结果纳入公共错误收集，先执行输入法和剪贴板恢复，再返回错误。验收需要同时断言错误返回值与原剪贴板内容。

### R9 P2 缺失 xdotool 时执行既定降级

位置：[clipboard.rs:189](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:189)。

enigo 降级只覆盖“成功启动 xdotool，但其退出状态非零”。命令不存在或不可执行时，`.output().map_err(...)?` 直接返回错误。文档将 xdotool 描述为仅影响定位的可选依赖，因此按文档使用源码构建或 AppImage、又没有安装 xdotool 的用户，会失去全部自动粘贴功能。

Xvfb 中仅从子进程 PATH 隐藏 xdotool，原始粘贴函数确定返回 `Failed to run xdotool: No such file or directory`，没有执行 enigo。建议将启动失败也纳入降级路径，并同步纠正文档中的用途和必需性说明。此项与 R8 是两个独立错误：修复降级不能保证其他粘贴错误时仍能恢复剪贴板。

### R10 P2 为 X11 剪贴板保留有效 owner

位置：[clipboard.rs:437](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:437)，相关通用恢复 helper 见 [clipboard.rs:36](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:36)，新增打包依赖见 [tauri.conf.json:72](/home/xxie/code/byetype/src-tauri/tauri.conf.json:72)。

Linux 使用 arboard 写入后，需要有有效的剪贴板所有者持续提供内容；最后一个 `Clipboard` 实例释放时，其他应用可能无法再读取，见 [arboard 3.6.1 的 Linux 行为说明](https://docs.rs/arboard/3.6.1/arboard/struct.Clipboard.html#linux)。当前仅成功执行 xclip 的路径有外部进程接管，而 deb 依赖和运行说明均未列出 xclip。缺失 xclip 时声称使用 arboard 降级，但局部实例在函数返回时释放。

在没有剪贴板管理器的 Xvfb 中验证了两种情况：隐藏 xclip、让按键命令成功，`paste_text(..., true)` 返回 `Ok(())`，随后读取为 `ContentNotAvailable`；即使 xclip 存在，调用 `paste_text(..., false)` 恢复原内容后，局部 arboard owner 释放，原剪贴板也变成不可用。因此只添加 xclip 依赖不能完整修复保留模式。

建议建立应用生命周期内的 Linux 剪贴板 owner，将写入和恢复交给同一服务；如果继续依赖 xclip，补齐打包及运行文档，并明确支持范围。这里包含沿用的通用 helper 在 Linux 上的适配遗漏，不应误称为本次新增了 `restore_clipboard()`。验收必须在没有剪贴板管理器的会话中，于函数返回后从另一实例或进程读取。

### R11 P2 尊重用户明确选择的非蓝牙麦克风

位置：[shortcut.rs:274](/home/xxie/code/byetype/src-tauri/src/shortcut.rs:274)，设备选择见 [audio/mod.rs:42](/home/xxie/code/byetype/src-tauri/src/audio/mod.rs:42)。

只要自动切换开关为默认的 `auto`，开始任何录音都会寻找蓝牙声卡并修改其 profile 和系统默认输入，没有检查已经读取的 `config.general.microphone`。当用户明确选择 USB 或有线麦克风，同时连接蓝牙耳机听音频时，录音器仍会按名称使用选定麦克风，但耳机却被无必要地降到 HFP，增加启动等待并改变系统音频状态。这也不符合运行文档中非蓝牙麦克风“不经过此流程”的描述。

此项由配置与设备选择调用链静态确认，未更改真实系统默认输入。建议让自动切换策略接收实际录音设备选择；明确选择非蓝牙设备时跳过，默认设备场景再确定需要使用的蓝牙设备。验收需覆盖“蓝牙耳机播放加 USB 麦克风录音”。

### R12 P3 修正获取窗口标题的命令参数

位置：[clipboard.rs:271](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:271)。

代码执行 `xdotool getwindowname --window <wid>`，但该子命令接受的位置参数是 `getwindowname [window]`。本机在 Xvfb 下执行代码中的参数形式，确定返回 `getwindowname: unrecognized option '--window'`，退出码为 1。调用方还忽略了退出状态，所以窗口标题实际上没有参与粘贴策略识别。

已有 WM_CLASS 分类仍可工作，因此此项影响主要是依赖标题的回退场景。建议改为 `getwindowname <wid>`，先确认退出成功再读取 stdout，并增加一次真实命令调用测试，避免只有分类纯函数测试通过而命令接口错误。

## 已执行验证及限制

本机为 Ubuntu 22.04.5 LTS、X11、PulseAudio 15.99.1，Rust 与 Cargo 均为 1.95.0。以下系统查询只读；蓝牙状态修改全部由临时 `pactl` 替身承接。剪贴板及按键相关验证在独立 Xvfb 中执行，不向用户桌面发送模拟按键。

| 验证 | 结果 | 能证明的范围 |
| --- | --- | --- |
| `npm run build` | 通过，存在大于 500 kB 的 chunk 警告 | TypeScript 检查及 Vite 构建 |
| `env -u DISPLAY -u WAYLAND_DISPLAY cargo test --locked --quiet` | 115 个测试通过 | 无桌面环境的单元测试；剪贴板测试有提前返回分支 |
| `xvfb-run -a cargo test --locked --quiet clipboard:: -- --nocapture` | 6 个测试通过；其中出现一次 snapshot 打开剪贴板失败日志 | 现有剪贴板测试的有限覆盖，不代表跨应用粘贴验收 |
| `env -u DISPLAY -u WAYLAND_DISPLAY cargo clippy --locked --quiet` | 退出码 0，有警告 | 静态检查通过，未达到零警告 |
| `git diff --check ccbc73a..e456dd4` | 通过 | 本次移植 diff 无空白错误 |
| 原始 audio_switch 模块加模拟 pactl | 6 个场景完成 | 正常切换对照、PipeWire 命名、已在 HFP、恢复查询失败、回滚失败、慢命令 |
| 真实 pactl 输出交给原始解析器 | C 环境 2 张卡，中文环境 0 张卡 | R1 的语言兼容性问题 |
| 原始 clipboard 模块加 Xvfb 与受控 PATH | 复现 R8、R9、R10 | 错误路径和 owner 生命周期，不代表实际目标应用已收到粘贴 |
| `xvfb-run -a xdotool getwindowname --window 1` | 退出码 1，参数不支持 | R12 的命令语法问题 |

临时验证代码保存在 [audio_probe.rs](/tmp/byetype-review-e456dd4/audio_probe.rs)、[pactl 替身](/tmp/byetype-review-e456dd4/pactl)、[音频场景运行器](/tmp/byetype-review-e456dd4/run_audio.py)、[clipboard_probe.rs](/tmp/byetype-review-e456dd4/clipboard_probe.rs) 和 [retention_probe.rs](/tmp/byetype-review-e456dd4/retention_probe.rs)。这些程序直接引用被审查的源文件，未将替身放进真实桌面的 PATH；它们是本次审查的临时证据，未加入项目测试集。

尚未执行真实蓝牙耳机录音、Fcitx 候选窗下的跨应用粘贴、截图 OCR 服务调用、HiDPI 多屏视觉验收，以及全新 Ubuntu 环境的 deb 和 AppImage 安装验证。发布工作流仅做静态检查，未触发 GitHub Release，也未验证自动更新产物。Wayland 已在项目文档中明确属于受限场景，本报告未将其原生按键限制列为新增缺陷。

## 修复后验收建议

1. 在中文和英文会话中，分别验证 PulseAudio 与 PipeWire 的蓝牙发现、HFP 就绪、默认输入切换及恢复。
2. 对正常停止、PTT 短按取消、启动失败、就绪超时、恢复失败和退出路径注入故障，断言原 profile 与默认输入仍可恢复，post hook 按约定执行。
3. 在无剪贴板管理器的 X11 会话中覆盖 xdotool 和 xclip 缺失、模拟按键失败、保留原剪贴板开关，以及函数返回后的跨进程读取。
4. 用真实 GNOME Terminal、普通文本编辑器和处于插入模式的 Vim GUI 验证粘贴及 Fcitx 状态恢复，再检查截图取消和多屏缩放。
5. 在干净的 Ubuntu 22.04 及实际支持的较新 Ubuntu 环境安装发布产物，验证运行时依赖和自动更新，不能用开发机已有工具代替安装验收。
