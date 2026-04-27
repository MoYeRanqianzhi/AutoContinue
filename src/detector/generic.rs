//! # 通用检测适配器 (detector/generic.rs)
//!
//! 用于不支持 JSONL 会话文件的 CLI 工具（如 Gemini CLI、OpenCode 等）。
//!
//! ## 检测机制
//!
//! - **错误检测**: 在原始输出文本中匹配常见错误模式
//!   （如 "Error:", "error:", "failed", "FATAL", "panic" 等）
//! - **空闲检测**: 依赖静默超时（与原 AC 行为一致）
//! - **忙碌检测**: 最近有输出时视为忙碌
//!
//! ## 与旧方案的区别
//!
//! 旧方案通过 ANSI 解析后的红色字符计数判断错误，
//! 本方案直接在原始输出文本中匹配关键词，更简单且不依赖颜色信息。

use anyhow::Result;
use std::time::{Duration, Instant};

use super::{CliStatus, Detector};

/// 错误模式匹配的最小字符数
///
/// 输出缓冲区中匹配到的错误关键词总字符数超过此阈值才视为错误，
/// 避免短暂的错误提示（如单词 "error" 出现在正常文本中）被误判。
const ERROR_CHAR_THRESHOLD: usize = 10;

/// 输出缓冲区最大大小（字节）
///
/// 防止内存无限增长，只保留最近的输出用于分析。
const OUTPUT_BUFFER_MAX: usize = 8192;

/// 常见的错误关键词列表
///
/// 按优先级排列，匹配时不区分大小写。
/// 这些关键词覆盖了大多数 CLI 工具的错误输出格式。
const ERROR_PATTERNS: &[&str] = &[
    "error:",
    "error[",      // Rust 编译器错误格式 error[E0xxx]
    "fatal:",
    "fatal error",
    "panic:",
    "panicked at",
    "failed:",
    "failure:",
    "exception:",
    "traceback",   // Python 错误
    "segfault",
    "segmentation fault",
    "aborted",
    "permission denied",
];

/// 通用检测适配器
///
/// 基于输出文本模式匹配的通用错误检测器。
/// 不依赖任何 CLI 工具特有的机制，适用于所有 CLI 工具。
pub struct GenericDetector {
    /// 输出缓冲区，存储最近的原始输出文本
    /// 用于错误模式匹配分析
    output_buffer: String,

    /// 是否检测到错误模式
    error_detected: bool,

    /// 检测到的错误信息摘要
    error_message: String,

    /// 最后一次接收到输出数据的时间
    last_output_time: Option<Instant>,
}

impl GenericDetector {
    /// 创建新的通用检测器
    pub fn new() -> Self {
        GenericDetector {
            output_buffer: String::new(),
            error_detected: false,
            error_message: String::new(),
            last_output_time: None,
        }
    }

    /// 在输出缓冲区中搜索错误模式
    ///
    /// 遍历所有已知的错误关键词，在缓冲区文本中进行不区分大小写的匹配。
    /// 如果匹配到的错误内容总字符数超过阈值，则标记为错误。
    fn scan_for_errors(&mut self) {
        // 将缓冲区转为小写用于不区分大小写匹配
        let lower = self.output_buffer.to_lowercase();

        // 收集所有匹配到的错误关键词及其周围上下文
        let mut error_chars = 0;
        let mut first_match = String::new();

        for pattern in ERROR_PATTERNS {
            if let Some(pos) = lower.find(pattern) {
                error_chars += pattern.len();

                // 提取错误行作为摘要（取匹配位置所在行）
                if first_match.is_empty() {
                    // 从匹配位置向前找行首
                    let line_start = lower[..pos].rfind('\n').map_or(0, |p| p + 1);
                    // 从匹配位置向后找行尾
                    let line_end = lower[pos..].find('\n').map_or(lower.len(), |p| pos + p);
                    // 截取原始文本（保留大小写）
                    let line = &self.output_buffer[line_start..line_end];
                    // 限制长度
                    first_match = line.chars().take(100).collect();
                }
            }
        }

        if error_chars >= ERROR_CHAR_THRESHOLD {
            self.error_detected = true;
            self.error_message = first_match;
        }
    }
}

impl Detector for GenericDetector {
    /// 初始化通用检测器
    ///
    /// 通用检测器不需要特殊初始化（无文件监控），
    /// 直接返回成功。
    fn init(&mut self, _cli_name: &str, _cli_args: &[String]) -> Result<()> {
        Ok(())
    }

