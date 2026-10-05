# Ubuntu X11 移植代码审查报告

本报告记录 ByeType 的 Ubuntu X11 移植审查及修复后的复审。**本轮仍不建议通过：当前有 7 项需要处理的问题，包含 1 项 P1、6 项 P2。** 中文环境和 PipeWire 命名等主要兼容问题已有修复，但新增的窗口标题分类会向普通浏览器发送错误快捷键，命令超时、蓝牙恢复及剪贴板初始化仍有缺陷。117 个 Rust 测试通过，不足以覆盖这些已复现的场景。

复审日期为 2026 年 10 月 1 日。范围为 `fa0ba40..e6cbaf5588351d075b450e8eaebae02f40e0ce79`，即 `1a29d6e` 和 `e6cbaf5` 两条修复提交，共 5 个文件。`fa0ba40` 只提交了首轮报告，产品代码与 `e456dd4` 相同。本轮 F 编号的代码链接和行号对应 `e6cbaf5`；本次仅更新报告，未修改产品代码或提交 Git 变更。

## 本轮复审意见

| 编号 | 等级 | 问题 | 与首轮的关系 |
| --- | --- | --- | --- |
| F1 | P1 | 窗口标题覆盖应用类型，浏览器可能收到刷新快捷键 | R12 修复激活的回归 |
| F2 | P2 | 等待进程退出前不读取管道，正常大输出被误判超时 | R7 新执行器引入 |
| F3 | P2 | hook 后代进程可绕过超时或在超时后继续执行 | R7 未完全修复 |
| F4 | P2 | profile 命令超时后假定系统未变化，丢失恢复快照 | 新超时路径与 R6 衔接遗漏 |
| F5 | P2 | 再次录音覆盖上一次恢复失败后保留的快照 | R4 与 R6 修复之间的回归 |
| F6 | P2 | 首次剪贴板连接失败被永久缓存，后续无法恢复 | R10 新 holder 引入 |
| F7 | P2 | 首次截图或预览复制仍未建立持久剪贴板 owner | R10 修复覆盖不完整 |

### F1 P1 按真实应用类型选择粘贴键

位置：[clipboard.rs:338](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:338)，分类逻辑见 [clipboard.rs:286](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:286)。

修复 `getwindowname` 后，真实窗口标题开始传入 `classify_paste_method()`。该函数把 WM_CLASS 与标题拼接后做子串匹配，标题中的 `neovide`、`gvim` 等词可以覆盖明确属于浏览器的 WM_CLASS。浏览器显示 Neovide 搜索结果或文档页面时，语音粘贴会发送 `Ctrl+R` 再发送 `+`，而不是 `Ctrl+V`，可能刷新页面并丢失尚未提交的输入。

在 Xvfb 中运行修复前后两版原始 `paste_text()`，用命令替身提供 `WM_CLASS = google-chrome` 和标题 `Neovide - Google Search - Google Chrome`。修复前记录的按键是 `ctrl+v`，当前版本是 `ctrl+r`、`plus`。标题换成 `Football scores - Google Chrome`，当前版本还会因 `foot` 子串误判终端而发送 `ctrl+shift+v`。测试记录了真实分类逻辑生成的命令，没有实际刷新用户的浏览器。

建议让可靠的 WM_CLASS 分类优先并结束判断，只在无法识别类名时谨慎使用标题回退；不要让网页标题或文件名改变已识别应用的快捷键语义。回归测试应包含浏览器标题中的 Neovide、Terminal 和 Football，而不能继续把“非终端类名加终端关键词标题”视为必然的终端。

### F2 P2 执行命令时持续读取输出管道

位置：[audio_switch.rs:210](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:210)。

新的执行器把 stdout 和 stderr 设为管道，却一直等 `try_wait()` 报告退出后才调用 `wait_with_output()` 读取。一旦任一输出超过管道容量，子进程会阻塞在写入操作，父进程又在等待它退出，最终把正常命令当作超时杀死。声卡或属性较多的 `pactl list cards`、输出日志较多的自定义 hook 都可能触发。

