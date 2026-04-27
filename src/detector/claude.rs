//! # Claude Code 检测适配器工厂 (detector/claude.rs)
//!
//! 提供创建 Claude Code 专用 JSONL 检测器的工厂函数。
//!
//! ## Claude Code 会话文件位置
//!
//! Claude Code 将会话消息写入以下路径：
//! `~/.claude/projects/{projectHash}/{sessionId}.jsonl`
//!
//! ## 消息类型
//!
//! - `user`: 用户输入的消息
//! - `assistant`: Claude 的回复
//! - `system`: 系统消息
//! - `summary`: 会话摘要（被识别为 Idle 状态）
//!
//! ## 与 Codex 的差异
//!
//! - 会话目录: `~/.claude/projects/`
//! - 不递归扫描子目录（仅扫描项目子目录的第一层）
//! - 识别 `summary` 消息类型为 Idle
//! - 使用 Claude 特有的错误关键词列表

use std::path::PathBuf;

use super::jsonl::{JsonlDetector, JsonlDetectorConfig};

/// 创建 Claude Code 专用的 JSONL 检测器
///
/// 配置 Claude Code 特有的行为：
/// - 会话目录: `~/.claude/projects/`
/// - 非递归扫描（每个项目子目录只扫描第一层）
/// - 识别 summary 消息类型为 Idle
/// - 使用 Claude 特有的错误关键词
///
/// # 返回值
/// 配置好的 `JsonlDetector` 实例
pub fn create_claude_detector() -> JsonlDetector {
    // 获取 Claude 项目目录路径
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let projects_dir = home_dir.join(".claude").join("projects");

    let config = JsonlDetectorConfig {
        name: "ClaudeDetector".to_string(),
        sessions_dir: projects_dir,
        recursive_scan: false,
        max_scan_depth: 5,
        error_keywords: vec![
            "error occurred",
            "failed to",
            "i encountered an error",
            "an error happened",
            "i'm unable to",
            "cannot complete",
            "command failed",
            "compilation error",
            "build failed",
        ],
        recognize_summary: true,
    };

    JsonlDetector::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{CliStatus, Detector};
    use std::time::Duration;

    /// 测试 Claude 检测器工厂函数创建的检测器名称正确
    #[test]
    fn test_claude_detector_name() {
        let detector = create_claude_detector();
        assert_eq!(detector.name(), "ClaudeDetector");
    }

    /// 测试 Claude 检测器在没有会话文件时返回 Unknown
    #[test]
    fn test_no_session_file_returns_unknown() {
        let detector = create_claude_detector();
        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Unknown => {} // 预期结果
            _ => panic!("没有会话文件时应返回 Unknown"),
        }
    }
}
