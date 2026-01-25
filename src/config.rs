//! # 配置管理模块 (config.rs)
//!
//! 该模块负责管理AutoContinue的配置，包括：
//! - 加载提示词（从命令行参数或文件）
//! - 支持IO模式：每次使用时动态读取文件
//! - 管理默认值
//! - 配置验证
//!
//! ## 默认值
//! - `continue_prompt`: "继续"
//! - `retry_prompt`: "重试"
//! - `sleep_time`: 15秒
//!
//! ## IO模式
//! 使用 -cpio 或 -rpio 参数时，提示词会在每次使用时重新读取文件，
//! 允许用户在程序运行时动态修改提示词内容。

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// 默认的继续提示词
const DEFAULT_CONTINUE_PROMPT: &str = "继续";

/// 默认的重试提示词
const DEFAULT_RETRY_PROMPT: &str = "重试";

/// 默认的等待时间（秒）
pub const DEFAULT_SLEEP_TIME: u64 = 15;

/// 默认的静默阈值（秒）
pub const DEFAULT_SILENCE_THRESHOLD: u64 = 30;

/// AutoContinue配置结构体
///
/// 该结构体包含所有运行时需要的配置信息。
/// 配置可以从命令行参数或文件加载。
/// 支持IO模式，允许动态读取提示词。
#[derive(Debug, Clone)]
pub struct Config {
    /// 要运行的CLI程序名称
    pub cli: String,

    /// 传递给CLI程序的参数
    pub cli_args: Vec<String>,

    /// 继续的提示词（静态模式）
    /// 当CLI正常结束时发送此提示词
    /// 如果设置了 continue_prompt_io，此字段为空
    pub continue_prompt: String,

    /// 继续提示词的IO文件路径（动态模式）
    /// 如果设置了此字段，每次使用时会重新读取文件
    pub continue_prompt_io: Option<String>,

    /// 重试的提示词（静态模式）
    /// 当CLI出错时发送此提示词
    /// 如果设置了 retry_prompt_io，此字段为空
    pub retry_prompt: String,

    /// 重试提示词的IO文件路径（动态模式）
    /// 如果设置了此字段，每次使用时会重新读取文件
    pub retry_prompt_io: Option<String>,

    /// 等待时间（秒）
    /// 在自动发送提示词之前等待的时间
    /// 给用户自主回复的机会
    pub sleep_time: u64,

    /// 静默阈值（秒）
    /// CLI无输入/输出超过此时间后开始计算等待时间
    /// 总等待时间 = 静默阈值 + 等待时间
    pub silence_threshold: u64,
}

impl Config {
    /// 从命令行参数创建配置
    ///
    /// # 参数
    /// - `args`: 解析后的命令行参数
    ///
    /// # 返回值
    /// 成功返回Config实例，失败返回错误
    ///
    /// # 错误
    /// - 当指定的提示词文件不存在或无法读取时返回错误
    ///
    /// # 示例
    /// ```
    /// let args = parse_args();
    /// let config = Config::from_args(&args)?;
    /// ```
    pub fn from_args(args: &crate::args::Args) -> Result<Self> {
        // 处理继续提示词：支持三种模式
        // 1. IO模式：存储文件路径，每次使用时重新读取
        // 2. 文件模式：启动时读取一次，存储内容
        // 3. 参数/默认值模式：直接使用字符串
        let (continue_prompt, continue_prompt_io) = if let Some(ref io_path) = args.continue_prompt_io {
            // IO模式：验证文件存在，存储路径
            if !Path::new(io_path).exists() {
                anyhow::bail!("继续提示词IO文件不存在: {}", io_path);
            }
            (String::new(), Some(io_path.clone()))
        } else if let Some(ref prompt) = args.continue_prompt {
            // 从命令行参数读取继续提示词
            (prompt.clone(), None)
        } else if let Some(ref file_path) = args.continue_prompt_file {
            // 从文件读取继续提示词（一次性）
            let prompt = load_prompt_from_file(file_path)
                .with_context(|| format!("无法从文件加载继续提示词: {}", file_path))?;
            (prompt, None)
        } else {
            // 使用默认继续提示词
            (DEFAULT_CONTINUE_PROMPT.to_string(), None)
        };

        // 处理重试提示词：同样支持三种模式
        let (retry_prompt, retry_prompt_io) = if let Some(ref io_path) = args.retry_prompt_io {
            // IO模式：验证文件存在，存储路径
            if !Path::new(io_path).exists() {
                anyhow::bail!("重试提示词IO文件不存在: {}", io_path);
            }
            (String::new(), Some(io_path.clone()))
        } else if let Some(ref prompt) = args.retry_prompt {
            // 从命令行参数读取重试提示词
            (prompt.clone(), None)
        } else if let Some(ref file_path) = args.retry_prompt_file {
            // 从文件读取重试提示词（一次性）
            let prompt = load_prompt_from_file(file_path)
                .with_context(|| format!("无法从文件加载重试提示词: {}", file_path))?;
            (prompt, None)
        } else {
            // 使用默认重试提示词
            (DEFAULT_RETRY_PROMPT.to_string(), None)
        };

        // 将CLI参数从OsString转换为String
        let cli_args: Vec<String> = args
            .cli_args
            .iter()
            .filter_map(|s| s.to_str().map(|s| s.to_string()))
            .collect();

        Ok(Config {
            cli: args.cli.clone(),
            cli_args,
            continue_prompt,
            continue_prompt_io,
            retry_prompt,
            retry_prompt_io,
            sleep_time: args.sleep_time,
            silence_threshold: args.silence_threshold,
        })
    }

