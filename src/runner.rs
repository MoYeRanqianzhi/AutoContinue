//! # CLI运行器模块 (runner.rs)
//!
//! 该模块负责启动和管理CLI子进程，使用伪终端（PTY）来保持CLI的完整交互性。
//!
//! ## 功能
//! - 使用portable-pty启动CLI进程
//! - 双向IO转发：stdin -> PTY，PTY -> stdout
//! - 跟踪最后活动时间（输入/输出）用于静默检测
//! - 输出数据同时发送给 Detector 进行状态/错误分析
//! - 确保用户可以正常操作CLI
//!
//! ## 跨平台支持
//! - Windows: 使用ConPTY
//! - Unix/Linux/macOS: 使用传统PTY
//!
//! ## 与旧版本的区别
//! - 移除了虚拟终端（VirtualTerminal）ANSI 解析
//! - 输出直接转发到 stdout，不做任何解析/修改
//! - 状态/错误检测委托给 Detector 模块

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::detector::Detector;

/// 共享检测器类型别名
///
/// Detector trait 对象被 Arc<Mutex<>> 包装以支持多线程访问：
/// - 输出线程：调用 feed_output() 投喂数据
/// - 主线程：调用 status() 查询状态、reset() 重置
pub type SharedDetector = Arc<Mutex<Box<dyn Detector>>>;

/// 终端 raw mode 的 RAII 守卫
///
/// 确保无论以何种方式退出（正常返回、panic、提前 return），
/// 终端的 raw mode 都会被正确恢复。
///
/// # 设计动机
/// 在 `start_io_forwarding()` 中如果只在 `stop()` 中调用 `disable_raw_mode()`，
/// 当主线程发生 panic 或通过 `?` 提前返回时，终端会留在 raw mode 状态，
/// 导致用户终端不可用。使用 RAII 守卫将清理逻辑绑定到对象生命周期，
/// 即使发生 panic，Rust 也会保证 Drop 被调用，从而恢复终端状态。
struct RawModeGuard;

impl RawModeGuard {
    /// 启用 raw mode 并返回守卫
    ///
    /// # 返回值
    /// 成功返回守卫实例（被持有期间 raw mode 保持启用），
    /// 失败返回错误（raw mode 启用失败）。
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()
            .context("无法启用终端 raw mode")?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    /// 守卫被丢弃时自动恢复终端模式
    ///
    /// 即使 `disable_raw_mode()` 失败也忽略错误，
    /// 因为在 Drop 中无法传播错误，并且在程序退出时
    /// 终端状态通常会被 shell/OS 自然恢复。
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// IO 转发线程句柄
///
/// 包含输出和输入转发线程的句柄，以及 raw mode 守卫。
/// 调用方在 stop() 后应调用 join() 等待线程退出。
///
/// # 字段说明
/// - `output_handle` / `input_handle`：两个 IO 转发线程
/// - `_raw_mode_guard`：RAII 守卫，IoHandles 被丢弃时自动恢复终端 raw mode
pub struct IoHandles {
    /// 输出转发线程句柄（PTY -> stdout + Detector）
    pub output_handle: thread::JoinHandle<()>,
    /// 输入转发线程句柄（stdin -> PTY）
    pub input_handle: thread::JoinHandle<()>,
    /// 终端 raw mode 守卫
    ///
    /// 字段名前缀下划线表示该字段不会被显式访问，
    /// 但其 Drop 实现会在 IoHandles 被丢弃时自动运行，恢复终端模式。
    /// 必须放在最后，使其在线程句柄之后被 drop（其实顺序在此处不重要，
    /// 因为我们在 join 内已主动消耗所有字段）。
    _raw_mode_guard: RawModeGuard,
}

impl IoHandles {
    /// 等待两个 IO 线程退出
    ///
    /// 在 runner.stop() 之后调用，确保线程干净退出。
    ///
    /// # ConPTY 阻塞问题
    /// 在 Windows 上使用 ConPTY 时，PTY 主端可能不会产生 EOF，
    /// 导致输出线程的 `reader.read()` 永久阻塞。由于 `std::thread::JoinHandle`
    /// 不支持原生超时 join，这里采用 channel + 辅助线程的方式实现超时：
    /// - 派生一个辅助线程持有 JoinHandle 并通过 channel 通知主线程
    /// - 主线程使用 `recv_timeout` 等待，超时后放弃 join（线程随进程退出回收）
    ///
    /// # 超时策略
    /// - 输出线程：等待最多 3 秒（ConPTY 可能不产生 EOF）
    /// - 输入线程：等待最多 1 秒（poll 超时为 50ms，应能快速退出）
    pub fn join(self) {
        // 创建用于接收线程退出通知的 channel
        // 一个发送端给输出线程的辅助线程，一个克隆给输入线程的辅助线程
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();

        // 输出线程：可能因 ConPTY 不产生 EOF 而阻塞在 read()
        // 派生辅助线程持有 join handle，超时后放弃
        let output_handle = self.output_handle;
        std::thread::spawn(move || {
            let _ = output_handle.join();
            let _ = tx.send(());
        });
        if rx.recv_timeout(std::time::Duration::from_secs(3)).is_err() {
            eprintln!("[AC] 警告: 输出线程未在 3 秒内退出，可能因 ConPTY 限制阻塞");
        }

        // 输入线程：理论上应在 50ms poll 超时后快速退出
        // 仍设置 1 秒超时作为兜底
        let input_handle = self.input_handle;
        std::thread::spawn(move || {
            let _ = input_handle.join();
            let _ = tx2.send(());
        });
        if rx.recv_timeout(std::time::Duration::from_secs(1)).is_err() {
            eprintln!("[AC] 警告: 输入线程未在 1 秒内退出");
        }

        // self._raw_mode_guard 在此函数结束时被 drop，自动恢复终端模式
    }
}

/// CLI运行器
///
/// 负责启动CLI进程并管理其生命周期。
/// 使用PTY来保持CLI的完整交互性。
/// 跟踪最后活动时间用于静默检测。
/// 输出数据同时发送给 Detector 进行分析。
pub struct Runner {
    /// PTY pair（主端和从端）
    pty_pair: PtyPair,

