//! # Codex 检测适配器工厂 (detector/codex.rs)
//!
//! 提供创建 Codex 专用 JSONL 检测器的工厂函数。
//!
//! ## Codex 会话文件位置
//!
//! Codex 将会话记录写入以下路径：
//! `~/.codex/sessions/{subdir}/{prefix}-{sessionId}.jsonl`
//!
//! 环境变量 `CODEX_HOME` 可覆盖默认的 `~/.codex` 路径。
//!
//! ## 与 Claude 的差异
//!
//! - 会话目录: `~/.codex/sessions/`（支持 `CODEX_HOME` 环境变量）
//! - 递归扫描子目录（最大深度 5 层）
//! - 不识别 `summary` 消息类型
//! - 使用 Codex 特有的错误关键词列表（包含 "runtime error", "exit code"）

use std::path::PathBuf;

use super::jsonl::{JsonlDetector, JsonlDetectorConfig};

/// 创建 Codex 专用的 JSONL 检测器
///
/// 配置 Codex 特有的行为：
/// - 会话目录: `~/.codex/sessions/`（优先使用 `CODEX_HOME` 环境变量）
/// - 递归扫描子目录（最大深度 5 层）
/// - 不识别 summary 消息类型
/// - 使用 Codex 特有的错误关键词
///
/// # 返回值
/// 配置好的 `JsonlDetector` 实例
pub fn create_codex_detector() -> JsonlDetector {
    // 优先使用 CODEX_HOME 环境变量，否则使用 ~/.codex
    let codex_home = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            home.join(".codex")
        });

    let sessions_dir = codex_home.join("sessions");

    let config = JsonlDetectorConfig {
        name: "CodexDetector".to_string(),
        sessions_dir,
        recursive_scan: true,
        max_scan_depth: 5,
        error_keywords: vec![
            "error occurred",
            "failed to",
            "command failed",
            "compilation error",
            "build failed",
            "runtime error",
            "exit code",
        ],
        recognize_summary: false,
    };

    JsonlDetector::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{CliStatus, Detector};
    use std::time::Duration;

    /// 测试 Codex 检测器工厂函数创建的检测器名称正确
    #[test]
    fn test_codex_detector_name() {
        let detector = create_codex_detector();
        assert_eq!(detector.name(), "CodexDetector");
    }

    /// 测试 Codex 检测器在没有会话文件时返回 Unknown
    #[test]
    fn test_no_session_file_returns_unknown() {
        let detector = create_codex_detector();
        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Unknown => {} // 预期结果
            _ => panic!("没有会话文件时应返回 Unknown"),
        }
    }
}
