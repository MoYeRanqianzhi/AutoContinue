//! # AutoContinue (AC) - 主程序入口
//!
//! AutoContinue是一个CLI工具包装器，用于自动继续或重试CLI工具的运行。
//!
//! ## 功能特性
//! - 自动检测CLI静默状态（无输入/输出）
//! - 静默超过阈值时自动发送继续提示词
//! - 检测错误输出（红色文本）自动发送重试提示词
//! - 保持CLI的完整交互性，用户可正常操作
//! - 任何输入/输出都会重置静默计时器
//! - Ctrl+C优雅退出
//!
//! ## 使用示例
//! ```bash
//! ac claude --resume --cp "继续迭代" --rp "重试"
//! ```

mod args;
mod config;
mod monitor;
mod runner;
mod terminal;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use config::Config;
use monitor::{create_exit_flag, setup_ctrlc_handler};
use runner::Runner;

/// 程序版本号
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 主函数入口
///
/// 解析命令行参数，启动CLI进程，并进入主循环监控状态。
fn main() -> Result<()> {
    // 解析命令行参数
    let args = args::parse_args();

    // 从参数创建配置
    let config = Config::from_args(&args).context("配置加载失败")?;

    // 打印启动信息
    print_banner(&config);

    // 创建退出标志并设置Ctrl+C处理器
    let exit_flag = create_exit_flag();
    setup_ctrlc_handler(exit_flag.clone())?;

    // 进入主循环
    run_main_loop(config, exit_flag)?;

    println!("\n[AC] 程序已退出");
    Ok(())
}

/// 打印启动横幅
///
/// # 参数
/// - `config`: 程序配置
fn print_banner(config: &Config) {
    // 计算总等待时间
    let total_wait = config.silence_threshold + config.sleep_time;

    // 获取提示词显示内容（IO模式显示文件路径）
    let prompt_display = if let Some(ref io_path) = config.continue_prompt_io {
        format!("[IO] {}", io_path)
    } else {
        config.continue_prompt.clone()
    };

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║           AutoContinue (AC) v{}                        ║", VERSION);
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  CLI: {:50} ║", config.cli);
    println!("║  静默阈值: {:3} 秒 (用户设置)                             ║", config.silence_threshold);
    println!("║  额外等待: {:3} 秒 (用户设置)                             ║", config.sleep_time);
    println!("║  总等待:   {:3} 秒                                        ║", total_wait);
    println!("║  继续提示词: {:44} ║", truncate_str(&prompt_display, 44));
    if config.is_continue_prompt_io() {
        println!("║  [IO模式] 每次使用时重新读取文件                          ║");
    }
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  按 Ctrl+C 退出 | 任何输入/输出都会重置计时器            ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
}

/// 截断字符串到指定长度
///
/// # 参数
/// - `s`: 原始字符串
/// - `max_len`: 最大长度
///
/// # 返回值
/// 返回截断后的字符串，如果超长则添加"..."
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        format!("{:width$}", s, width = max_len)
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