    /// 处理新的输出数据
    ///
    /// 将原始字节追加到输出缓冲区，并触发错误模式扫描。
    /// 缓冲区超过最大大小时，截断前半部分保留最新内容。
    ///
    /// # 注意
    /// 输入数据可能包含 ANSI 转义序列，但我们直接在原始文本中
    /// 搜索关键词，转义序列不会影响匹配结果。
    fn feed_output(&mut self, data: &[u8]) {
        // 更新最后输出时间
        self.last_output_time = Some(Instant::now());

        // 将字节数据追加到缓冲区（忽略非 UTF-8 字节）
        if let Ok(text) = std::str::from_utf8(data) {
            self.output_buffer.push_str(text);
        } else {
            // 有损转换：替换无效 UTF-8 字节
            let text = String::from_utf8_lossy(data);
            self.output_buffer.push_str(&text);
        }

        // 限制缓冲区大小，保留后半部分（最新内容）
        if self.output_buffer.len() > OUTPUT_BUFFER_MAX {
            // 找到一个安全的 UTF-8 边界进行截断
            let trim_to = self.output_buffer.len() - OUTPUT_BUFFER_MAX / 2;
            // 确保在字符边界处截断
            let safe_trim = self.output_buffer.ceil_char_boundary(trim_to);
            self.output_buffer = self.output_buffer[safe_trim..].to_string();
        }

        // 扫描错误模式
        self.scan_for_errors();
    }

    /// 查询当前状态
    ///
    /// 判断逻辑：
    /// 1. 如果检测到错误模式 → `Error`
    /// 2. 如果静默时间超过阈值 → `Unknown`（让主循环 fallback 到超时处理）
    /// 3. 如果最近有输出 → `Busy`（不应视为 Unknown，因为 CLI 正在活动）
    /// 4. 其他情况 → `Unknown`
    fn status(&self, silence_duration: Duration, silence_threshold: Duration) -> CliStatus {
        // 优先检查错误
        if self.error_detected {
            return CliStatus::Error {
                message: self.error_message.clone(),
            };
        }

        // 静默超时 → Unknown（让主循环的 fallback 逻辑处理）
        if silence_duration >= silence_threshold {
            return CliStatus::Unknown;
        }

        // 最近有输出 → Busy
        if let Some(last_time) = self.last_output_time {
            if last_time.elapsed() < Duration::from_secs(2) {
                return CliStatus::Busy;
            }
        }

        // 默认返回 Unknown
        CliStatus::Unknown
    }

    /// 重置检测状态
    ///
    /// 清空输出缓冲区和错误标记，准备检测下一轮输出。
    fn reset(&mut self) {
        self.output_buffer.clear();
        self.error_detected = false;
        self.error_message.clear();
    }

    /// 返回检测器名称
    fn name(&self) -> &str {
        "GenericDetector"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试基本错误检测
    #[test]
    fn test_error_detection() {
        let mut detector = GenericDetector::new();
        detector.init("gemini", &[]).unwrap();

        // 模拟包含错误的输出
        detector.feed_output(b"Some normal output\n");
        detector.feed_output(b"fatal error: connection refused\n");
        detector.feed_output(b"Error: something went wrong with the process\n");

        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Error { message } => {
                assert!(!message.is_empty());
            }
            _ => panic!("应该检测到错误，但得到了: {:?}", status),
        }
    }

    /// 测试正常输出不误报
    #[test]
    fn test_no_false_positive() {
        let mut detector = GenericDetector::new();
        detector.init("gemini", &[]).unwrap();

        // 正常输出不应触发错误
        detector.feed_output(b"Processing files...\n");
        detector.feed_output(b"Done! 42 files processed.\n");

        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Error { .. } => panic!("不应该检测到错误"),
            _ => {} // OK
        }
    }

    /// 测试重置后清除错误状态
    #[test]
    fn test_reset_clears_error() {
        let mut detector = GenericDetector::new();
        detector.init("test", &[]).unwrap();

        // 触发错误
        detector.feed_output(b"fatal error: something broke badly here\n");
        assert!(detector.error_detected);

        // 重置后错误应该被清除
        detector.reset();
        assert!(!detector.error_detected);
        assert!(detector.output_buffer.is_empty());
    }

    /// 测试缓冲区大小限制
    #[test]
    fn test_buffer_size_limit() {
        let mut detector = GenericDetector::new();
        detector.init("test", &[]).unwrap();

        // 写入超过最大缓冲区大小的数据
        let large_data = "x".repeat(OUTPUT_BUFFER_MAX + 1000);
        detector.feed_output(large_data.as_bytes());

        // 缓冲区应该被截断
        assert!(detector.output_buffer.len() <= OUTPUT_BUFFER_MAX);
    }

    /// 测试检测器名称
    #[test]
    fn test_detector_name() {
        let detector = GenericDetector::new();
        assert_eq!(detector.name(), "GenericDetector");
    }
}
