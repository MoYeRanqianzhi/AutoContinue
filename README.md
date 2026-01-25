# AutoContinue (AC)

**自动继续/重试 CLI 工具的智能包装器**

AutoContinue 是一个 Rust 编写的命令行工具，用于包装 Claude Code、Codex、Gemini、OpenCode 等 AI CLI 工具，实现自动继续和错误重试功能。

## 功能特性

- **自动继续**: 检测 CLI 静默状态，超时后自动发送继续提示词
- **错误检测**: 通过分析终端输出中的红色文本自动识别错误
- **自动重试**: 检测到错误时自动发送重试提示词
- **完整交互性**: 保持 CLI 的完整功能，用户可正常操作
- **鼠标支持**: 支持鼠标事件转发，兼容 OpenCode 等需要鼠标的 CLI
- **跨平台**: 支持 Windows、Linux、macOS
- **动态提示词**: 支持从文件读取提示词，可在运行时修改

---

## 安装

### 🤖 方式一：使用 AI 助手自动安装（推荐）

只需将以下内容复制给任意 AI 助手（如 Claude Code、Cursor、Copilot 等）：

```
Install and configure AutoContinue by following the instructions here:
https://raw.githubusercontent.com/MoYeRanQianZhi/AutoContinue/master/install-by-agent.md
```

AI 助手会自动完成安装过程。

---

### 🔧 方式二：手动安装

#### 前置要求

- Git
- [Rust](https://rustup.rs/) 1.70+

#### 从源码编译

```bash
# 克隆仓库
git clone https://github.com/MoYeRanQianZhi/AutoContinue.git
cd AutoContinue

# 编译发布版本
cargo build --release

# 二进制文件位于 target/release/ac (Linux/macOS) 或 target/release/ac.exe (Windows)
```

#### 添加到系统 PATH

**Windows (PowerShell):**
```powershell
Copy-Item target\release\ac.exe $env:USERPROFILE\.cargo\bin\
```

**Linux/macOS:**
```bash
cp target/release/ac ~/.cargo/bin/
# 或
sudo cp target/release/ac /usr/local/bin/
```

#### 使用 Cargo 安装（可选）

```bash
cargo install --path .
```

#### 验证安装

```bash
ac --version
```

---

## 使用方法

### 基本语法

```bash
ac <CLI程序> [CLI参数...] [AC参数...]
```

AC 参数和 CLI 参数可以混合使用，顺序不限。

### 快速开始

```bash
# 最简单的使用方式（使用默认提示词）
ac claude

# 带 CLI 参数
ac claude --resume

# 自定义继续提示词
ac claude --resume -cp "请继续完成任务"

# 自定义继续和重试提示词
ac claude --resume -cp "继续" -rp "重试上一步"

# 调整等待时间
ac claude -st 30 -sth 60
```

### AC 参数说明

| 参数 | 长参数 | 说明 | 默认值 |
|------|--------|------|--------|
| `-cp` | `--continue-prompt` | 继续提示词 | "继续" |
| `-cpf` | `--continue-prompt-file` | 继续提示词文件（启动时读取一次） | - |
| `-cpio` | `--continue-prompt-io` | 继续提示词IO文件（每次使用时重新读取） | - |
| `-rp` | `--retry-prompt` | 重试提示词 | "重试" |
| `-rpf` | `--retry-prompt-file` | 重试提示词文件（启动时读取一次） | - |
| `-rpio` | `--retry-prompt-io` | 重试提示词IO文件（每次使用时重新读取） | - |
| `-st` | `--sleep-time` | 额外等待时间（秒） | 15 |
| `-sth` | `--silence-threshold` | 静默阈值（秒） | 30 |
| `-h` | `--help` | 显示帮助信息 | - |
| `-v` | `--version` | 显示版本信息 | - |

### 时间参数说明

- **静默阈值 (`-sth`)**: CLI 无输入/输出超过此时间后开始计时
- **额外等待时间 (`-st`)**: 给用户自主回复的缓冲时间
- **总等待时间** = 静默阈值 + 额外等待时间

例如：默认设置下，CLI 静默 45 秒（30+15）后才会自动发送提示词。

### 使用示例

#### 示例 1：基础使用

```bash
ac claude --resume -cp "继续迭代，不断优化代码"
```

#### 示例 2：使用文件提示词

```bash
# 创建提示词文件
echo "请继续完成上一步的任务，保持代码风格一致" > continue.txt

# 使用文件提示词
ac claude --resume -cpf continue.txt
```

#### 示例 3：动态提示词（IO模式）

```bash
# 使用 IO 模式，可以在运行时修改提示词
ac claude --resume -cpio continue.txt

# 在另一个终端修改提示词，下次触发时会使用新内容
echo "新的提示词内容" > continue.txt
```

#### 示例 4：配合其他 AI CLI

```bash
# Codex
ac codex -cp "continue"

# OpenCode（支持鼠标）
ac opencode -cp "继续"

# Gemini
ac gemini -cp "请继续"
```

#### 示例 5：调整超时时间

```bash
# 长任务：增加等待时间
ac claude --resume -sth 60 -st 30

# 快速响应：减少等待时间
ac claude --resume -sth 15 -st 5
```

## 工作原理

1. **启动**: AC 使用伪终端（PTY）启动目标 CLI，保持完整的终端交互能力
2. **监控**: 实时监控 CLI 的输入/输出活动
3. **检测静默**: 当 CLI 无输入/输出超过阈值时，开始计时
4. **错误检测**: 分析终端输出，检测红色文本（通常表示错误）
5. **自动响应**:
   - 检测到错误 → 发送重试提示词
   - 正常静默 → 发送继续提示词
6. **用户优先**: 任何用户输入都会重置计时器，不会打断用户操作

## 错误检测机制

AC 通过分析终端输出中的红色文本来检测错误：
- 统计屏幕中红色字符数量（忽略底部 3 行状态栏）
- 红色字符超过 50 个判定为错误状态
- 支持 ANSI 标准红色和 RGB 红色（如 Claude Code 的错误色）

## 退出方式

- 按 `Ctrl+C` 优雅退出
- CLI 进程自行退出时 AC 也会退出

## 常见问题

### Q: 为什么我的输入没有响应？

A: 确保 CLI 程序支持标准输入。某些 CLI 可能需要特定参数才能接受输入。

### Q: 如何在 Windows 上使用？

A: AC 完全支持 Windows，使用 ConPTY 技术。直接运行即可。

### Q: 自动发送的提示词会打断我的输入吗？

A: 不会。任何用户输入/输出都会重置静默计时器，AC 只在真正静默时才会发送提示词。

### Q: 支持中文提示词吗？

A: 完全支持中文和其他 Unicode 字符。

## 开发

### 项目结构

```
AutoContinue/
├── Cargo.toml          # 项目配置
├── README.md           # 本文档
├── CLAUDE.md           # Claude Code 项目说明
├── src/
│   ├── main.rs         # 程序入口和主循环
│   ├── args.rs         # 命令行参数解析
│   ├── config.rs       # 配置管理
│   ├── runner.rs       # CLI 运行器（PTY管理、IO转发）
│   ├── monitor.rs      # 状态监控
│   └── terminal.rs     # 虚拟终端（ANSI解析、颜色检测）
```

### 运行测试

```bash
cargo test
```

### 构建发布版本

```bash
cargo build --release
```

## 许可证

MIT License

## 作者

MoYeRanQianZhi <MoYeRanQianZhi@gmail.com>
