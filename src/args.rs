//! # 参数解析模块 (args.rs)
//!
//! 该模块负责解析命令行参数，将AC参数和CLI参数分离。
//! AC参数和CLI参数可以混合输入，不分先后顺序。
//!
//! ## AC参数
//! - `-cp, --continue-prompt`: 继续的提示词
//! - `-cpf, --continue-prompt-file`: 继续的提示词文件（启动时读取一次）
//! - `-cpio, --continue-prompt-io`: 继续的提示词IO文件（每次使用时重新读取）
//! - `-rp, --retry-prompt`: 重试的提示词
//! - `-rpf, --retry-prompt-file`: 重试的提示词文件（启动时读取一次）
//! - `-rpio, --retry-prompt-io`: 重试的提示词IO文件（每次使用时重新读取）
//! - `-st, --sleep-time`: 等待时间（秒），默认15秒
//! - `-sth, --silence-threshold`: 静默阈值（秒），默认30秒
//! - `-h, --help`: 显示帮助信息
//! - `-v, --version`: 显示版本信息
//!
//! ## 使用示例
//! ```
//! ac claude --resume -cp "继续迭代" -rp "重试"
//! ac -cp "继续" claude --resume    # AC参数在前也可以
//! ac claude -cpio prompt.md        # 使用IO文件，可动态修改
//! ```

use clap::Parser;
use std::env;
use std::ffi::OsString;

/// AC参数名称列表（带值的参数）
/// 包含所有可能的格式：单横线短格式、双横线短格式、双横线长格式
const AC_ARGS_WITH_VALUE: &[&str] = &[
    // continue-prompt
    "-cp", "--cp", "--continue-prompt",
    // continue-prompt-file
    "-cpf", "--cpf", "--continue-prompt-file",
    // continue-prompt-io (新增)
    "-cpio", "--cpio", "--continue-prompt-io",
    // retry-prompt
    "-rp", "--rp", "--retry-prompt",
    // retry-prompt-file
    "-rpf", "--rpf", "--retry-prompt-file",
    // retry-prompt-io (新增)
    "-rpio", "--rpio", "--retry-prompt-io",
    // sleep-time
    "-st", "--st", "--sleep-time",
    // silence-threshold
    "-sth", "--sth", "--silence-threshold",
];

/// AC参数名称列表（不带值的参数）
const AC_ARGS_NO_VALUE: &[&str] = &[
    "-h", "--help",
    "-v", "--version", "-V",
];

/// AC特有的短参数列表（需要转换为双横线格式）
/// 这些参数支持单横线格式（如 -cp）但会被转换为双横线格式（如 --cp）
const AC_SHORT_ARGS: &[&str] = &["-cp", "-cpf", "-cpio", "-rp", "-rpf", "-rpio", "-st", "-sth"];

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
    /// 与 -cpf, -cpio 互斥
    #[arg(long = "continue-prompt", visible_alias = "cp", value_name = "PROMPT")]
    pub continue_prompt: Option<String>,

    /// 继续的提示词文件路径，从文件读取继续提示词（启动时读取一次）
    /// 与 -cp, -cpio 互斥
    #[arg(long = "continue-prompt-file", visible_alias = "cpf", value_name = "FILE", conflicts_with = "continue_prompt")]
    pub continue_prompt_file: Option<String>,

    /// 继续的提示词IO文件路径，每次使用时重新读取文件
    /// 允许在程序运行时动态修改提示词内容
    /// 与 -cp, -cpf 互斥
    #[arg(long = "continue-prompt-io", visible_alias = "cpio", value_name = "FILE", conflicts_with_all = ["continue_prompt", "continue_prompt_file"])]
    pub continue_prompt_io: Option<String>,

    /// 重试的提示词，当CLI出错时发送
    /// 与 -rpf, -rpio 互斥
    #[arg(long = "retry-prompt", visible_alias = "rp", value_name = "PROMPT")]
    pub retry_prompt: Option<String>,

    /// 重试的提示词文件路径，从文件读取重试提示词（启动时读取一次）
    /// 与 -rp, -rpio 互斥
    #[arg(long = "retry-prompt-file", visible_alias = "rpf", value_name = "FILE", conflicts_with = "retry_prompt")]
    pub retry_prompt_file: Option<String>,

    /// 重试的提示词IO文件路径，每次使用时重新读取文件
    /// 允许在程序运行时动态修改提示词内容
    /// 与 -rp, -rpf 互斥
    #[arg(long = "retry-prompt-io", visible_alias = "rpio", value_name = "FILE", conflicts_with_all = ["retry_prompt", "retry_prompt_file"])]
    pub retry_prompt_io: Option<String>,

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
/// 该函数会预处理参数，分离AC参数和CLI参数，使它们可以混合输入。
/// AC参数会被提取出来，CLI参数会被收集到cli_args中。
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
    let processed_args = separate_and_reorder_args(args);
    Args::parse_from(processed_args)
}

/// 检查参数是否是AC的带值参数
///
/// # 参数
/// - `arg`: 要检查的参数
///
/// # 返回值
/// 如果是AC带值参数返回true
fn is_ac_arg_with_value(arg: &str) -> bool {
    AC_ARGS_WITH_VALUE.contains(&arg)
}

/// 检查参数是否是AC的无值参数
///
/// # 参数
/// - `arg`: 要检查的参数
///
/// # 返回值
/// 如果是AC无值参数返回true
fn is_ac_arg_no_value(arg: &str) -> bool {
    AC_ARGS_NO_VALUE.contains(&arg)
}

