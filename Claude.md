# AutoContinue (AC) - 项目说明

## 项目概述

AutoContinue (AC) 是一个使用 Rust 开发的 CLI 工具包装器，用于自动继续/重试 AI CLI 工具（如 Claude Code、Codex、Gemini、OpenCode 等）的运行。

## 核心功能

1. **自动继续**: CLI 静默超时后自动发送继续提示词
2. **错误检测**: 通过红色文本检测错误状态
3. **自动重试**: 检测到错误时自动发送重试提示词
4. **完整交互性**: 使用 PTY 保持 CLI 的完整功能
5. **鼠标支持**: 支持鼠标事件转发

## 使用方法

```bash
ac <CLI程序> [CLI参数...] [AC参数...]
```

### 示例

```bash
# 基本使用
ac claude --resume -cp "继续迭代" -rp "重试"

# AC参数可以放在任意位置
ac -cp "继续" claude --resume

# 使用文件提示词
ac claude -cpio prompt.md
```

### AC 参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `-cp, --continue-prompt` | 继续提示词 | "继续" |
| `-cpf, --continue-prompt-file` | 继续提示词文件 | - |
| `-cpio, --continue-prompt-io` | 继续提示词IO文件（动态读取） | - |
| `-rp, --retry-prompt` | 重试提示词 | "重试" |
| `-rpf, --retry-prompt-file` | 重试提示词文件 | - |
| `-rpio, --retry-prompt-io` | 重试提示词IO文件（动态读取） | - |
| `-st, --sleep-time` | 额外等待时间（秒） | 15 |
| `-sth, --silence-threshold` | 静默阈值（秒） | 30 |
| `-h, --help` | 显示帮助 | - |
| `-v, --version` | 显示版本 | - |

## 项目结构

```
src/
├── main.rs      # 程序入口、主循环
├── args.rs      # 命令行参数解析（clap）
├── config.rs    # 配置管理、提示词加载
├── runner.rs    # CLI运行器（PTY、IO转发、鼠标支持）
├── monitor.rs   # 状态监控、Ctrl+C处理
└── terminal.rs  # 虚拟终端、ANSI解析、颜色检测
```

## 技术实现

### PTY（伪终端）

使用 `portable-pty` 库实现跨平台伪终端支持：
- Windows: ConPTY
- Linux/macOS: 传统 PTY

### 双向 IO 转发

- **输出线程**: PTY → stdout + 虚拟终端处理
- **输入线程**: stdin → PTY（支持键盘和鼠标事件）

### 错误检测

通过虚拟终端追踪屏幕内容：
- 解析 ANSI 转义序列
- 统计红色字符数量
- 超过 50 个红色字符判定为错误

### 鼠标支持

使用 SGR 扩展鼠标编码格式转发鼠标事件。

## 开发准则

1. **代码注释**: 每个函数都有详细的中文注释
2. **错误处理**: 使用 `anyhow` 进行错误处理
3. **测试**: 每个模块都有单元测试
4. **Git 规范**: 每个功能完成后提交，提交信息使用中文

## Git 信息

- Email: MoYeRanQianZhi@gmail.com
- Name: MoYeRanQianZhi

## 测试命令

```bash
# 运行所有测试
cargo test

# 构建发布版本
cargo build --release

# 实机测试
ac claude -cp "继续输出" -rp "重试"
```

## 注意事项

1. AC 不会影响 CLI 的正常功能
2. 用户输入会重置静默计时器
3. Ctrl+C 可优雅退出
4. 支持中文和 Unicode 字符
