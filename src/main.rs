//! # AutoContinue (AC) - 主程序入口
//!
//! AutoContinue是一个CLI工具包装器，用于自动继续或重试CLI工具的运行。
//!
//! ## 功能特性
//! - 自动检测CLI进程状态
//! - CLI正常结束时自动发送继续提示词
//! - CLI出错时自动发送重试提示词
//! - 保持CLI的完整交互性，用户可正常操作
//! - 支持用户手动恢复（在等待期间）
//! - Ctrl+C优雅退出
//!
//! ## 使用示例
//! ```bash
//! ac claude --resume -cp "继续迭代" -rp "重试"
//! ```

mod args;
mod config;
mod monitor;
mod runner;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use config::Config;
use monitor::{create_exit_flag, setup_ctrlc_handler, Monitor};
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
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║           AutoContinue (AC) v{}                        ║", VERSION);
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  CLI: {:50} ║", config.cli);
    println!("║  等待时间: {:3} 秒                                        ║", config.sleep_time);
    println!("║  继续提示词: {:44} ║", truncate_str(&config.continue_prompt, 44));
    println!("║  重试提示词: {:44} ║", truncate_str(&config.retry_prompt, 44));
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  按 Ctrl+C 退出                                          ║");
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
/// 2. 监控进程状态
/// 3. 进程退出时等待用户响应
/// 4. 超时后自动发送继续/重试提示词
/// 5. 重复上述过程
///
/// # 参数
/// - `config`: 程序配置
/// - `exit_flag`: 退出标志
///
/// # 返回值
/// 成功返回Ok(())，失败返回错误
#[allow(unused_assignments)]
fn run_main_loop(config: Config, exit_flag: Arc<AtomicBool>) -> Result<()> {
    // 创建状态监控器
    let mut monitor = Monitor::new(config.sleep_time, exit_flag.clone());

    // 循环计数器
    let mut iteration = 0u64;

    // 待发送的提示词（用于下一次迭代）
    let mut pending_prompt: Option<String> = None;

    // 主循环：持续运行直到用户按Ctrl+C
    while !monitor.should_exit() {
        iteration += 1;
        println!("\n[AC] === 第 {} 次迭代 ===", iteration);

        // 启动CLI进程
        let runner_result = start_cli(&config);

        match runner_result {
            Ok(mut runner) => {
                // 设置运行标志
                monitor.set_running_flag(runner.get_running_flag());
                monitor.set_running();

                // 启动IO转发
                let io_handle = runner.start_io_forwarding()?;

                // 如果有待发送的提示词，发送它
                if let Some(ref prompt) = pending_prompt {
                    // 等待一小段时间让CLI准备好接收输入
                    thread::sleep(Duration::from_millis(500));
                    println!("[AC] 发送提示词: {}", prompt);
                    if let Err(e) = runner.send_line(prompt) {
                        eprintln!("[AC] 发送提示词失败: {}", e);
                    }
                    pending_prompt = None;
                }

                // 监控进程状态
                let exit_status = monitor_process(&mut runner, &mut monitor, &exit_flag)?;

                // 等待IO线程结束
                runner.stop();
                let _ = io_handle.join();

                // 如果需要退出，跳出循环
                if monitor.should_exit() {
                    break;
                }

                // 根据退出状态决定发送哪个提示词
                let prompt = if runner::is_success(&exit_status) {
                    println!("\n[AC] CLI正常结束，准备发送继续提示词...");
                    &config.continue_prompt
                } else {
                    println!("\n[AC] CLI出错退出，准备发送重试提示词...");
                    &config.retry_prompt
                };

                // 等待用户响应或超时
                if wait_for_user_or_timeout(&mut monitor, &exit_flag)? {
                    // 用户手动响应了，不需要自动发送
                    println!("[AC] 检测到用户输入，跳过自动发送");
                    pending_prompt = None;
                    continue;
                }

                // 如果需要退出，跳出循环
                if monitor.should_exit() {
                    break;
                }

                println!("[AC] 等待超时，下一次迭代将自动发送: {}", prompt);
                pending_prompt = Some(prompt.clone());
            }
            Err(e) => {
                eprintln!("\n[AC] 启动CLI失败: {}", e);
                eprintln!("[AC] {} 秒后重试...", config.sleep_time);

                // 等待后重试
                if wait_with_interrupt(config.sleep_time, &exit_flag) {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// 启动CLI进程
///
/// # 参数
/// - `config`: 程序配置
///
/// # 返回值
/// 成功返回Runner实例，失败返回错误
fn start_cli(config: &Config) -> Result<Runner> {
    println!("[AC] 正在启动: {} {}", config.cli, config.cli_args.join(" "));

    Runner::new(&config.cli, &config.cli_args)
}

/// 监控CLI进程状态
///
/// # 参数
/// - `runner`: CLI运行器
/// - `monitor`: 状态监控器
/// - `exit_flag`: 退出标志
///
/// # 返回值
/// 返回CLI进程的退出状态
fn monitor_process(
    runner: &mut Runner,
    _monitor: &mut Monitor,
    exit_flag: &Arc<AtomicBool>,
) -> Result<portable_pty::ExitStatus> {
    // 循环检查进程状态
    loop {
        // 检查是否需要退出
        if exit_flag.load(Ordering::SeqCst) {
            // 等待进程结束
            return runner.wait();
        }

        // 检查进程是否仍在运行
        if !runner.is_running() {
            // 进程已结束，获取退出状态
            if let Some(status) = runner.get_exit_status() {
                return Ok(status);
            }
            // 等待获取退出状态
            return runner.wait();
        }

        // 短暂休眠避免忙等待
        thread::sleep(Duration::from_millis(100));
    }
}

/// 等待用户响应或超时
///
/// # 参数
/// - `monitor`: 状态监控器
/// - `exit_flag`: 退出标志
///
/// # 返回值
/// 如果用户有输入返回true，超时返回false
fn wait_for_user_or_timeout(
    monitor: &mut Monitor,
    exit_flag: &Arc<AtomicBool>,
) -> Result<bool> {
    monitor.set_waiting_user();

    let wait_time = monitor.remaining_wait_time();
    println!(
        "[AC] 等待 {} 秒让用户响应（或自动继续）...",
        wait_time.as_secs()
    );

    // 尝试启用原始模式以检测按键
    let raw_mode_enabled = enable_raw_mode().is_ok();

    let start = std::time::Instant::now();
    let mut user_input = false;

    while start.elapsed() < wait_time {
        // 检查退出标志
        if exit_flag.load(Ordering::SeqCst) {
            if raw_mode_enabled {
                let _ = disable_raw_mode();
            }
            return Ok(false);
        }

        // 检查用户输入（非阻塞）
        if raw_mode_enabled {
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key_event) = event::read()? {
                    // 检测到按键，用户有响应
                    // 但忽略Ctrl+C，因为那是退出信号
                    if !(key_event.modifiers.contains(KeyModifiers::CONTROL)
                        && key_event.code == KeyCode::Char('c'))
                    {
                        user_input = true;
                        break;
                    }
                }
            }
        } else {
            // 如果无法启用原始模式，只能简单等待
            thread::sleep(Duration::from_millis(100));
        }
    }

    // 恢复终端模式
    if raw_mode_enabled {
        let _ = disable_raw_mode();
    }

    if user_input {
        monitor.set_user_resumed();
    }

    Ok(user_input)
}

/// 带中断的等待
///
/// # 参数
/// - `seconds`: 等待秒数
/// - `exit_flag`: 退出标志
///
/// # 返回值
/// 如果被中断返回true，正常超时返回false
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