本机管道容量为 65,536 字节。让 pactl 替身立即输出约 128 KiB、语法仍有效的声卡列表，修复前约 205 ms 完成准备并可恢复；当前版本约 2,009 ms 后返回 `None`，尚未进入设备切换。该替身没有睡眠，差异来自输出管道未被及时读取。

建议在进程运行期间并行或异步消费 stdout 和 stderr，并将输出收集与进程退出共同纳入时限；若限制保存的输出量，也应继续排空管道，避免再次阻塞。增加超过管道容量的成功命令测试。

### F3 P2 超时必须覆盖输出收集和整个 hook 进程组

位置：[audio_switch.rs:218](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:218)，终止逻辑见 [audio_switch.rs:220](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:220)。

`sh` 退出后立即进入没有时限的 `wait_with_output()`；后台子进程若继承了输出管道，父进程仍可能无限等待 EOF。另一个分支只 `kill()` 直接子进程，不能保证终止 shell 启动的其他进程。后者可以在“hook 已超时、录音继续或已经恢复”的阶段再次修改设备状态。

两种情况均用原始 `pre_record()` 复现：hook 为 `/bin/sleep 6.5 &` 时，配置的 5 秒限制被绕过，约 **6,506 ms** 后还记录成功；hook 启动 Python 子进程、6 秒后写一个临时标记文件时，父调用约 **5,009 ms** 返回超时，但子进程随后仍写出了文件。测试产生的进程组已清理，写入仅发生在审查临时目录。

建议使用独立进程组管理 hook，超时终止并回收受控的整组进程，并为管道读取保留同一个截止时间。验收需确认超时返回后不会再出现延迟副作用，而不只是检查 `pre_record()` 是否返回。

### F4 P2 将变更命令超时视为结果未知

位置：[audio_switch.rs:290](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:290)。

代码假定 `set-card-profile` 返回失败就表示系统没有改变，直接返回，而快照要到该命令报告成功后才保存。新增的 2 秒超时使这个假定更不可靠：音频服务可能已经切到 HFP，只是客户端还没有正常退出。此时 `pactl_set()` 返回 false，但 `SESSION` 尚为空，后续收尾和退出都无法恢复。

模拟 `set-card-profile` 先将状态改成 HFP，再等待 3 秒后退出。当前版本约 2.11 秒返回 `None`；连续调用两次 `restore_session()` 都没有发出恢复命令，profile 一直停在 HFP。修复前等待命令完成后保留快照，可以正常恢复 A2DP。该验证只修改替身中的状态文件。

建议在发出可能改变系统状态的命令之前建立快照，将超时和连接中断视为“结果未知”，随后查询或尝试幂等恢复。只有确认未改变状态或恢复成功后才能清理快照。

### F5 P2 不要覆盖尚未恢复完成的会话快照

位置：[audio_switch.rs:299](/home/xxie/code/byetype/src-tauri/src/audio_switch.rs:299)。

`restore_session()` 失败后会保留旧快照，但下一次准备录音直接用当前系统状态构造并覆盖它。尤其是“恢复默认输入成功、恢复 A2DP 失败”的场景：系统暂时处于 HFP，旧快照仍记着 A2DP。下一次录音走新的“已经有输入通道”路径，把 HFP 当作原 profile 保存，之后的正常收尾也只会恢复到 HFP，真正的 A2DP 状态永久丢失。

用同一进程执行“开始、停止时第一次恢复 profile 失败、再次开始、再次停止”，修复前最终恢复到 `a2dp_sink`，当前版本最终保持 `handsfree_head_unit`。故障只注入一次，最后一次恢复命令本身成功，因而这是快照被覆盖的问题，不是持续的外部故障。

建议新录音开始前先处理尚未完成的恢复；若需要复用同一设备，会话仍须保留最初的状态，不能用上一次切换留下的中间状态替换。将“恢复失败后再次录音”加入集成测试。

### F6 P2 首次剪贴板初始化失败后允许重试

位置：[clipboard.rs:14](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:14)。