/// 将单横线AC参数转换为双横线格式
///
/// # 参数
/// - `arg`: 要转换的参数
///
/// # 返回值
/// 转换后的参数（如 -cp -> --cp）
fn convert_short_arg(arg: &str) -> String {
    for &ac_arg in AC_SHORT_ARGS {
        if arg == ac_arg {
            // 将 -cp 转换为 --cp
            return format!("-{}", arg);
        }
    }
    arg.to_string()
}

/// 分离并重新排序命令行参数
///
/// 该函数会扫描所有参数，将AC参数和CLI参数分离：
/// 1. 找到CLI程序名称（第一个非AC参数的位置参数）
/// 2. 提取所有AC参数（无论它们在哪个位置）
/// 3. 收集所有CLI参数
/// 4. 重新排序：程序名 + AC参数 + CLI名称 + CLI参数
///
/// # 参数
/// - `args`: 原始命令行参数列表
///
/// # 返回值
/// 返回重新排序后的参数列表，格式适合clap解析
fn separate_and_reorder_args(args: Vec<String>) -> Vec<String> {
    // 结果容器
    let mut program_name: Option<String> = None;  // ac程序名
    let mut cli_name: Option<String> = None;      // CLI程序名（如claude）
    let mut ac_args: Vec<String> = Vec::new();    // AC参数及其值
    let mut cli_args: Vec<String> = Vec::new();   // CLI参数

    let mut iter = args.into_iter().peekable();

    // 第一个参数是程序名（ac）
    if let Some(prog) = iter.next() {
        program_name = Some(prog);
    }

    // 遍历剩余参数
    while let Some(arg) = iter.next() {
        if is_ac_arg_with_value(&arg) {
            // AC带值参数：保存参数名和下一个值
            ac_args.push(convert_short_arg(&arg));
            if let Some(value) = iter.next() {
                ac_args.push(value);
            }
        } else if is_ac_arg_no_value(&arg) {
            // AC无值参数：直接保存
            ac_args.push(arg);
        } else if cli_name.is_none() && !arg.starts_with('-') {
            // 第一个非参数的位置参数是CLI名称
            cli_name = Some(arg);
        } else {
            // 其他参数都是CLI参数
            cli_args.push(arg);
        }
    }

    // 重新组装参数列表
    let mut result: Vec<String> = Vec::new();

    // 1. 程序名
    if let Some(prog) = program_name {
        result.push(prog);
    }

    // 2. AC参数
    result.extend(ac_args);

    // 3. CLI名称
    if let Some(cli) = cli_name {
        result.push(cli);
    }

    // 4. CLI参数
    result.extend(cli_args);

    result
}

/// 从原始参数列表中解析参数
///
/// 该函数会预处理参数，分离AC参数和CLI参数，
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
    let processed_args = separate_and_reorder_args(args);
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

    /// 测试AC参数在CLI名称之前
    #[test]
    fn test_ac_args_before_cli() {
        let args = parse_args_from(["ac", "-cp", "继续", "claude"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
    }

    /// 测试AC参数在CLI名称之后（混合顺序）
    #[test]
    fn test_ac_args_after_cli() {
        let args = parse_args_from(["ac", "claude", "-cp", "继续"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
    }

    /// 测试AC参数在CLI参数之后
    #[test]
    fn test_ac_args_after_cli_args() {
        let args = parse_args_from(["ac", "claude", "--resume", "-cp", "继续"]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
        assert!(args.cli_args.iter().any(|a| a == "--resume"));
    }

    /// 测试完全混合的参数顺序
    #[test]
    fn test_mixed_args_order() {
        let args = parse_args_from([
            "ac", "claude", "--resume", "-cp", "继续", "-p", "foo", "-st", "20"
        ]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
        assert_eq!(args.sleep_time, 20);
        // --resume 和 -p foo 应该是CLI参数
        assert!(args.cli_args.iter().any(|a| a == "--resume"));
        assert!(args.cli_args.iter().any(|a| a == "-p"));
        assert!(args.cli_args.iter().any(|a| a == "foo"));
    }

    /// 测试分离重排序函数
    #[test]
    fn test_separate_and_reorder() {
        let args = vec![
            "ac".to_string(),
            "claude".to_string(),
            "--resume".to_string(),
            "-cp".to_string(),
            "继续".to_string(),
            "-st".to_string(),
            "10".to_string(),
        ];
        let result = separate_and_reorder_args(args);
        // 期望顺序：ac, --cp, 继续, --st, 10, claude, --resume
        assert_eq!(result[0], "ac");
        assert_eq!(result[1], "--cp");
        assert_eq!(result[2], "继续");
        assert_eq!(result[3], "--st");
        assert_eq!(result[4], "10");
        assert_eq!(result[5], "claude");
        assert_eq!(result[6], "--resume");
    }

    /// 测试多个AC参数混合
    #[test]
    fn test_multiple_ac_args_mixed() {
        let args = parse_args_from([
            "ac", "-cp", "继续", "claude", "--resume", "-rp", "重试", "-st", "20"
        ]);
        assert_eq!(args.cli, "claude");
        assert_eq!(args.continue_prompt, Some("继续".to_string()));
        assert_eq!(args.retry_prompt, Some("重试".to_string()));
        assert_eq!(args.sleep_time, 20);
        assert!(args.cli_args.iter().any(|a| a == "--resume"));
    }
}
