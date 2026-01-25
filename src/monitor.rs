//! # 状态监控模块 (monitor.rs)
//!
//! 该模块负责监控CLI进程的运行状态，包括：
//! - 检测进程退出（正常结束/错误）
//! - 实现等待计时器
//! - 检测用户手动恢复
//!
//! ## 状态流程
//! ```
//! Running -> Stopped -> WaitingUser -> AutoAction
//!                   |-> UserResumed -> Running
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 进程状态枚举
///
/// 表示CLI进程的当前状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ProcessState {
    /// 进程正在运行
    Running,

    /// 进程已停止（正常结束，退出码为0）
    StoppedNormal,

    /// 进程已停止（出错，退出码非0）
    StoppedError,

    /// 等待用户响应中
    WaitingUser,

    /// 用户已手动恢复
    UserResumed,
}

/// 状态监控器
///
/// 负责监控CLI进程状态并管理等待计时器。
pub struct Monitor {
    /// 当前进程状态
    state: ProcessState,

    /// 等待时间（秒）
    wait_duration: Duration,

    /// 等待开始时间
    wait_start: Option<Instant>,

    /// 全局退出标志（Ctrl+C）
    exit_flag: Arc<AtomicBool>,

    /// 进程运行状态标志
    running_flag: Option<Arc<AtomicBool>>,
}

#[allow(dead_code)]
impl Monitor {
    /// 创建新的监控器
    ///
    /// # 参数
    /// - `wait_seconds`: 等待时间（秒）
    /// - `exit_flag`: 全局退出标志
    ///
    /// # 返回值
    /// 返回新的Monitor实例
    ///
    /// # 示例
    /// ```
    /// let exit_flag = Arc::new(AtomicBool::new(false));
    /// let monitor = Monitor::new(15, exit_flag);
    /// ```
    pub fn new(wait_seconds: u64, exit_flag: Arc<AtomicBool>) -> Self {
        Monitor {
            state: ProcessState::Running,
            wait_duration: Duration::from_secs(wait_seconds),
            wait_start: None,
            exit_flag,
            running_flag: None,
        }
    }

    /// 设置进程运行标志
    ///
    /// # 参数
    /// - `flag`: 进程运行状态的原子布尔值引用
    pub fn set_running_flag(&mut self, flag: Arc<AtomicBool>) {
        self.running_flag = Some(flag);
    }

    /// 获取当前状态
    ///
    /// # 返回值
    /// 返回当前进程状态
    pub fn get_state(&self) -> ProcessState {
        self.state
    }

    /// 更新状态为停止（正常结束）
    ///
    /// 当CLI进程正常退出时调用此方法
    pub fn set_stopped_normal(&mut self) {
        self.state = ProcessState::StoppedNormal;
        self.wait_start = Some(Instant::now());
    }

    /// 更新状态为停止（出错）
    ///
    /// 当CLI进程出错退出时调用此方法
    pub fn set_stopped_error(&mut self) {
        self.state = ProcessState::StoppedError;
        self.wait_start = Some(Instant::now());
    }

    /// 更新状态为等待用户
    pub fn set_waiting_user(&mut self) {
        self.state = ProcessState::WaitingUser;
        if self.wait_start.is_none() {
            self.wait_start = Some(Instant::now());
        }
    }

    /// 更新状态为运行中
    ///
    /// 当新的CLI进程启动或用户手动恢复时调用
    pub fn set_running(&mut self) {
        self.state = ProcessState::Running;
        self.wait_start = None;
    }

    /// 更新状态为用户已恢复
    pub fn set_user_resumed(&mut self) {
        self.state = ProcessState::UserResumed;
        self.wait_start = None;
    }

    /// 检查是否需要退出
    ///
    /// # 返回值
    /// 如果用户按下Ctrl+C返回true
    pub fn should_exit(&self) -> bool {
        self.exit_flag.load(Ordering::SeqCst)
    }

    /// 检查等待时间是否已过
    ///
    /// # 返回值
    /// 如果等待时间已过返回true，否则返回false
    ///
    /// # 说明
    /// 如果等待计时器未启动，返回false
    pub fn is_wait_elapsed(&self) -> bool {
        if let Some(start) = self.wait_start {
            start.elapsed() >= self.wait_duration
        } else {
            false
        }
    }