`OnceLock` 初始化时把 `Clipboard::new().ok()` 的结果永久存为 `Some` 或 `None`。如果首次 X11 连接短暂失败，`None` 也成为已初始化状态；之后所有调用直接返回 `clipboard unavailable`，不会再次尝试连接。暂时不可用变成了必须重启应用才能恢复的永久失败。

在同一测试进程中暂时移除 DISPLAY，让第一次创建失败，然后恢复有效的 Xvfb DISPLAY。此时直接调用 `arboard::Clipboard::new()` 已经成功，但当前 `paste_text()` 第二次仍返回 `clipboard unavailable`；修复前第二次调用可以成功。这里是对瞬时连接故障的模拟，没有重启用户的 X server。

建议 `OnceLock` 仅保存持久容器，在锁内发现内容为 `None` 时重新尝试创建连接，并保留实际错误信息。对恢复后的第二次操作进行断言。

### F7 P2 让截图与预览复制也初始化共享 owner

位置：[clipboard.rs:10](/home/xxie/code/byetype/src-tauri/src/clipboard.rs:10)，遗漏调用方见 [commands.rs:278](/home/xxie/code/byetype/src-tauri/src/commands.rs:278) 和 [task/mod.rs:747](/home/xxie/code/byetype/src-tauri/src/task/mod.rs:747)。

R10 的修复只在语音 `paste_text()` 及其快照和恢复 helper 中使用共享 holder。截图 OCR 的写入、预览窗口失焦与复制按钮调用的 `update_clipboard_text()` 仍创建局部 `Clipboard`。在没有剪贴板管理器的 X11 会话中，启动应用后先截图或点击预览复制，holder 尚未初始化，函数虽返回成功，内容却在局部 owner 释放后消失。先用一次语音输入才会间接让这些路径保留内容，造成依赖操作顺序的行为。

测试从 `commands.rs` 原样提取 `update_clipboard_text()` 函数体，在独立 Xvfb 进程中验证：首次调用返回 `Ok(())`，随后读取为 `ContentNotAvailable`；调用一次当前 `paste_text()` 后再运行相同复制函数，则可读到 `OCR_LATER`。这是 R10 尚未覆盖的既有 Linux 路径，不是声称本提交新增了 OCR 写入代码。

建议提供统一的剪贴板写入接口，让语音、截图、预览复制和失焦同步都经过它，或在应用启动时可靠建立 owner。验收必须包括启动后的第一次复制，不能先运行语音输入来预热。

## 上轮问题核验状态

“已修复”指首轮报告描述的具体触发路径已有代码或隔离验证支持，不代表完成了全部硬件验收。

| 首轮编号 | 本轮状态 | 依据及剩余问题 |
| --- | --- | --- |
| R1 | 已修复 | 在中文父环境中，pactl 替身确认每次调用均为 `LC_ALL=C` 且清除了 LANGUAGE |
| R2 | 已修复 | `bluez_input.*` 和 `bluez_source.*` 均可完成准备与恢复；新增匹配单元测试通过 |
| R3 | 已修复 | 静态确认启动错误分支已调用 `finish_recording()`；未用真实麦克风制造失败 |
| R4 | 基础场景已修复 | 已在 HFP、默认输入为内置麦克风时可正确临时切换并恢复；再次录音回归见 F5 |
| R5 | 已修复 | 恢复查询失败不会丢弃快照，故障注入后可以恢复原状态 |
| R6 | 部分修复 | source 就绪超时后的首次回滚失败可重试成功；变更命令超时和跨录音快照仍有 F4、F5 |
| R7 | 未完全修复 | 普通慢 pactl 已被终止，但大输出和后代进程仍有 F2、F3；1.5 秒轮询预算也不等于整个准备过程的总时限 |
| R8 | 已修复 | 故意使 enigo 无法连接时，返回粘贴错误仍保留 `ORIGINAL` |
| R9 | 已修复 | 隐藏 xdotool 后在 Xvfb 中降级 enigo，粘贴调用成功 |
| R10 | 部分修复 | 无 xclip 时语音结果和保留模式均可读；初始化重试和首次其他复制入口仍有 F6、F7 |
| R11 | 已修复 | 明确选择 `USB Mic` 时准备函数立即返回，pactl 替身收到 0 次调用 |
| R12 | 命令已修复但有回归 | 使用位置参数并检查退出状态；开始读取标题后激活 F1 的错误分类 |

