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
//! - `-sth, --silence-threshold`: 静默阈值（秒），默认30秒
//! - `-h, --help`: 显示帮助信息
//! - `-v, --version`: 显示版本信息
//!
//! ## 使用示例
//! ```
//! ac claude --resume -cp "继续迭代" -rp "重试"
//! ```

use clap::Parser;
use std::env;
use std::ffi::OsString;

/// AC特有的短参数列表（需要转换为双横线格式）
/// 这些参数支持单横线格式（如 -cp）但会被转换为双横线格式（如 --cp）
const AC_SHORT_ARGS: &[&str] = &["-cp", "-cpf", "-rp", "-rpf", "-st", "-sth"];

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
    #[arg(long = "continue-prompt", visible_alias = "cp", value_name = "PROMPT")]
    pub continue_prompt: Option<String>,

    /// 继续的提示词文件路径，从文件读取继续提示词
    /// 与 -cp 互斥
    #[arg(long = "continue-prompt-file", visible_alias = "cpf", value_name = "FILE", conflicts_with = "continue_prompt")]
    pub continue_prompt_file: Option<String>,

    /// 重试的提示词，当CLI出错时发送
    /// 与 -rpf 互斥
    #[arg(long = "retry-prompt", visible_alias = "rp", value_name = "PROMPT")]
    pub retry_prompt: Option<String>,

    /// 重试的提示词文件路径，从文件读取重试提示词
    /// 与 -rp 互斥
    #[arg(long = "retry-prompt-file", visible_alias = "rpf", value_name = "FILE", conflicts_with = "retry_prompt")]
    pub retry_prompt_file: Option<String>,

    /// 等待时间（秒），用于给用户自主回复的时间
    /// 超过该时间则自动继续，默认15秒
    #[arg(long = "sleep-time", visible_alias = "st", value_name = "SECONDS", default_value = "15")]
    pub sleep_time: u64,

    /// 静默阈值（秒），CLI无输入/输出超过此时间后开始计算等待时间
    /// 默认30秒，总等待时间 = 静默阈值 + 等待时间
    #[arg(long = "silence-threshold", visible_alias = "sth", value_name = "SECONDS", default_value = "30")]
    pub silence_threshold: u64,

    /// 传递给CLI程序的其他参数
    /// 这些参数会原样传递给CLI程序
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cli_args: Vec<OsString>,
}

/// 解析命令行参数
///
/// 该函数会预处理参数，将单横线的AC参数（如 -cp）转换为双横线格式（如 --cp），
/// 然后交给clap进行解析。
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
    // 获取原始参数并预处理
    let args: Vec<String> = env::args().collect();
    let processed_args = preprocess_args(args);
    Args::parse_from(processed_args)
}

/// 预处理命令行参数
///
/// 将单横线的AC参数（如 -cp）转换为双横线格式（如 --cp），
/// 以便clap能够正确解析。
///
/// # 参数
/// - `args`: 原始命令行参数列表
///
/// # 返回值
/// 返回处理后的参数列表
fn preprocess_args(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .map(|arg| {
            // 检查是否是需要转换的AC短参数
            for &ac_arg in AC_SHORT_ARGS {
                if arg == ac_arg {
                    // 将 -cp 转换为 --cp（去掉第一个横线，加上双横线）
                    return format!("-{}", arg);
                }
            }
            arg
        })
        .collect()
}

/// 从原始参数列表中解析参数
///
/// 该函数会预处理参数，将单横线的AC参数转换为双横线格式，
/// 然后交给clap进行解析。主要用于测试。
///
/// # 参数
/// - `raw_args`: 原始命令行参数列表
///
/// # 返回值
/// 返回解析后的Args结构体
#[allow(dead_code)]
pub fn parse_args_from<I, T>(raw_args: I) -> Args
where
    I: IntoIterator<Item = T>,
    T: Into<String> + Clone,
{
    let args: Vec<String> = raw_args.into_iter().map(|a| a.into()).collect();
    let processed_args = preprocess_args(args);
    Args::parse_from(processed_args)
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
        assert_eq!(args.silence_threshold, 30);
    }

    /// 测试带有继续提示词的参数（使用 -cp）
    #[test]
    fn test_continue_prompt() {
        let args = parse_args_from(["ac", "-cp", "继续", "claude"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
    }

    /// 测试预处理函数
    #[test]
    fn test_preprocess_args() {
        let args = vec![
            "ac".to_string(),
            "-cp".to_string(),
            "继续".to_string(),
            "-st".to_string(),
            "10".to_string(),
            "claude".to_string(),
        ];
        let processed = preprocess_args(args);
        assert_eq!(processed[1], "--cp");
        assert_eq!(processed[3], "--st");
    }

    /// 测试带有CLI参数的解析
    #[test]
    fn test_cli_args() {
        let args = parse_args_from(["ac", "-cp", "继续", "claude", "--resume"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
        // --resume 会被识别为cli_args的一部分
        assert!(args.cli_args.iter().any(|a| a == "--resume"));
    }
}