/// 主运行循环
///
/// 该函数实现核心的无限循环逻辑：
/// 1. 启动CLI进程
/// 2. 持续监控静默时间
/// 3. 静默超过阈值时自动发送继续提示词
/// 4. 任何输入/输出都会重置计时器
/// 5. 循环直到用户按Ctrl+C或CLI进程退出
///
/// # 参数
/// - `config`: 程序配置
/// - `exit_flag`: 退出标志
///
/// # 返回值
/// 成功返回Ok(())，失败返回错误
fn run_main_loop(config: Config, exit_flag: Arc<AtomicBool>) -> Result<()> {
    // 计算总静默阈值：静默阈值 + 用户设置的等待时间
    let silence_threshold = Duration::from_secs(config.silence_threshold + config.sleep_time);

    // 自动继续计数器
    let mut auto_continue_count = 0u64;

    println!("[AC] 正在启动: {} {}", config.cli, config.cli_args.join(" "));

    // 启动CLI进程
    let mut runner = Runner::new(&config.cli, &config.cli_args)?;

    // 启动双向IO转发（stdout和stdin）
    let _io_handles = runner.start_io_forwarding()?;

    println!("[AC] CLI已启动，开始监控静默状态...");
    println!("[AC] 静默超过 {} 秒将自动发送继续提示词", silence_threshold.as_secs());

    // 主监控循环
    loop {
        // 检查是否需要退出（Ctrl+C）
        if exit_flag.load(Ordering::SeqCst) {
            println!("\n[AC] 收到退出信号...");
            break;
        }

        // 检查CLI进程是否仍在运行
        if !runner.is_running() {
            println!("\n[AC] CLI进程已退出");
            break;
        }

        // 获取当前静默时间
        let silence_duration = runner.get_silence_duration();

        // 检查是否超过静默阈值
        if silence_duration >= silence_threshold {
            auto_continue_count += 1;

            // 获取颜色调试信息（用于诊断）
            let color_debug = runner.get_color_debug_info();
            println!("\n[AC] 调试 - 检测到的颜色: {}", color_debug);

            // 检测是否有错误输出（红色文本）
            let is_error = runner.has_error_output();

            // 根据错误状态选择提示词
            let (prompt, prompt_type) = if is_error {
                // 检测到错误，使用重试提示词
                match config.get_retry_prompt() {
                    Ok(p) => (p, "重试"),
                    Err(e) => {
                        eprintln!("[AC] 获取重试提示词失败: {}", e);
                        continue;
                    }
                }
            } else {
                // 正常状态，使用继续提示词
                match config.get_continue_prompt() {
                    Ok(p) => (p, "继续"),
                    Err(e) => {
                        eprintln!("[AC] 获取继续提示词失败: {}", e);
                        continue;
                    }
                }
            };

            // 发送提示词
            if is_error {
                let error_content = runner.get_error_content();
                println!("\n[AC] === 静默 {} 秒，检测到错误输出，自动发送第 {} 次{}提示词 ===",
                    silence_duration.as_secs(), auto_continue_count, prompt_type);
                if !error_content.is_empty() {
                    // 截断过长的错误内容
                    let display_content = if error_content.len() > 50 {
                        format!("{}...", &error_content[..50])
                    } else {
                        error_content
                    };
                    println!("[AC] 错误内容: {}", display_content);
                }
            } else {
                println!("\n[AC] === 静默 {} 秒，自动发送第 {} 次{}提示词 ===",
                    silence_duration.as_secs(), auto_continue_count, prompt_type);
            }
            println!("[AC] 发送: {}", prompt);

            if let Err(e) = runner.send_line(&prompt) {
                eprintln!("[AC] 发送提示词失败: {}", e);
            }

            // 清除错误检测状态，准备下一轮检测
            runner.clear_error_state();

            // 发送后活动时间会自动更新（在send_input中）
            // 不需要手动重置
        }

        // 短暂休眠避免忙等待（每500ms检查一次）
        thread::sleep(Duration::from_millis(500));
    }

    // 停止IO转发并恢复终端模式
    runner.stop();

    println!("[AC] 共自动发送了 {} 次提示词", auto_continue_count);

    Ok(())
}

/// 带中断的等待
///
/// # 参数
/// - `seconds`: 等待秒数
/// - `exit_flag`: 退出标志
///
/// # 返回值
/// 如果被中断返回true，正常超时返回false
#[allow(dead_code)]
fn wait_with_interrupt(seconds: u64, exit_flag: &Arc<AtomicBool>) -> bool {
    let duration = Duration::from_secs(seconds);
    let start = std::time::Instant::now();

    while start.elapsed() < duration {
        if exit_flag.load(Ordering::SeqCst) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试字符串截断功能
    #[test]
    fn test_truncate_str() {
        // 短字符串不截断
        assert_eq!(truncate_str("hello", 10), "hello     ");

        // 长字符串被截断
        let long_str = "这是一个很长的字符串";
        let result = truncate_str(long_str, 5);
        assert!(result.ends_with("..."));
    }
}
