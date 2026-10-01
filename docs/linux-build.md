# Linux 构建 / 运行说明（Fcitx4 感知移植）

ByeType 在 Linux 上以 X11 为主环境，针对 Fcitx4 做了"粘贴时临时停用 IME"的感知处理，避免拼音候选窗口与模拟 Ctrl+V 冲突。

## 系统依赖（构建时）

Debian/Ubuntu：

```bash
sudo apt install \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  libasound2-dev \
  libxdo-dev libxtst-dev libx11-dev \
  librsvg2-dev
```

- `libwebkit2gtk-4.1-dev` / `libgtk-3-dev`：Tauri Linux WebView 与窗口
- `libasound2-dev`：cpal 音频采集（ALSA 后端）
- `libxdo-dev` / `libxtst-dev` / `libx11-dev`：enigo 模拟按键（X11/XTest）

## 运行时依赖

| 工具 | 用途 | 必需 |
|---|---|---|
| `fcitx-remote` | 粘贴前停用/恢复 IME（fcitx4 自带；fcitx5 用 `fcitx5-remote`，已自动兼容） | 否（缺失时优雅降级，正常粘贴） |
| `xdotool` | 获取光标位置（bubble/preview 定位） | 否（缺失时回退到默认位置） |
| `maim` | X11 截图选区（F6） | 否（仅影响截图取词功能） |
| `grim` + `slurp` | Wayland 截图选区兜底 | 否（仅 Wayland 截图时需要） |

```bash
sudo apt install xdotool maim
# Wayland 用户：
sudo apt install grim slurp
```

## 构建

```bash
npm install
npm run tauri build      # 产出 src-tauri/target/release/bundle/ 下的 .deb 与 .AppImage
# 或开发模式：
npm run tauri dev
```

## Fcitx4 工作机制

ByeType 不是真正的输入法模块，而是"录音 → AI 转写 → 写剪贴板 → 模拟 Ctrl+V"。在 Linux + Fcitx4 下，粘贴前会：

1. `fcitx-remote` 检测 IME 是否激活（退出码 2 = 激活）
2. 若激活，执行 `fcitx-remote -c` 停用，等 30ms 让候选窗口关闭
3. 写剪贴板 + enigo 模拟 Ctrl+V
4. 等 150ms 让目标应用读完粘贴内容
5. `fcitx-remote -o` 恢复 IME 激活状态

无 fcitx / fcitx 未运行时，第 1 步返回 `NotInstalled`，跳过 2/5，直接走剪贴板 + 粘贴，不影响功能。

## 默认快捷键

Linux 上默认语音快捷键为 **F8**（不是 macOS/Windows 的 F4），因为 F4 是 Fcitx4 默认的"切换输入法"键。截图快捷键保持 F6。

## 已知限制

- **Wayland**：模拟按键（enigo）在原生 Wayland 下不可靠，仅 XWayland 下可用。截图分支已兜底 `grim`+`slurp`，但语音粘贴建议在 X11 会话使用。
- **HiDPI**：bubble 与 preview 在 Linux 上用 xdotool 光标（物理像素）+ Tauri Monitor API（物理像素）做贴光标定位，与 Windows 分支同构，HiDPI 多屏下应正常；若 xdotool 缺失则回退到 `center()`。
- **ALSA 独占**：cpal 默认走 ALSA，可能独占录音设备。若需与其他录音软件并行，后续可加 `pulseaudio` / `jack` feature。
- **Bubble 残影**：WebKitGTK 透明窗口不清除旧像素，状态切换时旧元素残影可能残留（如 "Thinking..." 文字被绿点覆盖后仍有残影）。第二次录音时干净。hide/show 可清除但闪烁明显，已禁用。

## 蓝牙耳机录音（profile 自动切换）

蓝牙耳机在 A2DP profile 下音质好但没有麦克风，HFP/HSP profile 下有麦克风但音质差。macOS/Windows 由系统在应用录音时自动切换 profile，Linux 则把策略留给应用，因此 ByeType 内置了自动切换：

**自动模式（默认，推荐）**：设置页 → 网络与性能 → 「自动切换蓝牙麦克风」保持开启即可，或编辑 config.json：

```json
{ "advanced": { "bluetoothSwitch": "auto" } }
```

录音时自动完成：发现蓝牙声卡 → 暂存当前 profile 与默认录音设备 → 切到录音 profile（优先保音质的 `a2dp_sink_hfp_hf`，其次 `handsfree_head_unit` / `headset_head_unit`）→ 轮询等录音设备就绪（上限 1.5 秒，超时自动还原、按原麦克风录音）→ 临时设为默认录音设备。录音结束（含取消/出错/退出应用）自动恢复原状。应用录音走 system-default 设备（cpal 在 ALSA 层，看不到 PipeWire 的蓝牙虚拟 source）。

无蓝牙耳机、未安装 `pactl`（`pulseaudio-utils` 包）时静默跳过，零开销。3.5mm/USB 麦克风是常驻设备，不经过此流程，在「麦克风」设置里直接选择即可。

**自定义钩子（高级）**：需要接管其他设备状态时，用 `preRecordHook` / `postRecordHook` 在录音前后执行任意 shell 命令（自动切换关闭时也可单独使用）：

```json
{
  "advanced": {
    "bluetoothSwitch": "off",
    "preRecordHook": "pactl set-card-profile bluez_card.88_92_CC_E7_A4_48 handsfree_head_unit && pactl set-default-source bluez_source.88_92_CC_E7_A4_48.handsfree_head_unit",
    "postRecordHook": "pactl set-default-source alsa_input.pci-0000_00_1f.3.analog-stereo && pactl set-card-profile bluez_card.88_92_CC_E7_A4_48 a2dp_sink"
  }
}
```

查找你的设备名：

```bash
pactl list cards short                                    # 找 bluez_card.XX
pactl list cards | grep -A5 'bluez' | grep Profiles        # 确认有 handsfree_head_unit
pactl list sources short | grep handsfree                  # 找 HFP source 名
```

hook 留空则不执行。非 Linux 平台自动忽略。