    /// PTY写入器，用于向CLI发送输入（共享，用于多线程访问）
    writer: Arc<Mutex<Box<dyn Write + Send>>>,

    /// 子进程句柄
    child: Box<dyn portable_pty::Child + Send + Sync>,

    /// 标志：进程是否正在运行
    running: Arc<AtomicBool>,

    /// 子进程的退出状态
    exit_status: Arc<Mutex<Option<portable_pty::ExitStatus>>>,

    /// 最后活动时间（输入或输出）
    /// 用于检测CLI是否处于静默状态（等待输入）
    last_activity_time: Arc<Mutex<Instant>>,

    /// 用于注入输入到输入线程的 channel（发送端）
    /// 通过这个 channel 发送的数据会被输入线程当作用户输入处理
    inject_sender: Option<Sender<Vec<u8>>>,

    /// 共享检测器，用于接收输出数据并进行状态分析
    detector: SharedDetector,
}

impl Runner {
    /// 创建并启动CLI运行器
    ///
    /// # 参数
    /// - `cli`: CLI程序名称
    /// - `args`: CLI程序参数
    /// - `detector`: 检测器实例（已初始化）
    ///
    /// # 返回值
    /// 成功返回Runner实例，失败返回错误
    ///
    /// # 错误
    /// - 无法创建PTY
    /// - 无法启动CLI进程
    pub fn new(cli: &str, args: &[String], detector: SharedDetector) -> Result<Self> {
        // 获取原生PTY系统
        let pty_system = native_pty_system();

        // 获取终端大小，如果失败则使用默认值
        let (cols, rows) = terminal::size().unwrap_or((80, 24));

        // 创建PTY对，指定终端大小
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("无法创建PTY")?;

        // 获取当前工作目录
        let current_dir = std::env::current_dir().context("无法获取当前工作目录")?;

        // 构建命令
        // 在Windows上，通过cmd.exe执行以支持.cmd/.bat脚本
        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = CommandBuilder::new("cmd.exe");
            c.arg("/c");
            c.arg(cli);
            c.args(args);
            c.cwd(&current_dir);
            c
        };

        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = CommandBuilder::new(cli);
            c.args(args);
            c.cwd(&current_dir);
            c
        };

