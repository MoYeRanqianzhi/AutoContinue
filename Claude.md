# AutoContinue (AC) - 项目说明

## 项目概述

AutoContinue (AC) 是一个使用 Rust 开发的 CLI 工具包装器，用于自动继续/重试 AI CLI 工具（如 Claude Code、Codex、Gemini、OpenCode 等）的运行。

## 核心功能

1. **自动继续**: CLI 静默超时后自动发送继续提示词
2. **智能检测**: 通过 Detector 适配器精确检测 CLI 状态和错误
3. **自动重试**: 检测到错误时自动发送重试提示词
4. **中断钩子**: 通过 stop-hook/stop-when 在满足条件时自动停止
5. **完整交互性**: 使用 PTY 保持 CLI 的完整功能

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

# 使用中断钩子：5轮后停止
ac claude -sw "<round=5>"

# 使用中断钩子：遇到错误停止 + 30分钟后停止
ac claude -sw "<error>" -sw "<duration=1800>"

# 使用自定义中断钩子命令
ac claude -sh "python check_done.py"

# 指定时间点停止
ac claude -sw "<time=2026-04-28 18:00:00>"
```

### AC 参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `-cp, --continue-prompt` | 继续提示词 | "继续" |
| `-cpf, --continue-prompt-file` | 继续提示词文件 | - |
| `-cpio, --continue-prompt-io` | 继续提示词IO文件（动态读取） | - |
| `-cpp, --continue-prompt-pipe` | 继续提示词管道命令（每次执行） | - |
| `-rp, --retry-prompt` | 重试提示词 | "重试" |
| `-rpf, --retry-prompt-file` | 重试提示词文件 | - |
| `-rpio, --retry-prompt-io` | 重试提示词IO文件（动态读取） | - |
| `-rpp, --retry-prompt-pipe` | 重试提示词管道命令（每次执行） | - |
| `--cformat <前缀> <后缀>` | 继续管道输出格式提取（取最后一组匹配） | - |
| `--rformat <前缀> <后缀>` | 重试管道输出格式提取（取最后一组匹配） | - |
| `-st, --sleep-time` | 额外等待时间（秒） | 15 |
| `-sth, --silence-threshold` | 静默阈值（秒） | 30 |
| `-l, --limit` | 最大自动发送轮次（-1为无限制） | -1 |
| `-sh, --stop-hook` | 中断钩子命令（可多次指定） | - |
| `-sw, --stop-when` | 预设中断条件（可多次指定） | - |
| `-h, --help` | 显示帮助 | - |
| `-v, --version` | 显示版本 | - |

### stop-when 预设条件

`-sw` 支持以下预设条件格式（可多次指定组合使用）：

| 条件 | 说明 | 示例 |
|------|------|------|
| `<round=N>` | 自动发送达 N 轮后停止 | `-sw "<round=5>"` |
| `<error>` | 检测到错误时停止 | `-sw "<error>"` |
| `<time=DATETIME>` | 到达指定时间点后停止 | `-sw "<time=2026-04-28 18:00:00>"` |
| `<duration=SECONDS>` | 运行指定秒数后停止 | `-sw "<duration=3600>"` |

## 项目结构

```
src/
├── main.rs              # 程序入口、主循环
├── args.rs              # 命令行参数解析（clap）
├── config.rs            # 配置管理、提示词加载
├── runner.rs            # CLI运行器（PTY、IO转发）
├── monitor.rs           # 状态监控、Ctrl+C处理
├── hook.rs              # 中断钩子（stop-hook/stop-when 解析与管理）
└── detector/            # 状态/错误检测引擎
    ├── mod.rs           #   Detector trait + CliStatus 枚举
    ├── claude.rs        #   Claude Code 适配器（JSONL 会话文件监控）
    ├── codex.rs         #   Codex 适配器（JSONL 会话文件监控）
    └── generic.rs       #   通用适配器（输出文本模式匹配）
```

## 技术实现

### PTY（伪终端）

使用 `portable-pty` 库实现跨平台伪终端支持：
- Windows: ConPTY
- Linux/macOS: 传统 PTY

### 双向 IO 转发

- **输出线程**: PTY → stdout（直接转发，不解析）+ Detector 数据投喂
- **输入线程**: stdin → PTY（支持键盘事件）

### Detector 检测引擎

使用 trait 对象实现适配器模式，根据 CLI 名称自动选择：

| CLI 工具 | 适配器 | 检测机制 |
|----------|--------|---------|
| Claude Code | ClaudeDetector | 监控 `~/.claude/projects/` JSONL 会话文件 |
| Codex | CodexDetector | 监控 `~/.codex/sessions/` JSONL 会话文件 |
| 其他 | GenericDetector | 输出文本模式匹配 + 静默超时 |

#### CliStatus 状态枚举

- `Busy`: CLI 正在工作，不干预
- `Idle`: CLI 空闲等待输入，发送继续 prompt
- `Error`: 检测到错误，发送重试 prompt
- `Unknown`: 状态不明，fallback 到静默超时检测

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