## 本轮验证及限制

测试基于当前源码。对比探针分别编译 `fa0ba40` 的原始模块和当前原始模块；蓝牙变更由 pactl 替身模拟，剪贴板操作在独立 Xvfb 内完成。没有修改实际蓝牙 profile，也没有向用户桌面发送测试按键。

| 验证 | 结果 |
| --- | --- |
| `env -u DISPLAY -u WAYLAND_DISPLAY cargo test --locked --offline --quiet` | 117 项通过。第一次受限运行有 1 项因回环端口监听被沙箱拒绝；经自动审批允许本地测试后复跑通过，未计为代码缺陷 |
| `cargo clippy --locked --offline --quiet` | 退出码 0，仍有存量风格及未使用字段警告 |
| `git diff --check fa0ba40..e6cbaf5` | 通过 |
| 音频准备与恢复探针 | 10 个当前场景完成；其中大输出、变更命令超时、恢复失败后再次录音，另与修复前版本对比 |
| hook 探针 | 后台进程保持管道及超时后副作用两种场景均复现；测试进程组已清理 |
| 剪贴板探针 | 8 个场景各运行修复前后两版，共 16 次；覆盖标题分类、工具缺失、保留及错误恢复、初始化重试和首次复制 |

探针源码及命令替身位于 [本轮音频探针](/tmp/byetype-review-e6cbaf5/audio_probe.rs)、[音频场景运行器](/tmp/byetype-review-e6cbaf5/run_audio.py)、[pactl 替身](/tmp/byetype-review-e6cbaf5/pactl)、[剪贴板探针生成器](/tmp/byetype-review-e6cbaf5/prepare_clipboard.py) 和 [剪贴板场景运行器](/tmp/byetype-review-e6cbaf5/run_clipboard.py)。它们未加入产品测试集，临时路径可能被系统清理；报告正文保留了输入条件与观察结果。

本轮没有前端源代码变化，因此未重复前端构建。真实蓝牙硬件、Fcitx 候选窗与具体桌面应用的端到端粘贴、HiDPI、多屏、截图 OCR 服务调用、安装包和跨平台运行仍未验收。上述边界不影响已经由原始模块与对比测试确认的缺陷。

## 首轮审查记录

以下保留截至 `e456dd4` 的历史证据与建议，不应将其中 12 项全部理解为当前仍未修复。首轮定位属于旧提交；判断当前状态以上方复审核验表为准。

首轮审查日期为 2026 年 10 月 1 日。范围为 `ccbc73aebed588a719684fafc47b3bbbeb8ef9c2`（上游 v1.22.4）到 `e456dd42962857c11e76db30af9513f0b402ebf0`，共 15 个提交、20 个文件。重点覆盖 X11 初始化、输入法、粘贴、截图、窗口定位、蓝牙录音、配置及 Linux CI 和打包。首轮仅新增报告，未修改产品代码。

## 首轮审查意见

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

位置：`e456dd4:src-tauri/src/audio_switch.rs:182`，相关解析见 `e456dd4:src-tauri/src/audio_switch.rs:90`。

`run_pactl()` 继承桌面语言环境，而 `parse_cards()` 只识别 `Card #`、`Name:`、`Profiles:`、`Active Profile:` 等英文字段；profile 解析也依赖英文的 `sources:` 和 `available: yes`。中文桌面输出的是“卡”“名称”“配置文件”“活动配置”等字段，因此自动切换会把已连接设备当成不存在并静默跳过。