        // 在从端启动子进程
        // 子进程通过 spawn_command 内部继承 slave 端 fd（在 fork 后由子进程持有），
        // 父进程理论上应在 spawn 后释放 slave 端，使得当子进程退出时主端能收到 EOF。
        //
        // 但 portable-pty 的 PtyPair 结构体同时拥有 master 与 slave 字段，
        // 无法在不解构整个 pair 的情况下单独 drop slave。
        // 这是 portable-pty API 的设计限制。
        //
        // 影响：在某些平台上，主端可能不会因子进程退出而收到 EOF，
        //       从而需要依赖 IoHandles::join() 中的超时机制兜底。
        // 释放：slave 会在 Runner（拥有 PtyPair）被 drop 时随之释放。
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("无法启动CLI进程")?;

        // 获取写入器用于发送输入
        let writer = pair
            .master
            .take_writer()
            .context("无法获取PTY写入器")?;

        let running = Arc::new(AtomicBool::new(true));
        let exit_status = Arc::new(Mutex::new(None));
        let last_activity_time = Arc::new(Mutex::new(Instant::now()));

        Ok(Runner {
            pty_pair: pair,
            writer: Arc::new(Mutex::new(writer)),
            child,
            running,
            exit_status,
            last_activity_time,
            inject_sender: None,
            detector,
        })
    }

    /// 启动双向IO转发线程
    ///
    /// 该方法启动两个后台线程：
    /// 1. 输出转发：PTY -> stdout（直接转发，不解析）+ Detector 数据投喂
    /// 2. 输入转发：stdin -> PTY（用户输入到CLI）
    ///
    /// 每次有输入或输出时，都会更新最后活动时间。
    /// 输出数据同时被发送给 Detector 进行状态分析。
    ///
    /// # 返回值
    /// 返回包含两个线程句柄的IoHandles结构
    pub fn start_io_forwarding(&mut self) -> Result<IoHandles> {
        // 启用终端原始模式，以便直接获取用户输入
        //
        // 使用 RawModeGuard 实现 RAII：
        // - 启用失败时通过 `?` 直接返回错误（解决 [M5] 返回值被忽略的问题）
        // - 守卫被存入 IoHandles 后，会在 IoHandles drop 时自动恢复终端模式
        // - 即使主线程 panic 或提前 return，Drop 仍会执行，避免终端处于 raw mode
        let raw_mode_guard = RawModeGuard::enable()?;

        let running_output = self.running.clone();
        let running_input = self.running.clone();

        // 获取最后活动时间的共享引用
        let last_activity_output = self.last_activity_time.clone();
        let last_activity_input = self.last_activity_time.clone();

        // 获取PTY读取器
        let mut reader = self
            .pty_pair
            .master
            .try_clone_reader()
            .context("无法克隆PTY读取器")?;

        // 获取PTY写入器的共享引用
        let writer = self.writer.clone();
        let writer_for_inject = self.writer.clone();

        // 创建用于注入输入的 channel
        let (inject_tx, inject_rx) = mpsc::channel::<Vec<u8>>();
        self.inject_sender = Some(inject_tx);

        // 获取检测器的共享引用
        let detector = self.detector.clone();

        // 启动输出转发线程：PTY -> stdout + Detector
        // 输出数据直接转发到 stdout（不经过 ANSI 解析），
        // 同时发送给 Detector 进行分析
        let output_handle = thread::spawn(move || {
            let mut stdout = std::io::stdout();
            let mut buffer = [0u8; 4096];

            while running_output.load(Ordering::SeqCst) {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        // EOF，进程已结束
                        break;
                    }
                    Ok(n) => {
                        let data = &buffer[..n];

                        // 更新最后活动时间（有输出）
                        if let Ok(mut time) = last_activity_output.lock() {
                            *time = Instant::now();
                        }

                        // 将数据发送给 Detector 进行分析
                        if let Ok(mut det) = detector.lock() {
                            det.feed_output(data);
                        }

                        // 将数据直接写入 stdout（不做任何解析/修改）
                        if stdout.write_all(data).is_err() {
                            break;
                        }
                        let _ = stdout.flush();
                    }
                    Err(e) => {
                        // 检查是否是非阻塞读取导致的暂时性错误
                        if e.kind() != std::io::ErrorKind::WouldBlock {
                            break;
                        }
                        // 短暂休眠避免忙等待
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        });

        // 启动输入转发线程：stdin -> PTY
        // 使用crossterm的event系统正确处理特殊键（方向键、ESC等）
        // 每次有输入时更新最后活动时间
        // 同时检查注入的输入（通过channel）
        let input_handle = thread::spawn(move || {
            while running_input.load(Ordering::SeqCst) {
                // 首先检查是否有注入的输入
                if let Ok(bytes) = inject_rx.try_recv() {
                    // 更新最后活动时间
                    if let Ok(mut time) = last_activity_input.lock() {
                        *time = Instant::now();
                    }
                    // 发送注入的输入
                    if let Ok(mut w) = writer_for_inject.lock() {
                        if w.write_all(&bytes).is_err() {
                            break;
                        }
                        let _ = w.flush();
                    }
                }

                // 使用crossterm的event poll来非阻塞检测输入
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => {
                        // 有事件可读，使用crossterm读取事件
                        match crossterm::event::read() {
                            Ok(event) => {
                                // 将事件转换为字节序列
                                if let Some(bytes) = event_to_bytes(&event) {
                                    // 更新最后活动时间（有输入）
                                    if let Ok(mut time) = last_activity_input.lock() {
                                        *time = Instant::now();
                                    }

                                    // 将数据写入PTY
                                    if let Ok(mut w) = writer.lock() {
                                        if w.write_all(&bytes).is_err() {
                                            break;
                                        }
                                        let _ = w.flush();
                                    }
                                }
                            }
                            Err(_) => {
                                // 读取事件出错，短暂休眠后继续
                                thread::sleep(Duration::from_millis(10));
                            }
                        }
                    }
                    Ok(false) => {
                        // 没有事件，继续循环
                    }
                    Err(_) => {
                        // poll出错，短暂休眠后继续
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        });

        Ok(IoHandles {
            output_handle,
            input_handle,
            // 将 raw mode 守卫转移给 IoHandles
            // 当 IoHandles 被 drop 时，守卫的 Drop 实现会自动恢复终端模式
            _raw_mode_guard: raw_mode_guard,
        })
    }

    /// 向CLI发送输入
    ///
    /// # 参数
    /// - `input`: 要发送的输入字符串
    ///
    /// # 返回值
    /// 成功返回Ok(())，失败返回错误
    pub fn send_input(&mut self, input: &str) -> Result<()> {
        let mut writer = self.writer.lock().map_err(|_| anyhow::anyhow!("无法获取写入器锁"))?;

        // 更新最后活动时间（程序发送输入也算活动）
        if let Ok(mut time) = self.last_activity_time.lock() {
            *time = Instant::now();
        }

        // 写入输入
        writer
            .write_all(input.as_bytes())
            .context("无法向CLI发送输入")?;

        // 确保数据被刷新
        writer.flush().context("无法刷新输入缓冲区")?;

        Ok(())
    }

    /// 向CLI发送一行输入（自动添加回车）
    ///
    /// # 参数
    /// - `line`: 要发送的输入行
    ///
    /// # 返回值
    /// 成功返回Ok(())，失败返回错误
    ///
    /// # 实现说明
    /// 某些 CLI（如 codex）有"粘贴突发"检测机制，会把快速输入后的 Enter
    /// 当作文本累积而不是提交。codex 的 PASTE_ENTER_SUPPRESS_WINDOW 为 120ms，
    /// 即粘贴活动结束后 120ms 内 Enter 仍被当作换行。
    /// 因此在发送文本后等待 150ms 再发送回车，确保粘贴窗口完全关闭。
    pub fn send_line(&mut self, line: &str) -> Result<()> {
        // 发送文本内容（直接写入PTY）
        self.send_input(line)?;

        // 等待 150ms，确保超过 codex 的 PASTE_ENTER_SUPPRESS_WINDOW (120ms)
        // 这样 Enter 会被正确识别为提交而不是换行
        thread::sleep(Duration::from_millis(150));

        // 发送回车通过 channel，让输入线程发送
        // 这样回车和用户按Enter走同样的路径
        if let Some(ref sender) = self.inject_sender {
            if sender.send(vec![b'\r']).is_err() {
                eprintln!("[AC] 警告: 输入注入 channel 已关闭，回车可能未发送");
            }
        }

        Ok(())
    }

    /// 获取自上次活动以来的静默时间
    ///
    /// # 返回值
    /// 返回自上次输入/输出以来经过的时间。
    /// 锁失败时返回极大值（24小时），确保不会误判为"刚有活动"。
    pub fn get_silence_duration(&self) -> Duration {
        if let Ok(time) = self.last_activity_time.lock() {
            time.elapsed()
        } else {
            eprintln!("[AC] 警告: 活动时间锁中毒，返回最大静默时间");
            Duration::from_secs(86400)
        }
    }

    /// 检查CLI进程是否仍在运行
    ///
    /// # 返回值
    /// 如果进程仍在运行返回true，否则返回false
    pub fn is_running(&mut self) -> bool {
        // 尝试获取进程状态（非阻塞）
        match self.child.try_wait() {
            Ok(Some(status)) => {
                // 进程已退出
                self.running.store(false, Ordering::SeqCst);
                // 与 get_exit_code() 等其他方法保持一致：
                // 即使锁中毒（其他线程在持锁时 panic），也通过 into_inner() 恢复内部值，
                // 避免因 unwrap() panic 导致整个清理流程崩溃。
                // 这里我们刚要写入新值，PoisonError 暴露的旧值（None 或上次结果）会被立即覆盖。
                let mut guard = self
                    .exit_status
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                *guard = Some(status);
                false
            }
            Ok(None) => {
                // 进程仍在运行
                true
            }
            Err(_) => {
                // 出错，假设进程已结束
                self.running.store(false, Ordering::SeqCst);
                false
            }
        }
    }

    /// 停止运行标志，通知所有相关 IO 线程优雅退出
    ///
    /// 仅设置运行标志为 false。两个 IO 线程会在下次循环检查时退出。
    ///
    /// # 终端模式恢复
    /// 终端 raw mode 的恢复由 `IoHandles` 持有的 `RawModeGuard` 在 drop 时
    /// 自动完成（RAII），不再在此处显式调用 `disable_raw_mode()`。
    /// 这同时解决了：
    /// - 主线程 panic 时终端无法恢复的问题
    /// - 通过 `?` 提前返回时终端无法恢复的问题
    ///
    /// # 不再需要 sleep
    /// 旧实现中需要 100ms sleep 是为了避免在输入线程仍在读取的情况下
    /// 立即调用 `disable_raw_mode()` 导致终端状态异常。
    /// 现在恢复时机由 `IoHandles::join()` 后的 Drop 控制，
    /// 而 join 内已包含等待和超时机制，无需再延时。
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// 获取子进程退出码
    ///
    /// # 返回值
    /// 子进程已退出时返回 Some(退出码)，仍在运行或无法获取时返回 None
    pub fn get_exit_code(&self) -> Option<u32> {
        if let Ok(status) = self.exit_status.lock() {
            status.as_ref().map(|s| s.exit_code())
        } else {
            None
        }
    }
}

/// 将crossterm事件转换为PTY可接受的字节序列
///
/// 该函数处理键盘事件和粘贴事件，将它们转换为
/// 相应的ANSI转义序列或UTF-8字节。
/// 鼠标事件暂不支持。
///
/// # 参数
/// - `event`: crossterm事件
///
/// # 返回值
/// 如果是支持的事件类型，返回对应的字节序列；否则返回None
fn event_to_bytes(event: &Event) -> Option<Vec<u8>> {
    match event {
        Event::Key(key_event) => key_event_to_bytes(key_event),
        Event::Paste(text) => {
            // 粘贴事件，直接返回文本的UTF-8字节
            Some(text.as_bytes().to_vec())
        }
        _ => None, // 忽略鼠标事件和其他事件
    }
}

/// 将键盘事件转换为字节序列
///
/// # 参数
/// - `key_event`: 键盘事件
///
/// # 返回值
/// 返回对应的字节序列
///
/// # 注意
/// 只处理 KeyEventKind::Press 事件，忽略 Release 事件
/// 这是因为Windows上crossterm会同时报告按下和释放事件
fn key_event_to_bytes(key_event: &KeyEvent) -> Option<Vec<u8>> {
    let KeyEvent { code, modifiers, kind, .. } = key_event;

    // 只处理按键按下事件，忽略释放事件（避免重复输入）
    // Windows上crossterm会报告Press和Release两个事件
    if *kind != KeyEventKind::Press && *kind != KeyEventKind::Repeat {
        return None;
    }

    // 处理Ctrl组合键
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            // 特殊的Ctrl组合键
            KeyCode::Char('[') => Some(vec![0x1B]), // Ctrl+[ = ESC
            KeyCode::Char('\\') => Some(vec![0x1C]),
            KeyCode::Char(']') => Some(vec![0x1D]),
            KeyCode::Char('^') => Some(vec![0x1E]),
            KeyCode::Char('_') => Some(vec![0x1F]),
            // Ctrl+A 到 Ctrl+Z 映射到 0x01 到 0x1A
            KeyCode::Char(c) => {
                let ctrl_char = (*c as u8).to_ascii_lowercase();
                if ctrl_char >= b'a' && ctrl_char <= b'z' {
                    Some(vec![ctrl_char - b'a' + 1])
                } else {
                    None
                }
            }
            _ => None,
        };
    }

    // 处理普通键和特殊键
    match code {
        // 普通字符
        KeyCode::Char(c) => Some(c.to_string().into_bytes()),

        // 回车键 - 发送 \r
        KeyCode::Enter => Some(vec![b'\r']),

        // 退格键
        KeyCode::Backspace => Some(vec![0x7F]), // DEL

        // Tab键
        KeyCode::Tab => Some(vec![b'\t']),

        // ESC键
        KeyCode::Esc => Some(vec![0x1B]),

        // 方向键（ANSI转义序列）
        KeyCode::Up => Some(vec![0x1B, b'[', b'A']),
        KeyCode::Down => Some(vec![0x1B, b'[', b'B']),
        KeyCode::Right => Some(vec![0x1B, b'[', b'C']),
        KeyCode::Left => Some(vec![0x1B, b'[', b'D']),

        // Home/End键
        KeyCode::Home => Some(vec![0x1B, b'[', b'H']),
        KeyCode::End => Some(vec![0x1B, b'[', b'F']),

        // Insert/Delete键
        KeyCode::Insert => Some(vec![0x1B, b'[', b'2', b'~']),
        KeyCode::Delete => Some(vec![0x1B, b'[', b'3', b'~']),

        // Page Up/Down键
        KeyCode::PageUp => Some(vec![0x1B, b'[', b'5', b'~']),
        KeyCode::PageDown => Some(vec![0x1B, b'[', b'6', b'~']),

        // 功能键 F1-F12
        KeyCode::F(1) => Some(vec![0x1B, b'O', b'P']),
        KeyCode::F(2) => Some(vec![0x1B, b'O', b'Q']),
        KeyCode::F(3) => Some(vec![0x1B, b'O', b'R']),
        KeyCode::F(4) => Some(vec![0x1B, b'O', b'S']),
        KeyCode::F(5) => Some(vec![0x1B, b'[', b'1', b'5', b'~']),
        KeyCode::F(6) => Some(vec![0x1B, b'[', b'1', b'7', b'~']),
        KeyCode::F(7) => Some(vec![0x1B, b'[', b'1', b'8', b'~']),
        KeyCode::F(8) => Some(vec![0x1B, b'[', b'1', b'9', b'~']),
        KeyCode::F(9) => Some(vec![0x1B, b'[', b'2', b'0', b'~']),
        KeyCode::F(10) => Some(vec![0x1B, b'[', b'2', b'1', b'~']),
        KeyCode::F(11) => Some(vec![0x1B, b'[', b'2', b'3', b'~']),
        KeyCode::F(12) => Some(vec![0x1B, b'[', b'2', b'4', b'~']),

        // 其他未处理的键
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector;

    /// 测试简单命令执行
    #[test]
    #[cfg(target_os = "windows")]
    fn test_simple_command() -> Result<()> {
        let det = Arc::new(Mutex::new(detector::create_detector("test")));
        let mut runner = Runner::new("cmd", &["/c".to_string(), "echo".to_string(), "hello".to_string()], det)?;

        // 等待进程结束
        let status = runner.child.wait().context("等待进程失败")?;
        assert!(status.success());

        Ok(())
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_simple_command() -> Result<()> {
        let det = Arc::new(Mutex::new(detector::create_detector("test")));
        let mut runner = Runner::new("echo", &["hello".to_string()], det)?;

        // 等待进程结束
        let status = runner.child.wait().context("等待进程失败")?;
        assert!(status.success());

        Ok(())
    }

    /// 测试静默时间检测
    #[test]
    fn test_silence_duration() -> Result<()> {
        let det = Arc::new(Mutex::new(detector::create_detector("test")));
        let runner = Runner::new("cmd", &["/c".to_string(), "echo".to_string(), "test".to_string()], det)?;

        // 刚创建时静默时间应该很短
        let duration = runner.get_silence_duration();
        assert!(duration < Duration::from_secs(1));

        // 等待一秒
        thread::sleep(Duration::from_secs(1));

        // 静默时间应该增加
        let duration = runner.get_silence_duration();
        assert!(duration >= Duration::from_secs(1));

        Ok(())
    }
}