    /// 获取剩余等待时间
    ///
    /// # 返回值
    /// 返回剩余的等待时间，如果计时器未启动则返回完整等待时间
    pub fn remaining_wait_time(&self) -> Duration {
        if let Some(start) = self.wait_start {
            let elapsed = start.elapsed();
            if elapsed >= self.wait_duration {
                Duration::ZERO
            } else {
                self.wait_duration - elapsed
            }
        } else {
            self.wait_duration
        }
    }

    /// 重置等待计时器
    ///
    /// 清除等待开始时间
    pub fn reset_wait_timer(&mut self) {
        self.wait_start = None;
    }

    /// 检查进程是否仍在运行
    ///
    /// # 返回值
    /// 如果有运行标志且为true，返回true；否则返回false
    pub fn is_process_running(&self) -> bool {
        if let Some(ref flag) = self.running_flag {
            flag.load(Ordering::SeqCst)
        } else {
            false
        }
    }

    /// 获取状态描述字符串
    ///
    /// # 返回值
    /// 返回当前状态的可读描述
    pub fn state_description(&self) -> &'static str {
        match self.state {
            ProcessState::Running => "运行中",
            ProcessState::StoppedNormal => "正常结束",
            ProcessState::StoppedError => "出错退出",
            ProcessState::WaitingUser => "等待用户",
            ProcessState::UserResumed => "用户已恢复",
        }
    }
}

/// 创建全局退出标志
///
/// # 返回值
/// 返回用于Ctrl+C处理的原子布尔值
pub fn create_exit_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// 设置Ctrl+C处理器
///
/// # 参数
/// - `exit_flag`: 退出标志，按下Ctrl+C时会被设置为true
///
/// # 返回值
/// 成功返回Ok(())，失败返回错误
///
/// # 示例
/// ```
/// let exit_flag = create_exit_flag();
/// setup_ctrlc_handler(exit_flag.clone())?;
/// ```
pub fn setup_ctrlc_handler(exit_flag: Arc<AtomicBool>) -> anyhow::Result<()> {
    ctrlc::set_handler(move || {
        // 设置退出标志
        exit_flag.store(true, Ordering::SeqCst);
        // 打印退出消息
        eprintln!("\n[AC] 收到退出信号，正在退出...");
    })
    .map_err(|e| anyhow::anyhow!("无法设置Ctrl+C处理器: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试监控器初始状态
    #[test]
    fn test_initial_state() {
        let exit_flag = create_exit_flag();
        let monitor = Monitor::new(15, exit_flag);
        assert_eq!(monitor.get_state(), ProcessState::Running);
    }

    /// 测试状态转换
    #[test]
    fn test_state_transitions() {
        let exit_flag = create_exit_flag();
        let mut monitor = Monitor::new(15, exit_flag);

        // 初始状态为运行中
        assert_eq!(monitor.get_state(), ProcessState::Running);

        // 转换为停止（正常）
        monitor.set_stopped_normal();
        assert_eq!(monitor.get_state(), ProcessState::StoppedNormal);

        // 转换回运行中
        monitor.set_running();
        assert_eq!(monitor.get_state(), ProcessState::Running);

        // 转换为停止（错误）
        monitor.set_stopped_error();
        assert_eq!(monitor.get_state(), ProcessState::StoppedError);
    }

    /// 测试等待计时器
    #[test]
    fn test_wait_timer() {
        let exit_flag = create_exit_flag();
        let mut monitor = Monitor::new(1, exit_flag); // 1秒等待时间

        // 初始时计时器未启动
        assert!(!monitor.is_wait_elapsed());

        // 设置停止状态（启动计时器）
        monitor.set_stopped_normal();

        // 刚启动时应该还没过期
        assert!(!monitor.is_wait_elapsed());

        // 等待计时器过期
        std::thread::sleep(Duration::from_millis(1100));
        assert!(monitor.is_wait_elapsed());
    }
}
