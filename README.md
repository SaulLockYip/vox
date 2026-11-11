# Vox

**无登录的语音输入工具** —— 用 Rust + Tauri v2 写的 macOS 桌面应用。

三种模式：**听写 / 翻译 / 问答（Ask Anything）**。模型层走你自己的接口
（`base_url` / `model` / `prompt` 全部在设置里可改），**没有账号、没有订阅**，
所有数据只留在本机。

---

## 快速开始

```bash
# 编译 + 打包 + ad-hoc 签名 + 安装到 /Applications/Vox.app
bash tools/package-macos.sh

# 只编译不安装
bash tools/package-macos.sh --no-install

# 跑测试
cargo test --workspace

# 命令行版（不需要 GUI）
cargo run -p vox-cli -- doctor      # 体检：配置/密钥/麦克风/权限
cargo run -p vox-cli -- dictation    # 录音（回车停止）→ 整理 → 插入光标
```

装好后启动 `Vox.app`（菜单栏图标常驻）。**第一次使用需要给「辅助功能」权限**：
系统设置 › 隐私与安全性 › 辅助功能 › 勾选 Vox，否则全局热键和 ⌘V 注入都用不了。

## 三种模式与热键

| 模式 | 开始热键 | 结束 | 结果去哪儿 |
|---|---|---|---|
| 听写 | `RightCtrl` | 再按一次 `RightCtrl` | 插入光标处 |
| 翻译 | `RightCtrl+RightShift` | 按 `RightCtrl` | 插入光标处 |
| 问答 | `RightCtrl+Space` | 按 `RightCtrl` | **由模型自己决定**（见下） |

- **结束热键统一是听写热键**：三种模式只用「开始热键」区分。
- `RightCtrl` 是其它组合的前缀，所以**先按 `RightCtrl` 再加按 `Space`** 才是问答。
- 录音期间浮条会显示在**主屏底部居中**；点它可以**直接结束录音**（Esc 同理）。

### 问答的投递方式由模型决定

模型在回答末尾给一行标记，Vox 照办：

| 标记 | 行为 |
|---|---|
| `[[deliver: reply]]` | 显示在浮窗面板里（**默认**：解释、建议、查询） |
| `[[deliver: insert]]` | 直接写进光标处（用户明确要「写进文档」时） |
| `[[deliver: replace]]` | 替换掉选中的内容 |
| `[[open: URL]]` | 打开网址/应用（**直接执行，不弹确认框**） |

模型还可以在回答里带上确认（「好的，正在为你打开…」），**Vox 不做事前确认弹窗** ——
只有涉及**内容确认 / 内容修改**时才需要交互。

### 问答能真的查实时数据

`general.agent_tools` 控制本地工具权限：`off` / `safe`（默认，只读白名单：
curl / date / open / grep / jq…）/ `all`。模型自己决定要不要调工具，比如
「明天北京天气怎么样」→ 它会 `curl wttr.in` → 答案里带真实数据。

## 配置与密钥

| 位置 | 内容 |
|---|---|
| `~/Library/Application Support/Vox/settings.toml` | 配置（模型、base_url、prompt、热键…） |
| `~/Library/Application Support/Vox/secrets.json` | 密钥（XChaCha20-Poly1305 加密，0600） |
| `~/Library/Application Support/Vox/vox.db` | 历史 + 个人词典（SQLite） |
| `~/Library/Application Support/Vox/tmp/` | 临时录音（WAV） |

密钥**只落在本地加密文件里，不进系统钥匙串**。用 `vox secret set stt|llm`（从标准输入读，
不留在 shell history）或设置页填。

改内置 prompt 之后，老配置里存的旧默认值会**自动迁移**到新版本；
你自己改过的 prompt **一律不动**。

## 目录结构

```
crates/
  core/       配置模型、错误类型、模式定义、跨进程锁（instance.rs）
  providers/  ASR + Anthropic 兼容 LLM（含 tool_use 多轮循环）
  audio/      cpal 采集、重采样、WAV 编码
  pipeline/   三种模式的编排、prompt 渲染、词典替换、本地工具执行
  storage/    SQLite：历史、词典
  macos/      热键(CGEventTap)、注入、辅助功能、CoreAudio（静音/蓝牙 profile）
  app/        门面 crate：把上面这些串起来
  cli/        命令行版（vox）
src-tauri/    Tauri v2 外壳：设置界面、托盘、浮条
ui/           前端（纯静态，不经 npm 构建）
tools/        开发工具（见下）
```

## 开发工具（`tools/`）

| 脚本 | 用途 |
|---|---|
| `package-macos.sh` | 编译 → 组装 `.app` → ad-hoc 签名 → 安装 |
| `fake-server.py` | 本地假 ASR/LLM，让全链路可**确定性复现**（真实录音不外传） |
| `post-key.py` | 注入按键事件，用来自动验证全局热键 |
| `audio-device.py` | 看/切默认音频设备（排查蓝牙耳机卡在 HFP） |

假服务器用法：

```bash
python3 tools/fake-server.py 8787 &
echo "今天天气不错" > /tmp/vox-fake/transcript.txt   # ASR 返回什么
echo "这是回答。"    > /tmp/vox-fake/answer.txt       # LLM 返回什么
cat /tmp/vox-fake/prompt.log                         # 每次发给模型的 prompt
```

测试时用 `VOX_CONFIG_DIR=/tmp/vox-test-cfg` 跑，**完全不碰真实配置**。

## 已知限制

- **ad-hoc 签名**：本机没有 Developer ID 证书，拷到别的 Mac 会被 Gatekeeper 拦。
- **问答的 `replace`** 目前等价于 insert（靠粘贴顺带替换选区），没做精确的选区写入。
- 还没做的：免提双击、合盖停止、富文本插入、词典自动学习、个性化、多语言 UI。
