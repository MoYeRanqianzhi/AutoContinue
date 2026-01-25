//! # 参数解析模块 (args.rs)
//!
//! 该模块负责解析命令行参数，将AC参数和CLI参数分离。
//!
//! ## AC参数
//! - `-cp, --continue-prompt`: 继续的提示词
//! - `-cpf, --continue-prompt-file`: 继续的提示词文件
//! - `-rp, --retry-prompt`: 重试的提示词
//! - `-rpf, --retry-prompt-file`: 重试的提示词文件
//! - `-st, --sleep-time`: 等待时间（秒），默认15秒
//! - `-h, --help`: 显示帮助信息
//! - `-v, --version`: 显示版本信息
//!
//! ## 使用示例
//! ```
//! ac claude --resume -cp "继续迭代" -rp "重试"
//! ```

use clap::Parser;
use std::ffi::OsString;

/// AutoContinue (AC) - 自动继续/重试CLI工具的包装器
///
/// 该程序会自动监控CLI工具的运行状态，在CLI停止时自动发送继续或重试的提示词。
/// 用户仍然可以正常操作CLI，所有CLI功能不受影响。
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
#[command(name = "ac")]
pub struct Args {
    /// 要运行的CLI程序名称（如：claude, codex, gemini, opencode等）
    #[arg(required = true)]
    pub cli: String,

    /// 继续的提示词，当CLI正常结束时发送
    /// 与 -cpf 互斥
    #[arg(short = 'c', long = "continue-prompt", value_name = "PROMPT")]
    pub continue_prompt: Option<String>,

    /// 继续的提示词文件路径，从文件读取继续提示词
    /// 与 -cp 互斥
    #[arg(long = "continue-prompt-file", value_name = "FILE", conflicts_with = "continue_prompt")]
    pub continue_prompt_file: Option<String>,

    /// 重试的提示词，当CLI出错时发送
    /// 与 -rpf 互斥
    #[arg(short = 'r', long = "retry-prompt", value_name = "PROMPT")]
    pub retry_prompt: Option<String>,

    /// 重试的提示词文件路径，从文件读取重试提示词
    /// 与 -rp 互斥
    #[arg(long = "retry-prompt-file", value_name = "FILE", conflicts_with = "retry_prompt")]
    pub retry_prompt_file: Option<String>,

    /// 等待时间（秒），用于给用户自主回复的时间
    /// 超过该时间则自动继续，默认15秒
    #[arg(short = 's', long = "sleep-time", value_name = "SECONDS", default_value = "15")]
    pub sleep_time: u64,

    /// 传递给CLI程序的其他参数
    /// 这些参数会原样传递给CLI程序
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cli_args: Vec<OsString>,
}

/// 解析命令行参数
///
/// # 返回值
/// 返回解析后的Args结构体
///
/// # 示例
/// ```
/// let args = parse_args();
/// println!("CLI: {}", args.cli);
/// ```
pub fn parse_args() -> Args {
    Args::parse()
}

/// 从原始参数列表中手动解析AC参数和CLI参数
///
/// 这个函数用于处理复杂的参数分离场景，
/// 当clap的自动解析无法满足需求时使用。
///
/// # 参数
/// - `raw_args`: 原始命令行参数列表
///
/// # 返回值
/// 返回元组 (Args, Vec<OsString>)，分别为AC参数和CLI参数
#[allow(dead_code)]
pub fn parse_args_from<I, T>(raw_args: I) -> Args
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Args::parse_from(raw_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试基本参数解析
    #[test]
    fn test_basic_args() {
        let args = parse_args_from(["ac", "claude"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.sleep_time, 15);
    }

    /// 测试带有继续提示词的参数
    #[test]
    fn test_continue_prompt() {
        let args = parse_args_from(["ac", "claude", "-c", "继续"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
    }

    /// 测试带有CLI参数的解析
    #[test]
    fn test_cli_args() {
        let args = parse_args_from(["ac", "claude", "--resume", "-c", "继续"]);
        assert_eq!(args.cli, "claude");
        // --resume 会被识别为cli_args的一部分
    }
}