本机使用同一组声卡，以 `LC_ALL=C LANGUAGE=C` 和 `LC_ALL=zh_CN.UTF-8 LANGUAGE=zh_CN` 分别执行 `pactl list cards`，将结果交给原始 `parse_cards()`：前者得到 **2 张声卡**，后者得到 **0 张声卡**。PulseAudio 的输出字段确实经过本地化，见 [pactl 15.99.1 源码](https://github.com/pulseaudio/pulseaudio/blob/v15.99.1/src/utils/pactl.c#L1137)。

建议：对子进程明确设置 `LC_ALL=C`，并清理或固定 `LANGUAGE`；也可在支持版本范围内改用 `pactl --format=json`。验收需覆盖中文会话，确认其设备识别结果与英文环境一致。

### R2 P1 兼容 PipeWire 的蓝牙输入命名

位置：`e456dd4:src-tauri/src/audio_switch.rs:75`，匹配调用见 `e456dd4:src-tauri/src/audio_switch.rs:241`。

`source_prefix()` 将 `bluez_card.<地址>` 固定转换成 `bluez_source.<地址>`。PipeWire 配合 WirePlumber 使用的输入节点名通常为 `bluez_input.<地址>.<编号>`；WirePlumber 0.4.17 的[节点创建代码](https://github.com/PipeWire/wireplumber/blob/0.4.17/src/scripts/monitors/bluez.lua#L255)明确采用这一命名。于是即使 HFP 麦克风已出现，轮询仍匹配不到它，最终恢复 A2DP，无法完成蓝牙录音准备。

模拟 `pactl` 返回可用的 `bluez_input.11_22_33_44_55_66.0` 后，直接调用原始 `prepare_bluetooth_recording()`，结果为 `None`，约 1.69 秒后打印 `HFP source not ready in 1500ms, rolling back`。同一测试换成 `bluez_source.*` 则成功。这是命名匹配验证，未在真实 PipeWire 蓝牙硬件上录音。

建议：优先通过 source 的声卡索引或设备属性关联声卡，避免推导固定名称；最小修复至少同时支持 PulseAudio 和 PipeWire 命名，并排除输出 monitor。为两种音频服务各保留一份真实输出 fixture。

### R3 P2 录音启动失败也必须执行收尾

位置：`e456dd4:src-tauri/src/shortcut.rs:274`，遗漏分支见 `e456dd4:src-tauri/src/shortcut.rs:363`。

自动切换和 `pre_record_hook` 已在 `recorder.start(&mic)` 之前执行，但 `start()` 的错误分支仅取消任务并发送错误事件，没有调用 `finish_recording()`。`task::cancel_recording()` 也只清理气泡与任务计数。设备被独占、音频格式不支持、打开音频流失败等情况下，耳机会继续停在 HFP，默认输入设备也不会立即恢复，自定义 post hook 则完全遗漏。

这是静态控制流确认：录音器存在多个可返回错误的设备初始化步骤，错误出口没有音频恢复调用；没有通过改变用户真实麦克风来触发它。建议在启动失败分支执行与停止失败相同的收尾，或使用录音会话守卫统一管理。验收应在蓝牙切换成功后注入 `recorder.start()` 失败，确认 profile、默认输入和 post hook 均被恢复或执行。

### R4 P2 已处于 HFP 时仍须正确选择录音来源

位置：`e456dd4:src-tauri/src/audio_switch.rs:209`。

当蓝牙声卡已经处于含输入通道的 profile 时，函数直接返回 source 名，没有保存原默认输入，也没有执行 `set-default-source`。返回值在快捷键调用方只用于日志，不参与 `recorder.start(&mic)` 的设备选择。因此在使用 `system-default`、耳机已处于 HFP、系统默认输入仍为内置麦克风的场景下，日志声称使用蓝牙，实际仍录制内置麦克风。

模拟验证中，准备结果为 `Some(bluez_source...)`，但默认输入始终是 `alsa_input.internal`，整个准备过程只有两次查询。建议将“是否需要切换 profile”和“是否需要切换默认输入”分开处理，即使无需切 profile，也保存并设置默认输入，结束后恢复。

### R5 P2 不要把声卡查询失败当作声卡消失

位置：`e456dd4:src-tauri/src/audio_switch.rs:275`。

`run_pactl(["list", "cards", "short"])` 失败时，`.unwrap_or(true)` 将 `card_gone` 设为真，随后清空会话快照。音频服务短暂不可达或查询返回非零状态并不能证明耳机已断开；这里会丢失原 profile 和默认输入，后续停止或退出也无法重试恢复。

故障注入让第一次恢复查询失败、之后允许正常查询。连续调用两次 `restore_session()` 后，profile 仍为 HFP，默认输入仍为蓝牙；第二次调用未再执行任何 `pactl` 命令。建议仅在查询成功且明确找不到声卡时清理快照，查询失败则保留会话并记录错误。

### R6 P2 就绪超时后的回滚失败必须保留快照

位置：`e456dd4:src-tauri/src/audio_switch.rs:246` 和 `e456dd4:src-tauri/src/audio_switch.rs:251`。

会话快照只在找到输入 source 后才存入 `SESSION`。若切换 HFP 已成功、source 迟迟未出现，超时路径仅尝试一次恢复 profile，忽略命令结果并返回。此时回滚一旦失败，快照随栈变量销毁，正常收尾及退出恢复都变成空操作。

模拟“切到 HFP 成功、source 不出现、恢复 A2DP 失败”后，准备函数返回 `None`；再调用两次恢复，设备仍处于 HFP，且没有新的恢复命令。建议在系统状态首次改变前后立即建立可恢复会话，并在回滚成功前保留快照；超时和正常收尾使用同一恢复路径。

### R7 P2 为 pactl 和录音钩子设置执行超时

位置：`e456dd4:src-tauri/src/audio_switch.rs:182`，直接命令调用还见 `e456dd4:src-tauri/src/audio_switch.rs:226`、`e456dd4:src-tauri/src/audio_switch.rs:350`。

所有 `pactl` 调用和 shell hook 都使用同步 `.output()`，没有进程执行超时。1.5 秒 deadline 只在一次 `list sources` 命令返回后检查，且声卡发现和 profile 切换发生在 deadline 建立之前。因此音频服务或 hook 阻塞时，所谓“超时降级、不阻断录音”并不成立。

模拟单次 source 查询耗时 2.2 秒，原始准备函数耗时约 **2.42 秒**，仍以成功返回而未触发超时。调用来自全局快捷键处理器，因此长期阻塞会阻止该快捷键事件线程继续处理开始、停止或松键事件；这里不能据此断言整个 GTK 界面必然冻结。

建议统一实现带总时限的命令执行器，超时后终止并回收子进程，进入可重试的恢复路径。自定义 hook 也需要有界执行，并处理 shell 子进程残留。验收包括慢查询、无响应命令及超时后的下一次快捷键操作。

### R8 P2 粘贴失败后仍应恢复原剪贴板

位置：`e456dd4:src-tauri/src/clipboard.rs:465`。

Linux 分支在公共恢复逻辑之前执行 `paste_result?`。只要模拟粘贴返回错误，函数就提前返回，跳过 `restore_clipboard(backup)`，破坏“关闭覆盖剪贴板后保留原内容”的行为。macOS 和 Windows 的错误被外层 `paste_result` 收集，新增 Linux 分支没有接入这一结构。

在隔离 Xvfb 中保留有效 clipboard owner，先写入 `ORIGINAL_CLIPBOARD`，隐藏 `xdotool` 后调用原始 `paste_text("VOICE_RESULT", false)`：函数返回启动命令失败，剪贴板却保留 `VOICE_RESULT`。保留 owner 是为排除 R10 的生命周期问题。

建议将 Linux 粘贴结果纳入公共错误收集，先执行输入法和剪贴板恢复，再返回错误。验收需要同时断言错误返回值与原剪贴板内容。

### R9 P2 缺失 xdotool 时执行既定降级

位置：`e456dd4:src-tauri/src/clipboard.rs:189`。

enigo 降级只覆盖“成功启动 xdotool，但其退出状态非零”。命令不存在或不可执行时，`.output().map_err(...)?` 直接返回错误。文档将 xdotool 描述为仅影响定位的可选依赖，因此按文档使用源码构建或 AppImage、又没有安装 xdotool 的用户，会失去全部自动粘贴功能。

Xvfb 中仅从子进程 PATH 隐藏 xdotool，原始粘贴函数确定返回 `Failed to run xdotool: No such file or directory`，没有执行 enigo。建议将启动失败也纳入降级路径，并同步纠正文档中的用途和必需性说明。此项与 R8 是两个独立错误：修复降级不能保证其他粘贴错误时仍能恢复剪贴板。

### R10 P2 为 X11 剪贴板保留有效 owner

位置：`e456dd4:src-tauri/src/clipboard.rs:437`，相关通用恢复 helper 见 `e456dd4:src-tauri/src/clipboard.rs:36`，新增打包依赖见 `e456dd4:src-tauri/tauri.conf.json:72`。

Linux 使用 arboard 写入后，需要有有效的剪贴板所有者持续提供内容；最后一个 `Clipboard` 实例释放时，其他应用可能无法再读取，见 [arboard 3.6.1 的 Linux 行为说明](https://docs.rs/arboard/3.6.1/arboard/struct.Clipboard.html#linux)。当前仅成功执行 xclip 的路径有外部进程接管，而 deb 依赖和运行说明均未列出 xclip。缺失 xclip 时声称使用 arboard 降级，但局部实例在函数返回时释放。

在没有剪贴板管理器的 Xvfb 中验证了两种情况：隐藏 xclip、让按键命令成功，`paste_text(..., true)` 返回 `Ok(())`，随后读取为 `ContentNotAvailable`；即使 xclip 存在，调用 `paste_text(..., false)` 恢复原内容后，局部 arboard owner 释放，原剪贴板也变成不可用。因此只添加 xclip 依赖不能完整修复保留模式。

建议建立应用生命周期内的 Linux 剪贴板 owner，将写入和恢复交给同一服务；如果继续依赖 xclip，补齐打包及运行文档，并明确支持范围。这里包含沿用的通用 helper 在 Linux 上的适配遗漏，不应误称为本次新增了 `restore_clipboard()`。验收必须在没有剪贴板管理器的会话中，于函数返回后从另一实例或进程读取。

### R11 P2 尊重用户明确选择的非蓝牙麦克风

位置：`e456dd4:src-tauri/src/shortcut.rs:274`，设备选择见 `e456dd4:src-tauri/src/audio/mod.rs:42`。

只要自动切换开关为默认的 `auto`，开始任何录音都会寻找蓝牙声卡并修改其 profile 和系统默认输入，没有检查已经读取的 `config.general.microphone`。当用户明确选择 USB 或有线麦克风，同时连接蓝牙耳机听音频时，录音器仍会按名称使用选定麦克风，但耳机却被无必要地降到 HFP，增加启动等待并改变系统音频状态。这也不符合运行文档中非蓝牙麦克风“不经过此流程”的描述。

此项由配置与设备选择调用链静态确认，未更改真实系统默认输入。建议让自动切换策略接收实际录音设备选择；明确选择非蓝牙设备时跳过，默认设备场景再确定需要使用的蓝牙设备。验收需覆盖“蓝牙耳机播放加 USB 麦克风录音”。

### R12 P3 修正获取窗口标题的命令参数

位置：`e456dd4:src-tauri/src/clipboard.rs:271`。

代码执行 `xdotool getwindowname --window <wid>`，但该子命令接受的位置参数是 `getwindowname [window]`。本机在 Xvfb 下执行代码中的参数形式，确定返回 `getwindowname: unrecognized option '--window'`，退出码为 1。调用方还忽略了退出状态，所以窗口标题实际上没有参与粘贴策略识别。

已有 WM_CLASS 分类仍可工作，因此此项影响主要是依赖标题的回退场景。建议改为 `getwindowname <wid>`，先确认退出成功再读取 stdout，并增加一次真实命令调用测试，避免只有分类纯函数测试通过而命令接口错误。

## 首轮验证及限制

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
