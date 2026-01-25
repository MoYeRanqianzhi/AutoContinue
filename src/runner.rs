//! # CLI运行器模块 (runner.rs)
//!
//! 该模块负责启动和管理CLI子进程，使用伪终端（PTY）来保持CLI的完整交互性。
//!
//! ## 功能
//! - 使用portable-pty启动CLI进程
//! - 保持stdin/stdout的完整传递
//! - 确保用户可以正常操作CLI
//! - 支持向CLI发送输入（提示词）
//!
//! ## 跨平台支持
//! - Windows: 使用ConPTY
//! - Unix/Linux/macOS: 使用传统PTY

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// CLI运行器
///
/// 负责启动CLI进程并管理其生命周期。
/// 使用PTY来保持CLI的完整交互性。
pub struct Runner {
    /// PTY pair（主端和从端）
    pty_pair: PtyPair,

    /// PTY写入器，用于向CLI发送输入
    writer: Box<dyn Write + Send>,

    /// 子进程句柄
    child: Box<dyn portable_pty::Child + Send + Sync>,

    /// 标志：进程是否正在运行
    running: Arc<AtomicBool>,

    /// 子进程的退出状态
    exit_status: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
}

impl Runner {
    /// 创建并启动CLI运行器
    ///
    /// # 参数
    /// - `cli`: CLI程序名称
    /// - `args`: CLI程序参数
    ///
    /// # 返回值
    /// 成功返回Runner实例，失败返回错误
    ///
    /// # 错误
    /// - 无法创建PTY
    /// - 无法启动CLI进程
    ///
    /// # 示例
    /// ```
    /// let runner = Runner::new("claude", &["--resume"])?;
    /// ```
    pub fn new(cli: &str, args: &[String]) -> Result<Self> {
        // 获取原生PTY系统
        let pty_system = native_pty_system();

        // 获取终端大小，如果失败则使用默认值
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

        // 创建PTY对，指定终端大小
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("无法创建PTY")?;

        // 构建命令
        let mut cmd = CommandBuilder::new(cli);
        cmd.args(args);

        // 在从端启动子进程
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

        Ok(Runner {
            pty_pair: pair,
            writer,
            child,
            running,
            exit_status,
        })
    }

    /// 启动IO转发线程
    ///
    /// 该方法启动后台线程，将PTY的输出转发到stdout，
    /// 将stdin转发到PTY，实现完整的交互性。
    ///
    /// # 参数
    /// - `running`: 共享的运行状态标志
    ///
    /// # 返回值
    /// 返回输出转发线程的句柄
    pub fn start_io_forwarding(&mut self) -> Result<thread::JoinHandle<()>> {
        let running = self.running.clone();

        // 获取PTY读取器
        let mut reader = self
            .pty_pair
            .master
            .try_clone_reader()
            .context("无法克隆PTY读取器")?;

        // 启动输出转发线程：PTY -> stdout
        let output_handle = thread::spawn(move || {
            let mut stdout = std::io::stdout();
            let mut buffer = [0u8; 4096];

            while running.load(Ordering::SeqCst) {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        // EOF，进程已结束
                        break;
                    }
                    Ok(n) => {
                        // 将数据写入stdout
                        if stdout.write_all(&buffer[..n]).is_err() {
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

        Ok(output_handle)
    }

    /// 向CLI发送输入
    ///
    /// # 参数
    /// - `input`: 要发送的输入字符串
    ///
    /// # 返回值
    /// 成功返回Ok(())，失败返回错误
    ///
    /// # 示例
    /// ```
    /// runner.send_input("继续\n")?;
    /// ```
    pub fn send_input(&mut self, input: &str) -> Result<()> {
        // 写入输入
        self.writer
            .write_all(input.as_bytes())
            .context("无法向CLI发送输入")?;

        // 确保数据被刷新
        self.writer.flush().context("无法刷新输入缓冲区")?;

        Ok(())
    }

    /// 向CLI发送一行输入（自动添加换行符）
    ///
    /// # 参数
    /// - `line`: 要发送的输入行
    ///
    /// # 返回值
    /// 成功返回Ok(())，失败返回错误
    pub fn send_line(&mut self, line: &str) -> Result<()> {
        let input = format!("{}\n", line);
        self.send_input(&input)
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
                *self.exit_status.lock().unwrap() = Some(status);
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

    /// 等待CLI进程结束
    ///
    /// # 返回值
    /// 返回进程的退出状态
    pub fn wait(&mut self) -> Result<portable_pty::ExitStatus> {
        let status = self.child.wait().context("等待CLI进程结束失败")?;
        self.running.store(false, Ordering::SeqCst);
        *self.exit_status.lock().unwrap() = Some(status.clone());
        Ok(status)
    }

    /// 获取进程退出状态
    ///
    /// # 返回值
    /// 如果进程已退出，返回Some(ExitStatus)，否则返回None
    pub fn get_exit_status(&self) -> Option<portable_pty::ExitStatus> {
        self.exit_status.lock().unwrap().clone()
    }

    /// 获取运行状态标志的克隆
    ///
    /// # 返回值
    /// 返回运行状态的Arc引用
    pub fn get_running_flag(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }

    /// 停止运行标志
    ///
    /// 设置运行标志为false，通知所有相关线程停止
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// 检查退出状态是否表示成功
///
/// # 参数
/// - `status`: 进程退出状态
///
/// # 返回值
/// 如果退出码为0返回true，否则返回false
pub fn is_success(status: &portable_pty::ExitStatus) -> bool {
    status.success()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试简单命令执行
    #[test]
    #[cfg(target_os = "windows")]
    fn test_simple_command() -> Result<()> {
        let mut runner = Runner::new("cmd", &["/c".to_string(), "echo".to_string(), "hello".to_string()])?;

        // 等待进程结束
        let status = runner.wait()?;
        assert!(is_success(&status));

        Ok(())
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_simple_command() -> Result<()> {
        let mut runner = Runner::new("echo", &["hello".to_string()])?;

        // 等待进程结束
        let status = runner.wait()?;
        assert!(is_success(&status));

        Ok(())
    }
}
