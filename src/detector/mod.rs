//! # 检测引擎模块 (detector/)
//!
//! 该模块提供可扩展的 CLI 状态/错误检测引擎，替代原有的虚拟终端 ANSI 解析方案。
//!
//! ## 架构
//!
//! 使用 trait 对象实现适配器模式：
//! - `Detector` trait：定义统一的检测接口
//! - `JsonlDetector`：JSONL 通用检测器（通过配置适配 Claude Code 和 Codex）
//! - `GenericDetector`：通用适配器，基于输出文本模式匹配
//!
//! ## 模块组织
//!
//! - `jsonl.rs`：JSONL 通用检测器实现（`JsonlDetector` + `JsonlDetectorConfig`）
//! - `claude.rs`：Claude Code 工厂函数（返回配置好的 `JsonlDetector`）
//! - `codex.rs`：Codex 工厂函数（返回配置好的 `JsonlDetector`）
//! - `generic.rs`：通用文本模式匹配检测器
//!
//! ## 使用流程
//!
//! 1. `create_detector()` 根据 CLI 名称自动选择适配器
//! 2. `init()` 初始化检测器（启动文件监控等）
//! 3. `feed_output()` 持续接收 PTY 输出数据
//! 4. `status()` 查询当前 CLI 状态
//! 5. `reset()` 在发送 prompt 后重置状态

pub mod claude;
pub mod codex;
pub mod generic;
pub mod jsonl;

use anyhow::Result;
use std::fmt;
use std::time::Duration;

/// CLI 当前状态枚举
///
/// 表示被包装的 CLI 工具当前处于什么状态，
/// 主循环根据此状态决定是否发送 prompt。
#[derive(Debug, Clone)]
pub enum CliStatus {
    /// 正在运行/思考中，不应干预
    ///
    /// 当 JSONL 文件正在增长或最近有输出时返回此状态。
    Busy,

    /// 空闲等待用户输入
    ///
    /// 当 JSONL 检测到最后一条消息为 assistant 类型
    /// 且文件已停止增长时返回此状态。
    Idle,

    /// 发生了错误，需要发送重试 prompt
    ///
    /// 当检测到错误相关的消息内容或模式时返回此状态。
    Error {
        /// 错误信息摘要
        message: String,
    },

    /// 状态未知，应 fallback 到静默超时检测
    ///
    /// 当 JSONL 文件不可用或无法解析时返回此状态。
    /// 主循环将使用传统的静默超时逻辑处理。
    Unknown,
}

impl fmt::Display for CliStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliStatus::Busy => write!(f, "忙碌中"),
            CliStatus::Idle => write!(f, "空闲"),
            CliStatus::Error { message } => write!(f, "错误: {}", message),
            CliStatus::Unknown => write!(f, "未知"),
        }
    }
}

/// CLI 工具检测适配器 trait
///
/// 所有适配器都需要实现此 trait，提供统一的状态检测接口。
/// 适配器负责监控 CLI 工具的状态（通过 JSONL 文件、输出分析等），
/// 并在查询时返回当前状态。
///
/// ## 线程安全
///
/// 实现必须是 `Send`，因为检测器会在多线程环境中使用
/// （主线程查询状态，输出线程投喂数据）。
pub trait Detector: Send {
    /// 初始化检测器
    ///
    /// 在 CLI 进程启动后调用，执行以下操作：
    /// - 定位 CLI 工具的会话文件目录
    /// - 启动文件监控（如果适用）
    /// - 设置初始状态
    ///
    /// # 参数
    /// - `cli_name`: CLI 程序名称（如 "claude", "codex"）
    /// - `cli_args`: CLI 程序参数列表
    ///
    /// # 返回值
    /// 成功返回 Ok(())，初始化失败返回错误
    fn init(&mut self, cli_name: &str, cli_args: &[String]) -> Result<()>;

    /// 处理新的输出数据
    ///
    /// 由输出转发线程调用，将 PTY 读取到的原始字节传入检测器。
    /// 检测器可以选择分析这些数据（如 GenericDetector 的文本模式匹配），
    /// 也可以忽略它们（如 JsonlDetector 主要依赖 JSONL 文件）。
    ///
    /// # 参数
    /// - `data`: 从 PTY 读取的原始字节切片
    fn feed_output(&mut self, data: &[u8]);

    /// 查询当前 CLI 状态
    ///
    /// 主循环每 500ms 调用一次此方法，根据返回的状态决定行为：
    /// - `Busy`: 不干预，继续等待
    /// - `Idle`: 发送继续 prompt
    /// - `Error`: 发送重试 prompt
    /// - `Unknown`: fallback 到静默超时检测
    ///
    /// # 参数
    /// - `silence_duration`: 自上次 IO 活动以来的静默时间
    /// - `silence_threshold`: 用户配置的静默阈值
    ///
    /// # 返回值
    /// 当前 CLI 状态
    fn status(&self, silence_duration: Duration, silence_threshold: Duration) -> CliStatus;

    /// 重置检测状态
    ///
    /// 在发送 prompt 后调用，清除之前的检测结果，
    /// 准备检测下一轮输出。
    fn reset(&mut self);

    /// 返回检测器名称（用于日志输出）
    fn name(&self) -> &str;
}

/// 根据 CLI 名称创建对应的检测器
///
/// 自动识别 CLI 工具类型并创建最合适的适配器实例。
///
/// # 参数
/// - `cli_name`: CLI 程序名称
///
/// # 返回值
/// 返回对应的检测器 trait 对象
///
/// # 匹配规则
/// - 名称包含 "claude" → 使用 Claude 配置的 `JsonlDetector`
/// - 名称包含 "codex" → 使用 Codex 配置的 `JsonlDetector`
/// - 其他 → `GenericDetector`
pub fn create_detector(cli_name: &str) -> Box<dyn Detector> {
    let name_lower = cli_name.to_lowercase();

    if name_lower.contains("claude") {
        Box::new(claude::create_claude_detector())
    } else if name_lower.contains("codex") {
        Box::new(codex::create_codex_detector())
    } else {
        Box::new(generic::GenericDetector::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 CliStatus 的 Display 实现
    #[test]
    fn test_cli_status_display() {
        assert_eq!(format!("{}", CliStatus::Busy), "忙碌中");
        assert_eq!(format!("{}", CliStatus::Idle), "空闲");
        assert_eq!(
            format!("{}", CliStatus::Error { message: "连接超时".to_string() }),
            "错误: 连接超时"
        );
        assert_eq!(format!("{}", CliStatus::Unknown), "未知");
    }

    /// 测试检测器工厂函数的匹配逻辑
    #[test]
    fn test_create_detector_matching() {
        // Claude Code 匹配
        let d = create_detector("claude");
        assert_eq!(d.name(), "ClaudeDetector");

        // 大小写不敏感
        let d = create_detector("Claude");
        assert_eq!(d.name(), "ClaudeDetector");

        // 路径中包含 claude
        let d = create_detector("/usr/bin/claude");
        assert_eq!(d.name(), "ClaudeDetector");

        // Codex 匹配
        let d = create_detector("codex");
        assert_eq!(d.name(), "CodexDetector");

        // 通用 fallback
        let d = create_detector("gemini");
        assert_eq!(d.name(), "GenericDetector");

        let d = create_detector("opencode");
        assert_eq!(d.name(), "GenericDetector");
    }
}
