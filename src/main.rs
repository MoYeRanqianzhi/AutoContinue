//! # AutoContinue (AC) - 主程序入口
//!
//! AutoContinue是一个CLI工具包装器，用于自动继续或重试CLI工具的运行。
//!
//! ## 功能特性
//! - 自动检测CLI静默状态（无输入/输出）
//! - 静默超过阈值时自动发送继续提示词
//! - 通过 Detector 适配器精确检测 CLI 状态和错误
//!   - Claude Code: 监控 JSONL 会话文件
//!   - Codex: 监控 JSONL 会话文件
//!   - 其他工具: 输出文本模式匹配
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
mod detector;
mod runner;
mod hook;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use config::Config;
use detector::{CliStatus, Detector, create_detector};
use hook::{StopHookManager, StopHook};
use runner::Runner;

/// 创建全局退出标志
///
/// # 返回值
/// 返回用于 Ctrl+C 处理的原子布尔值
fn create_exit_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// 设置 Ctrl+C 处理器
///
/// # 参数
/// - `exit_flag`: 退出标志，按下 Ctrl+C 时设置为 true
///
/// # 返回值
/// 成功返回 Ok(())，失败返回错误
fn setup_ctrlc_handler(exit_flag: Arc<AtomicBool>) -> Result<()> {
    ctrlc::set_handler(move || {
        exit_flag.store(true, Ordering::SeqCst);
        eprintln!("\n[AC] 收到退出信号，正在退出...");
    })
    .map_err(|e| anyhow::anyhow!("无法设置Ctrl+C处理器: {}", e))
}

/// 程序版本号
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 主函数入口
///
/// 程序的最顶层入口。
/// 调用 `run()` 执行实际逻辑，并在所有 Drop 实现执行完毕后传播退出码。
///
/// 设计原因：
/// `std::process::exit` 会立即终止进程，绕过当前栈帧上所有变量的 Drop 实现。
/// 为了避免这一问题，我们将所有真正持有资源的逻辑放在 `run()` 中，
/// `main()` 只在 `run()` 返回（栈帧已展开、Drop 已运行）后再调用 exit。
fn main() {
    // 调用 run() 执行实际逻辑，根据返回结果决定退出码：
    //   - Ok(Some(code))：子进程退出码
    //   - Ok(None)：默认成功（0）
    //   - Err(e)：打印错误并使用退出码 1
    let code = match run() {
        Ok(exit_code) => exit_code.unwrap_or(0),
        Err(e) => {
            eprintln!("[AC] 错误: {:#}", e);
            1
        }
    };
    // 此时 run() 已返回，其内部所有 Drop 已执行完毕，可安全调用 exit。
    std::process::exit(code as i32);
}

/// 程序实际逻辑入口
///
/// 解析命令行参数，启动CLI进程，并进入主循环监控状态。
/// 在函数返回时（无论成功或失败），所有局部变量的 Drop 都会被执行，
/// 终端模式恢复、IO 线程 join 等关键清理动作得以正常完成。
///
/// # 返回值
/// - `Ok(Some(code))`：CLI 子进程的退出码
/// - `Ok(None)`：未取得退出码，按 0 处理
/// - `Err(e)`：执行过程中遇到的错误
fn run() -> Result<Option<u32>> {
    let args = args::parse_args();
    let config = Config::from_args(&args).context("配置加载失败")?;

    // 创建检测器（避免 print_banner 中重复创建）
    let mut detector = create_detector(&config.cli);
    let detector_name = detector.name().to_string();

    print_banner(&config, &detector_name);

    let exit_flag = create_exit_flag();
    setup_ctrlc_handler(exit_flag.clone())?;

    // 初始化检测器
    detector.init(&config.cli, &config.cli_args)
        .context("检测器初始化失败")?;

    // 创建中断钩子管理器
    let mut hook_manager = StopHookManager::new();

    // 注册预设钩子（--stop-when）
    for spec in &config.stop_whens {
        let h = hook::parse_stop_when(spec)
            .with_context(|| format!("无法解析预设钩子 '{}'", spec))?;
        eprintln!("[AC] 注册预设钩子: {}", h.name());
        hook_manager.add(h);
    }

    // 注册自定义命令钩子（--stop-hook）
    for cmd in &config.stop_hooks {
        let h = hook::CommandHook::new(cmd)
            .with_context(|| format!("无法启动命令钩子 '{}'", cmd))?;
        eprintln!("[AC] 注册命令钩子: {}", h.name());
        hook_manager.add(Box::new(h));
    }

    // 向后兼容：--limit N 等价于 --stop-when "<round=N>"
    if config.limit >= 0 {
        hook_manager.add(Box::new(hook::RoundHook::new(config.limit as u64)));
        eprintln!("[AC] 注册轮次限制钩子: {} 次 (来自 --limit)", config.limit);
    }

    let exit_code = run_main_loop(config, exit_flag, detector, &detector_name, hook_manager)?;

    println!("\n[AC] 程序已退出");

    // 直接返回子进程退出码；由 main() 在 Drop 完成后调用 std::process::exit
    Ok(exit_code)
}

