# PR 草稿：Linux (X11) 移植支持

> 状态：**WIP** —— 内部复审尚有 7 项待处理（1 P1 + 6 P2，见文末），修复完成并真机验收后转正式 PR。

---

**标题**：`feat(linux): Linux (X11) 移植支持：录音转写、粘贴、截图、蓝牙麦克风切换`

## 概述

在 macOS/Windows 之外新增 Linux（X11 会话，Ubuntu 22.04+）平台支持，语音输入全流程可用：全局快捷键录音 → AI 转写 → 剪贴板写入 → 粘贴键注入。所有 Linux 代码以 `#[cfg(target_os = "linux")]` 隔离，对现有平台零改动。

Linux 音频栈与桌面协议的策略与 macOS/Windows 不同，部分能力需要应用层补偿（详见下节），因此本 PR 除功能外还包含：

- Linux CI（push/PR 触发：前端构建 + `cargo test` + clippy）
- Release 矩阵新增 ubuntu-22.04 构建（固定在最早支持版本上构建，产物兼容 22.04/24.04/26.04）
- 20 个单元测试覆盖平台特有逻辑（纯函数抽取 + 样本测试）

## 平台差异说明（为什么这些代码是必要的）

| 能力 | macOS / Windows | Linux 现状 | 本 PR 的补偿 |
|---|---|---|---|
| 全局快捷键 | 系统 API | Wayland 协议不支持全局捕获；X11 可用（需 XInitThreads 修 x11rb/GTK 多线程冲突） | X11 支持，Wayland 列为受限 |
| 蓝牙录音 profile | 系统在应用录音时自动切 HFP | PulseAudio/PipeWire 把策略留给应用 | `bluetoothSwitch: auto` 自动暂存-切换-恢复 |
| 剪贴板 | 系统服务持有内容 | X11 需要持续 owner | 应用生命周期共享 arboard 实例 + xclip 双选区同步 |
| IME 粘贴冲突 | — | fcitx 候选窗口拦截模拟按键 | fcitx4/fcitx5 感知（粘贴前停用、后恢复） |
| 截图选区 | 系统工具 | 无统一方案 | maim (X11) / grim+slurp (Wayland) |

## 功能清单（按提交分组）

**平台基础**
- X11 多线程初始化（修 global-hotkey 与 GTK 的 XCB 崩溃）
- 默认语音快捷键 F8（F4 与 Fcitx4 切输入法冲突）

**输入与粘贴**
- fcitx4/fcitx5 输入法感知模块（stdout 状态解析，两代语义兼容）
- 剪贴板 CLIPBOARD + PRIMARY 双选区同步（xclip）
- 按焦点窗口 WM_CLASS 选择粘贴键序（终端 Ctrl+Shift+V / Vim GUI 寄存器粘贴 / 普通 Ctrl+V），xdotool 缺失时降级 enigo

**窗口与截图**
- 气泡/预览窗口 xdotool 光标定位（物理像素，与 Monitor API 同坐标系）
- WebKitGTK 透明窗口动画禁用
- 截图取词 maim / grim+slurp 分流

**音频**
- 蓝牙耳机录音 profile 自动切换（默认开启）：发现 bluez 声卡 → 暂存 profile 与默认输入 → 切 HFP（优先保音质的 a2dp_sink_hfp_hf）→ 轮询就绪 → 录音结束/取消/启动失败/应用退出统一幂等恢复
- 兼容 PulseAudio（bluez_source.*）与 PipeWire（bluez_input.*）命名；pactl 固定 LC_ALL=C 防本地化解析失败
- 用户明确选择非蓝牙麦克风时跳过切换
- 保留 preRecordHook/postRecordHook 自定义 shell 钩子（高级逃生舱，5s 超时）

**CI 与打包**
- ci-linux.yml；Release 矩阵 ubuntu-22.04 + updater latest.json 平台后缀
- deb 依赖：libxdo3 / xdotool / maim / xclip

## 已知限制（如实声明）

- **Wayland 原生会话**：全局快捷键不可用（tauri#3578），语音流程断裂；截图可用（grim）。建议 X11 会话使用。portal GlobalShortcuts 在 roadmap。
- **WebKitGTK 透明窗口残影**：状态切换偶有旧帧残留，第二次录音干净。
- **ALSA 独占**：cpal 走 ALSA 后端，可能独占录音设备。
- Ubuntu 20.04 不支持（Tauri v2 需要 webkit2gtk 4.1）。

## 测试

- `cargo test` 117 个全过（新增 20 个：pactl 解析、xdotool 解析、粘贴分类、fcitx 双代语义）
- clippy 对本 PR 触及文件零新增警告
- 本机（Ubuntu 22.04 / X11 / fcitx5 / PulseAudio）完成构建与基础流程验证
- 内部两轮代码审查 + 一轮复审，问题清单与修复记录见 `docs/ubuntu-x11-review.md`

## Review 指引

提交按功能分层（基础→模块→UI→CI/文档），可按组 review。建议重点：

1. `audio_switch.rs` — 会话快照的生命周期与恢复路径
2. `clipboard.rs` linux 模块 — 粘贴策略分类与共享剪贴板 owner
3. `shortcut.rs` — 与上游录音竞态修复的衔接（task_id 预分配不变量保持）

## Roadmap（不包含在本 PR）

- 粘贴按键策略用户可配置 / WM_CLASS 规则表开放扩展
- ibus 支持（当前仅 fcitx）
- Wayland：portal GlobalShortcuts、wtype/ydotool
- PipeWire 原生 API 替代 pactl

---

## 附：当前待处理问题（内部复审 F1-F7，修复后移除本节）

- F1 (P1) 窗口标题会覆盖 WM_CLASS 分类，浏览器标题含 "Neovide" 等词时误发 Ctrl+R 刷新
- F2 (P2) run_with_timeout 等待期间不排空管道，大输出命令被误判超时
- F3 (P2) hook 经 sh -c 启动，超时只杀 sh 不杀子进程组
- F4 (P2) set-card-profile 超时按"未生效"处理，实际可能已生效，丢恢复快照
- F5 (P2) 上次恢复失败保留的快照会被新一轮录音覆盖
- F6 (P2) 剪贴板共享实例首次初始化失败被永久缓存为 None
- F7 (P2) 截图/预览的复制路径未走共享 owner，首次复制可能无 owner