    /// 获取当前的继续提示词
    ///
    /// 如果配置了IO模式，会重新读取文件获取最新内容；
    /// 否则返回静态存储的提示词。
    ///
    /// # 返回值
    /// 成功返回提示词内容，失败返回错误
    ///
    /// # 错误
    /// - IO模式下文件读取失败
    pub fn get_continue_prompt(&self) -> Result<String> {
        if let Some(ref io_path) = self.continue_prompt_io {
            // IO模式：每次重新读取文件
            load_prompt_from_file(io_path)
                .with_context(|| format!("无法读取继续提示词IO文件: {}", io_path))
        } else {
            // 静态模式：返回存储的提示词
            Ok(self.continue_prompt.clone())
        }
    }

    /// 获取当前的重试提示词
    ///
    /// 如果配置了IO模式，会重新读取文件获取最新内容；
    /// 否则返回静态存储的提示词。
    ///
    /// # 返回值
    /// 成功返回提示词内容，失败返回错误
    ///
    /// # 错误
    /// - IO模式下文件读取失败
    pub fn get_retry_prompt(&self) -> Result<String> {
        if let Some(ref io_path) = self.retry_prompt_io {
            // IO模式：每次重新读取文件
            load_prompt_from_file(io_path)
                .with_context(|| format!("无法读取重试提示词IO文件: {}", io_path))
        } else {
            // 静态模式：返回存储的提示词
            Ok(self.retry_prompt.clone())
        }
    }

    /// 检查是否使用继续提示词IO模式
    ///
    /// # 返回值
    /// 如果配置了IO模式返回true
    pub fn is_continue_prompt_io(&self) -> bool {
        self.continue_prompt_io.is_some()
    }

    /// 检查是否使用重试提示词IO模式
    ///
    /// # 返回值
    /// 如果配置了IO模式返回true
    #[allow(dead_code)]
    pub fn is_retry_prompt_io(&self) -> bool {
        self.retry_prompt_io.is_some()
    }

    /// 获取完整的CLI命令（包含程序名和参数）
    ///
    /// # 返回值
    /// 返回CLI程序名和所有参数组成的向量
    #[allow(dead_code)]
    pub fn get_full_command(&self) -> Vec<String> {
        let mut cmd = vec![self.cli.clone()];
        cmd.extend(self.cli_args.clone());
        cmd
    }
}

/// 从文件加载提示词
///
/// # 参数
/// - `path`: 提示词文件路径
///
/// # 返回值
/// 成功返回文件内容（去除首尾空白，标准化换行符），失败返回错误
///
/// # 错误
/// - 文件不存在
/// - 文件无法读取
///
/// # 注意
/// 该函数会标准化换行符：
/// - Windows换行符 `\r\n` 会被转换为 `\n`
/// - 单独的 `\r` 也会被转换为 `\n`
/// 这确保了跨平台的一致行为
fn load_prompt_from_file<P: AsRef<Path>>(path: P) -> Result<String> {
    let content = fs::read_to_string(path.as_ref())
        .with_context(|| "读取文件失败")?;

    // 标准化换行符：将 \r\n 和单独的 \r 都转换为 \n
    // 这样可以避免在PTY中换行被重复处理
    let normalized = content
        .replace("\r\n", "\n")  // Windows换行符 -> Unix换行符
        .replace("\r", "\n");   // 旧Mac换行符 -> Unix换行符

    // 去除首尾空白字符
    Ok(normalized.trim().to_string())
}

impl Default for Config {
    /// 创建默认配置
    ///
    /// 默认配置使用空CLI名称和默认提示词（静态模式）
    fn default() -> Self {
        Config {
            cli: String::new(),
            cli_args: Vec::new(),
            continue_prompt: DEFAULT_CONTINUE_PROMPT.to_string(),
            continue_prompt_io: None,
            retry_prompt: DEFAULT_RETRY_PROMPT.to_string(),
            retry_prompt_io: None,
            sleep_time: DEFAULT_SLEEP_TIME,
            silence_threshold: DEFAULT_SILENCE_THRESHOLD,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// 测试从文件加载提示词
    #[test]
    fn test_load_prompt_from_file() -> Result<()> {
        // 创建临时文件
        let mut file = NamedTempFile::new()?;
        writeln!(file, "  测试提示词  ")?;

        // 加载并验证
        let prompt = load_prompt_from_file(file.path())?;
        assert_eq!(prompt, "测试提示词");

        Ok(())
    }

    /// 测试换行符标准化（Windows格式 \r\n）
    #[test]
    fn test_normalize_crlf() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        // 写入Windows格式换行符
        file.write_all(b"Line1\r\nLine2\r\nLine3")?;

        let prompt = load_prompt_from_file(file.path())?;
        // 应该只有 \n，没有 \r
        assert!(!prompt.contains('\r'));
        assert_eq!(prompt, "Line1\nLine2\nLine3");

        Ok(())
    }

    /// 测试换行符标准化（单独的 \r）
    #[test]
    fn test_normalize_cr() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        // 写入旧Mac格式换行符
        file.write_all(b"Line1\rLine2\rLine3")?;

        let prompt = load_prompt_from_file(file.path())?;
        // \r 应该被转换为 \n
        assert!(!prompt.contains('\r'));
        assert_eq!(prompt, "Line1\nLine2\nLine3");

        Ok(())
    }

    /// 测试默认配置
    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.continue_prompt, DEFAULT_CONTINUE_PROMPT);
        assert_eq!(config.retry_prompt, DEFAULT_RETRY_PROMPT);
        assert_eq!(config.sleep_time, DEFAULT_SLEEP_TIME);
        assert_eq!(config.silence_threshold, DEFAULT_SILENCE_THRESHOLD);
    }
}