/// 打印启动横幅
///
/// # 参数
/// - `config`: 程序配置
/// - `detector_name`: 检测器名称（避免重复创建 Detector）
fn print_banner(config: &Config, detector_name: &str) {
    // 计算总等待时间
    let total_wait = config.silence_threshold + config.sleep_time;

    // 获取提示词显示内容（根据模式显示不同前缀）
    let prompt_display = if let Some(ref pipe_cmd) = config.continue_prompt_pipe {
        format!("[PIPE] {}", pipe_cmd)
    } else if let Some(ref io_path) = config.continue_prompt_io {
        format!("[IO] {}", io_path)
    } else {
        config.continue_prompt.clone()
    };

    // 获取轮次限制显示文本
    let limit_display = if config.limit < 0 {
        "无限制".to_string()
    } else {
        format!("{} 次", config.limit)
    };

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║           AutoContinue (AC) v{}                        ║", VERSION);
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  CLI: {:50} ║", config.cli);
    println!("║  检测器: {:47} ║", detector_name);
    println!("║  静默阈值: {:3} 秒 (用户设置)                             ║", config.silence_threshold);
    println!("║  额外等待: {:3} 秒 (用户设置)                             ║", config.sleep_time);
    println!("║  总等待:   {:3} 秒                                        ║", total_wait);
    println!("║  轮次限制: {:46} ║", limit_display);
    println!("║  继续提示词: {:44} ║", truncate_str(&prompt_display, 44));
    if config.is_continue_prompt_pipe() {
        println!("║  [PIPE模式] 每次使用时执行命令获取提示词                ║");
        if let Some((ref prefix, ref suffix)) = config.cformat {
            println!("║  [CFORMAT] {}...{}                           ║",
                truncate_str(prefix, 15), truncate_str(suffix, 15));
        }
    } else if config.is_continue_prompt_io() {
        println!("║  [IO模式] 每次使用时重新读取文件                          ║");
    }
    // 显示中断钩子信息
    if !config.stop_whens.is_empty() || !config.stop_hooks.is_empty() {
        let hook_count = config.stop_whens.len() + config.stop_hooks.len();
        println!("║  中断钩子: {} 个                                          ║", hook_count);
        for sw in &config.stop_whens {
            println!("║    [预设] {:46} ║", truncate_str(sw, 46));
        }
        for sh in &config.stop_hooks {
            println!("║    [命令] {:46} ║", truncate_str(sh, 46));
        }
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
///
/// # 边界处理
/// 当 `max_len <= 3` 时，无法在尾部追加省略号（"..." 本身就占 3 个字符），
/// 此时直接按 `max_len` 截取，避免 `max_len - 3` 在 `usize` 上发生下溢 panic。
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        // 字符串本身不超长，按目标宽度左对齐补空格
        format!("{:width$}", s, width = max_len)
    } else if max_len <= 3 {
        // max_len 太小无法附加 "..."，直接截取前 max_len 个字符
        s.chars().take(max_len).collect()
    } else {
        // 截取 (max_len - 3) 个字符，再拼上 "..." 保证总长度等于 max_len
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

/// 主运行循环
///
/// 该函数实现核心的无限循环逻辑：
/// 1. 启动 CLI 进程
/// 2. 持续查询 Detector 获取 CLI 状态
/// 3. 根据状态决定是否发送继续/重试提示词
/// 4. 对于 Unknown 状态，fallback 到静默超时检测
/// 5. 在每次自动发送前检查中断钩子（StopHook）
/// 6. 循环直到用户按 Ctrl+C、CLI 进程退出或中断钩子触发
///
/// # 参数
/// - `config`: 程序配置
/// - `exit_flag`: 退出标志
/// - `detector`: 已初始化的检测器
/// - `detector_name`: 检测器名称
/// - `hook_manager`: 中断钩子管理器，集中管理所有停止条件
///
/// # 返回值
/// 成功返回 Ok(子进程退出码)
fn run_main_loop(
    config: Config,
    exit_flag: Arc<AtomicBool>,
    detector: Box<dyn Detector>,
    detector_name: &str,
    mut hook_manager: StopHookManager,
) -> Result<Option<u32>> {
    // 计算总静默阈值：静默阈值 + 用户设置的等待时间
    let silence_threshold = Duration::from_secs(config.silence_threshold + config.sleep_time);

    // 自动继续计数器
    let mut auto_continue_count = 0u64;

    let shared_detector = Arc::new(Mutex::new(detector));

    println!("[AC] 正在启动: {} {}", config.cli, config.cli_args.join(" "));
    println!("[AC] 使用检测器: {}", detector_name);

    // 启动CLI进程（传入共享检测器）
    let mut runner = Runner::new(&config.cli, &config.cli_args, shared_detector.clone())?;

    // 启动双向IO转发（stdout和stdin）
    let io_handles = runner.start_io_forwarding()?;

    println!("[AC] CLI已启动，开始监控状态...");
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

        // 查询 Detector 获取 CLI 状态
        // 使用 unwrap_or_else 处理 Mutex 中毒：即使线程 panic，也恢复内部值继续运行
        let status = {
            let det = shared_detector.lock().unwrap_or_else(|p| p.into_inner());
            det.status(silence_duration, silence_threshold)
        };

        // 根据状态决定行为
        match status {
            CliStatus::Busy => {
                // CLI 正在工作，不干预
            }

            CliStatus::Idle => {
                // Detector 确认 CLI 空闲，发送继续 prompt

                // 检查中断钩子：在发送继续提示词之前判断是否应当停止
                if hook_manager.should_stop(auto_continue_count, &status) {
                    break;
                }

                auto_continue_count += 1;

                let prompt = match config.get_continue_prompt() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[AC] 获取继续提示词失败: {:#}", e);
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };

                println!("\n[AC] === [{}] 检测到空闲状态，自动发送第 {} 次继续提示词 ===",
                    detector_name, auto_continue_count);
                println!("[AC] 发送: {}", prompt);

                if let Err(e) = runner.send_line(&prompt) {
                    eprintln!("[AC] 发送提示词失败: {}", e);
                }

                // 重置检测器状态
                shared_detector.lock().unwrap_or_else(|p| p.into_inner()).reset();
            }

            CliStatus::Error { ref message } => {
                // Detector 检测到错误，发送重试 prompt

                // 检查中断钩子：在发送重试提示词之前判断是否应当停止
                if hook_manager.should_stop(auto_continue_count, &status) {
                    break;
                }

                auto_continue_count += 1;

                let prompt = match config.get_retry_prompt() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[AC] 获取重试提示词失败: {:#}", e);
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };

                println!("\n[AC] === [{}] 检测到错误，自动发送第 {} 次重试提示词 ===",
                    detector_name, auto_continue_count);
                if !message.is_empty() {
                    let display_msg: String = message.chars().take(80).collect();
                    println!("[AC] 错误信息: {}", display_msg);
                }
                println!("[AC] 发送: {}", prompt);

                if let Err(e) = runner.send_line(&prompt) {
                    eprintln!("[AC] 发送提示词失败: {}", e);
                }

                // 重置检测器状态
                shared_detector.lock().unwrap_or_else(|p| p.into_inner()).reset();
            }

            CliStatus::Unknown => {
                // Detector 无法确定状态，fallback 到传统静默超时检测
                if silence_duration >= silence_threshold {
                    // 检查中断钩子：在 fallback 发送继续提示词之前判断是否应当停止
                    if hook_manager.should_stop(auto_continue_count, &status) {
                        break;
                    }

                    auto_continue_count += 1;

                    let prompt = match config.get_continue_prompt() {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("[AC] 获取继续提示词失败: {:#}", e);
                            thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    };

                    println!("\n[AC] === 静默 {} 秒，自动发送第 {} 次继续提示词 (fallback) ===",
                        silence_duration.as_secs(), auto_continue_count);
                    println!("[AC] 发送: {}", prompt);

                    if let Err(e) = runner.send_line(&prompt) {
                        eprintln!("[AC] 发送提示词失败: {}", e);
                    }

                    // 重置检测器状态（与其他分支保持一致：Mutex 中毒时恢复内部值继续运行）
                    shared_detector.lock().unwrap_or_else(|p| p.into_inner()).reset();
                }
            }
        }

        // 短暂休眠避免忙等待（每500ms检查一次）
        thread::sleep(Duration::from_millis(500));
    }

    // 停止 IO 转发并恢复终端模式
    runner.stop();

    // 等待 IO 线程干净退出
    io_handles.join();

    // 获取子进程退出码
    let exit_code = runner.get_exit_code();

    println!("[AC] 共自动发送了 {} 次提示词", auto_continue_count);

    Ok(exit_code)
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
