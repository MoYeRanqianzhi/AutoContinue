//! # 中断钩子模块 (hook.rs)
//!
//! 该模块提供可扩展的「中断钩子」机制，允许用户在特定条件满足时
//! 自动停止 AutoContinue 的自动发送循环。
//!
//! ## 架构
//!
//! 使用 trait 对象实现策略模式：
//! - `StopHook` trait：定义统一的钩子接口
//! - 预设钩子：`RoundHook`、`ErrorHook`、`TimeHook`、`DurationHook`
//! - 自定义钩子：`CommandHook`（执行外部命令判定）
//! - `StopHookManager`：管理所有钩子，提供统一的 `should_stop()` 接口
//!
//! ## 预设钩子规格字符串
//!
//! 通过 `--stop-when` 参数指定，支持以下格式：
//! - `<round=N>` 或 `<r=N>` — 达到 N 轮后停止
//! - `<error>` 或 `<e>` — 检测到错误时停止（不 retry）
//! - `<time=YYYY-MM-DD HH:MM:SS>` 或 `<t=...>` — 到达指定时间后停止
//! - `<duration=N>` 或 `<d=N>` — 运行 N 秒后停止
//!
//! ## 自定义命令钩子
//!
//! 通过 `--stop-command` 参数指定，启动长驻进程：
//! - 进程退出 → 触发停止
//! - 进程输出 "0" → 停止
//! - 进程输出 "1" → 继续

use anyhow::{bail, Result};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use crate::detector::CliStatus;

// ---------------------------------------------------------------------------
// HookVerdict 枚举
// ---------------------------------------------------------------------------

/// 中断钩子的判定结果
///
/// 每次主循环准备发送提示词前，会逐一调用已注册钩子的 `check()` 方法，
/// 根据返回的 `HookVerdict` 决定是否继续运行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookVerdict {
    /// 继续运行，不中断
    Continue,
    /// 停止自动发送
    Stop,
}

// ---------------------------------------------------------------------------
// StopHook trait
// ---------------------------------------------------------------------------

/// 中断钩子 trait
///
/// 所有钩子（预设和自定义）都实现此 trait。
/// 主循环在每次准备发送提示词前调用 `check()` 判断是否应停止。
///
/// ## 线程安全
///
/// 实现必须是 `Send`，因为钩子可能在多线程环境中被传递。
pub trait StopHook: Send {
    /// 检查是否应该停止
    ///
    /// # 参数
    /// - `round`: 当前已完成的轮次数（从 0 开始）
    /// - `last_status`: 最后一次检测到的 CLI 状态
    ///
    /// # 返回值
    /// - `HookVerdict::Continue` — 不中断，继续自动发送
    /// - `HookVerdict::Stop` — 中断，停止自动发送
    fn check(&mut self, round: u64, last_status: &CliStatus) -> HookVerdict;

    /// 钩子名称（用于日志显示）
    ///
    /// 返回一个人类可读的名称，用于在触发停止时的日志输出中标识该钩子。
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// RoundHook — 轮次限制钩子
// ---------------------------------------------------------------------------

/// 轮次限制钩子：达到指定轮次后停止
///
/// 对应预设规格 `<round=N>` 或 `<r=N>`。
/// 当 `round >= max_rounds` 时触发停止。
///
/// ## 示例
/// - `<round=5>` — 自动发送 5 轮后停止
/// - `<r=1>` — 只自动发送 1 轮
pub struct RoundHook {
    /// 最大允许的轮次数
    max_rounds: u64,
    /// 钩子名称（包含参数值，用于日志显示）
    name_str: String,
}

impl RoundHook {
    /// 创建一个新的轮次限制钩子
    ///
    /// # 参数
    /// - `max_rounds`: 达到此轮次数后触发停止
    pub fn new(max_rounds: u64) -> Self {
        Self {
            max_rounds,
            name_str: format!("轮次限制({})", max_rounds),
        }
    }
}

impl StopHook for RoundHook {
    /// 检查当前轮次是否已达到上限
    ///
    /// 当 `round >= self.max_rounds` 时返回 `Stop`，否则返回 `Continue`。
    fn check(&mut self, round: u64, _last_status: &CliStatus) -> HookVerdict {
        if round >= self.max_rounds {
            HookVerdict::Stop
        } else {
            HookVerdict::Continue
        }
    }

    /// 返回钩子名称（含参数值，如 "轮次限制(5)"）
    fn name(&self) -> &str {
        &self.name_str
    }
}

// ---------------------------------------------------------------------------
// ErrorHook — 错误中断钩子
// ---------------------------------------------------------------------------

/// 错误中断钩子：检测到错误时停止（不 retry）
///
/// 对应预设规格 `<error>` 或 `<e>`。
/// 当 `last_status` 为 `CliStatus::Error { .. }` 时触发停止。
///
/// 使用场景：用户希望在遇到错误时不自动重试，而是停止运行并手动处理。
pub struct ErrorHook;

impl ErrorHook {
    /// 创建一个新的错误中断钩子
    pub fn new() -> Self {
        Self
    }
}

impl StopHook for ErrorHook {
    /// 检查当前状态是否为错误
    ///
    /// 当 `last_status` 匹配 `CliStatus::Error { .. }` 时返回 `Stop`。
    fn check(&mut self, _round: u64, last_status: &CliStatus) -> HookVerdict {
        if matches!(last_status, CliStatus::Error { .. }) {
            HookVerdict::Stop
        } else {
            HookVerdict::Continue
        }
    }

    /// 返回钩子名称
    fn name(&self) -> &str {
        "错误中断"
    }
}

// ---------------------------------------------------------------------------
// TimeHook — 定时中断钩子
// ---------------------------------------------------------------------------

/// 定时中断钩子：到达指定时间后停止
///
/// 对应预设规格 `<time=YYYY-MM-DD HH:MM:SS>` 或 `<t=...>`。
/// 当系统时间 >= deadline 时触发停止。
///
/// ## 时间格式
/// 使用 `%Y-%m-%d %H:%M:%S` 格式，例如 `2026-04-28 18:00:00`。
///
/// ## 注意
/// 由于不使用外部时间库，输入的时间被视为 UTC 时间处理。
/// 如果需要精确的本地时间支持，需使用外部库如 chrono。
///
/// ## 示例
/// - `<time=2026-04-28 18:00:00>` — 在 2026年4月28日 18:00 UTC 停止
/// - `<t=2030-12-31 23:59:59>` — 在 2030年最后一秒 UTC 停止
pub struct TimeHook {
    /// 截止时间点（SystemTime 表示）
    deadline: SystemTime,
}

impl TimeHook {
    /// 创建一个新的定时中断钩子
    ///
    /// # 参数
    /// - `deadline`: 截止时间点，到达此时间后触发停止
    pub fn new(deadline: SystemTime) -> Self {
        Self { deadline }
    }
}

impl StopHook for TimeHook {
    /// 检查当前时间是否已过截止时间
    ///
    /// 当 `SystemTime::now() >= self.deadline` 时返回 `Stop`。
    fn check(&mut self, _round: u64, _last_status: &CliStatus) -> HookVerdict {
        if SystemTime::now() >= self.deadline {
            HookVerdict::Stop
        } else {
            HookVerdict::Continue
        }
    }

    /// 返回钩子名称
    fn name(&self) -> &str {
        "定时中断"
    }
}

// ---------------------------------------------------------------------------
// DurationHook — 持续时间中断钩子
// ---------------------------------------------------------------------------

/// 持续时间中断钩子：运行 N 秒后停止
///
/// 对应预设规格 `<duration=N>` 或 `<d=N>`。
/// 从钩子创建时开始计时，经过 `max_duration` 秒后触发停止。
///
/// ## 示例
/// - `<duration=3600>` — 运行 1 小时后停止
/// - `<d=300>` — 运行 5 分钟后停止
pub struct DurationHook {
    /// 钩子创建时的时间点（用于计算已运行时长）
    start: Instant,
    /// 最大允许运行时长
    max_duration: Duration,
    /// 钩子名称（包含参数值，用于日志显示）
    name_str: String,
}

impl DurationHook {
    /// 创建一个新的持续时间中断钩子
    ///
    /// # 参数
    /// - `seconds`: 最大允许运行秒数，超过后触发停止
    pub fn new(seconds: u64) -> Self {
        Self {
            start: Instant::now(),
            max_duration: Duration::from_secs(seconds),
            name_str: format!("持续时间({}s)", seconds),
        }
    }
}

impl StopHook for DurationHook {
    /// 检查是否已运行超过最大时长
    ///
    /// 当 `self.start.elapsed() >= self.max_duration` 时返回 `Stop`。
    fn check(&mut self, _round: u64, _last_status: &CliStatus) -> HookVerdict {
        if self.start.elapsed() >= self.max_duration {
            HookVerdict::Stop
        } else {
            HookVerdict::Continue
        }
    }

    /// 返回钩子名称（含参数值，如 "持续时间(300s)"）
    fn name(&self) -> &str {
        &self.name_str
    }
}

// ---------------------------------------------------------------------------
// CommandHook — 自定义命令钩子
// ---------------------------------------------------------------------------

/// 自定义命令钩子
///
/// 启动时 spawn 一个长驻子进程，通过两种方式判定是否停止：
///
/// 1. **进程退出** → 触发停止
/// 2. **进程输出判定** → 输出 "0" 停止，输出 "1" 继续（动态切换）
///
/// 无输出且进程仍在运行时，使用上次的判定结果（默认为 `Continue`）。
///
/// ## 实现细节
///
/// - 子进程的 stdout 被 pipe 到后台线程，后台线程逐行读取并通过 channel 发送
/// - `check()` 方法通过 `try_recv` 非阻塞地消费所有待读取的输出行
/// - 取最后一行的值作为最新判定
/// - 子进程在钩子 Drop 时被 kill
pub struct CommandHook {
    /// 钩子名称（包含命令内容，用于日志显示）
    name_str: String,
    /// 子进程句柄（Option 以支持 Drop 中的 take）
    child: Option<Child>,
    /// 上一次的判定结果（用于无新输出时的默认值）
    last_verdict: HookVerdict,
    /// 后台线程通过此 channel 发送子进程的每一行输出
    output_rx: mpsc::Receiver<String>,
}

impl CommandHook {
    /// 创建一个新的自定义命令钩子
    ///
    /// 立即 spawn 子进程并启动后台读取线程。
    ///
    /// # 参数
    /// - `command`: 要执行的 shell 命令字符串
    ///
    /// # 返回值
    /// 成功返回 `CommandHook` 实例，失败返回错误（如命令无法启动）
    ///
    /// # 平台差异
    /// - Windows: 使用 `cmd /C <command>` 执行
    /// - Unix: 使用 `sh -c <command>` 执行
    pub fn new(command: &str) -> Result<Self> {
        // 根据平台选择 shell 执行方式
        let mut child = Self::spawn_command(command)?;

        // 从子进程获取 stdout pipe
        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!("无法获取 hook 命令 '{}' 的 stdout", command)
        })?;

        // 创建有容量限制的 channel，防止子进程大量输出导致 OOM
        // 容量 64 足够缓冲输出，check() 只取最后一行所以中间值不重要
        let (tx, rx) = mpsc::sync_channel::<String>(64);

        // 启动后台线程，逐行读取子进程 stdout
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        // 使用 try_send：channel 满时丢弃该行（旧数据不重要，check 只看最新行）
                        // 发送失败（Disconnected）说明接收端已 drop，退出线程
                        match tx.try_send(text) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(_)) => {
                                // channel 已满，丢弃此行，继续读取下一行
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                // 接收端已关闭，退出线程
                                break;
                            }
                        }
                    }
                    // 读取错误（如管道关闭），退出线程
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            name_str: format!("CommandHook({})", command),
            child: Some(child),
            last_verdict: HookVerdict::Continue,
            output_rx: rx,
        })
    }

    /// 在 Windows 平台上 spawn 子进程
    ///
    /// 使用 `cmd /C <command>` 执行命令，并设置 CREATE_NO_WINDOW 标志
    /// 防止弹出 cmd 窗口。
    #[cfg(windows)]
    fn spawn_command(command: &str) -> Result<Child> {
        use std::os::windows::process::CommandExt;

        Command::new("cmd")
            .args(["/C", command])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .creation_flags(0x08000000) // CREATE_NO_WINDOW：不弹出 cmd 窗口
            .spawn()
            .map_err(|e| anyhow::anyhow!("无法启动 hook 命令 '{}': {}", command, e))
    }

    /// 在 Unix 平台上 spawn 子进程
    ///
    /// 使用 `sh -c <command>` 执行命令。
    #[cfg(not(windows))]
    fn spawn_command(command: &str) -> Result<Child> {
        Command::new("sh")
            .args(["-c", command])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("无法启动 hook 命令 '{}': {}", command, e))
    }
}

impl StopHook for CommandHook {
    /// 检查自定义命令钩子是否应停止
    ///
    /// 执行流程：
    /// 1. 通过 `try_recv` 非阻塞地消费所有待读取的输出行
    /// 2. 取最后一行 trim 后的内容判定：
    ///    - "0" → `last_verdict = Stop`
    ///    - "1" → `last_verdict = Continue`
    ///    - 其他内容 → 忽略，保持上次判定
    /// 3. 检查子进程是否已退出：已退出 → 强制返回 `Stop`
    /// 4. 返回当前 `last_verdict`
    fn check(&mut self, _round: u64, _last_status: &CliStatus) -> HookVerdict {
        // 步骤 1 & 2：消费所有输出，取最后一行更新判定
        let mut latest_line: Option<String> = None;
        while let Ok(line) = self.output_rx.try_recv() {
            latest_line = Some(line);
        }
        if let Some(line) = latest_line {
            let trimmed = line.trim();
            if trimmed == "0" {
                self.last_verdict = HookVerdict::Stop;
            } else if trimmed == "1" {
                self.last_verdict = HookVerdict::Continue;
            }
            // 其他内容忽略，保持上次判定
        }

        // 步骤 3：检查子进程是否已退出
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                // 子进程已退出 → 强制停止
                Ok(Some(_exit_status)) => {
                    return HookVerdict::Stop;
                }
                // 子进程仍在运行 → 使用 last_verdict
                Ok(None) => {}
                // try_wait 出错 → 保守起见停止
                Err(_) => {
                    return HookVerdict::Stop;
                }
            }
        } else {
            // child 已被 take（不应发生），返回 Stop
            return HookVerdict::Stop;
        }

        // 步骤 4：返回当前判定
        self.last_verdict
    }

    /// 返回钩子名称（包含命令内容）
    fn name(&self) -> &str {
        &self.name_str
    }
}

impl Drop for CommandHook {
    /// 在钩子销毁时 kill 并回收子进程
    ///
    /// 确保不会留下孤儿进程或僵尸进程。
    /// kill 失败时（进程可能已退出）仍尝试 wait 回收资源。
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child.take() {
            let _ = child.kill();
            // kill 后必须 wait 回收子进程，否则在 Unix 上会产生僵尸进程
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// StopHookManager — 钩子管理器
// ---------------------------------------------------------------------------

/// 中断钩子管理器
///
/// 持有所有已注册的钩子，提供统一的 `should_stop()` 接口。
/// 主循环在每次准备发送提示词前调用 `should_stop()`，
/// 任意一个钩子返回 `Stop` 即触发停止。
///
/// ## 使用流程
///
/// 1. 创建管理器：`StopHookManager::new()`
/// 2. 注册钩子：`manager.add(Box::new(RoundHook::new(5)))`
/// 3. 每轮检查：`if manager.should_stop(round, &status) { break; }`
pub struct StopHookManager {
    /// 已注册的钩子列表
    hooks: Vec<Box<dyn StopHook>>,
}

impl StopHookManager {
    /// 创建一个新的空钩子管理器
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// 注册一个钩子
    ///
    /// # 参数
    /// - `hook`: 实现了 `StopHook` trait 的钩子实例（boxed）
    pub fn add(&mut self, hook: Box<dyn StopHook>) {
        self.hooks.push(hook);
    }

    /// 检查所有钩子，任意一个返回 Stop 即返回 true
    ///
    /// 遍历所有已注册的钩子，调用其 `check()` 方法。
    /// 一旦有钩子返回 `HookVerdict::Stop`，立即输出日志并返回 `true`。
    /// 所有钩子都返回 `Continue` 时返回 `false`。
    ///
    /// # 参数
    /// - `round`: 当前已完成的轮次数
    /// - `last_status`: 最后一次检测到的 CLI 状态
    ///
    /// # 返回值
    /// - `true` — 应该停止
    /// - `false` — 继续运行
    pub fn should_stop(&mut self, round: u64, last_status: &CliStatus) -> bool {
        for hook in &mut self.hooks {
            if hook.check(round, last_status) == HookVerdict::Stop {
                eprintln!("[AC] 中断钩子 [{}] 触发停止", hook.name());
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// 时间解析辅助函数
// ---------------------------------------------------------------------------

/// 判断指定年份是否为闰年
///
/// 闰年规则：
/// - 能被 4 整除且不能被 100 整除，或
/// - 能被 400 整除
fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// 获取指定年月的天数
///
/// # 参数
/// - `year`: 年份
/// - `month`: 月份（1-12）
///
/// # 返回值
/// 该月的天数
fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// 将日期时间字符串解析为 `SystemTime`
///
/// 手动实现，不依赖外部时间库（如 chrono）。将 `YYYY-MM-DD HH:MM:SS` 格式的
/// 时间字符串转换为 `SystemTime`。
///
/// ## 实现方式
///
/// 将解析的年月日时分秒各字段换算为从 UNIX Epoch (1970-01-01 00:00:00 UTC) 起
/// 的总秒数，然后加到 `SystemTime::UNIX_EPOCH` 上。
///
/// ## 注意
///
/// 由于标准库不提供时区信息获取接口，输入的时间被视为 **UTC 时间**处理。
/// 如需精确的本地时间支持，需引入外部库如 `chrono`。
///
/// # 参数
/// - `s`: 格式为 `YYYY-MM-DD HH:MM:SS` 的时间字符串
///
/// # 返回值
/// 解析成功返回对应的 `SystemTime`，格式错误返回 `Err`
fn parse_datetime_to_systemtime(s: &str) -> Result<SystemTime> {
    // 分割日期和时间部分
    let parts: Vec<&str> = s.trim().split_whitespace().collect();
    if parts.len() != 2 {
        bail!("时间格式错误，期望 'YYYY-MM-DD HH:MM:SS'，实际: '{}'", s);
    }

    // 解析日期部分 YYYY-MM-DD
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        bail!("日期格式错误，期望 'YYYY-MM-DD'，实际: '{}'", parts[0]);
    }
    let year: u64 = date_parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("年份解析失败: '{}'", date_parts[0]))?;
    let month: u64 = date_parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("月份解析失败: '{}'", date_parts[1]))?;
    let day: u64 = date_parts[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("日期解析失败: '{}'", date_parts[2]))?;

    // 解析时间部分 HH:MM:SS
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if time_parts.len() != 3 {
        bail!("时间格式错误，期望 'HH:MM:SS'，实际: '{}'", parts[1]);
    }
    let hour: u64 = time_parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("小时解析失败: '{}'", time_parts[0]))?;
    let minute: u64 = time_parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("分钟解析失败: '{}'", time_parts[1]))?;
    let second: u64 = time_parts[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("秒数解析失败: '{}'", time_parts[2]))?;

    // 基本范围校验
    if !(1..=12).contains(&month) {
        bail!("月份超出范围 (1-12): {}", month);
    }
    if !(1..=days_in_month(year, month)).contains(&day) {
        bail!("日期超出范围 (1-{}): {}", days_in_month(year, month), day);
    }
    if hour >= 24 {
        bail!("小时超出范围 (0-23): {}", hour);
    }
    if minute >= 60 {
        bail!("分钟超出范围 (0-59): {}", minute);
    }
    if second >= 60 {
        bail!("秒数超出范围 (0-59): {}", second);
    }

    // 计算从 UNIX Epoch (1970-01-01 00:00:00 UTC) 到指定日期的总天数
    let mut total_days: u64 = 0;

    // 累加完整年份的天数（从 1970 到 year-1）
    for y in 1970..year {
        if is_leap_year(y) {
            total_days += 366;
        } else {
            total_days += 365;
        }
    }

    // 累加完整月份的天数（从 1 到 month-1）
    for m in 1..month {
        total_days += days_in_month(year, m);
    }

    // 加上当月已过的天数（day 从 1 开始，所以减 1）
    total_days += day - 1;

    // 将总天数和时间转换为秒数
    let utc_seconds = total_days * 86400 + hour * 3600 + minute * 60 + second;

    // 构造 SystemTime
    let duration_from_epoch = Duration::from_secs(utc_seconds);
    let system_time = SystemTime::UNIX_EPOCH + duration_from_epoch;

    Ok(system_time)
}

// ---------------------------------------------------------------------------
// parse_stop_when — 预设钩子解析函数
// ---------------------------------------------------------------------------

/// 解析 `--stop-when` 预设钩子规格字符串
///
/// 支持的格式：
/// - `<round=N>` 或 `<r=N>` — 轮次限制
/// - `<error>` 或 `<e>` — 错误中断
/// - `<time=YYYY-MM-DD HH:MM:SS>` 或 `<t=...>` — 定时中断
/// - `<duration=N>` 或 `<d=N>` — 持续时间中断（秒）
///
/// # 参数
/// - `spec`: 规格字符串，如 `<round=5>` 或 `<e>`
///
/// # 返回值
/// 成功返回对应的钩子实例（boxed），格式错误返回 `Err`
///
/// # 示例
/// ```ignore
/// let hook = parse_stop_when("<round=5>")?;
/// let hook = parse_stop_when("<e>")?;
/// let hook = parse_stop_when("<time=2026-04-28 18:00:00>")?;
/// let hook = parse_stop_when("<d=300>")?;
/// ```
pub fn parse_stop_when(spec: &str) -> Result<Box<dyn StopHook>> {
    // 去掉外层引号、尖括号和空白，宽容处理各种用户输入格式
    // 支持: <round=5>, "round=5", "<round=5>", round=5 等
    let inner = spec
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();

    // 按第一个 '=' 分割为类型和值
    let (hook_type, value) = if let Some(pos) = inner.find('=') {
        let (t, v) = inner.split_at(pos);
        // 跳过 '=' 字符
        (t.trim(), v[1..].trim())
    } else {
        // 没有 '='，整体作为类型（如 "error" 或 "e"）
        (inner, "")
    };

    // 大小写不敏感：统一转为小写比较
    let hook_type_lower = hook_type.to_lowercase();

    match hook_type_lower.as_str() {
        // 轮次限制钩子
        "round" | "r" => {
            let n: u64 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("轮次值必须是正整数，实际: '{}'", value))?;
            if n == 0 {
                bail!("轮次值必须大于 0");
            }
            Ok(Box::new(RoundHook::new(n)))
        }

        // 错误中断钩子
        "error" | "e" => {
            if !value.is_empty() {
                bail!("error 钩子不接受参数，实际: '{}'", value);
            }
            Ok(Box::new(ErrorHook::new()))
        }

        // 定时中断钩子
        "time" | "t" => {
            if value.is_empty() {
                bail!("time 钩子需要时间参数，格式: <time=YYYY-MM-DD HH:MM:SS>");
            }
            let deadline = parse_datetime_to_systemtime(value)?;
            Ok(Box::new(TimeHook::new(deadline)))
        }

        // 持续时间中断钩子
        "duration" | "d" => {
            let seconds: u64 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("持续时间必须是正整数（秒），实际: '{}'", value))?;
            if seconds == 0 {
                bail!("持续时间必须大于 0 秒");
            }
            Ok(Box::new(DurationHook::new(seconds)))
        }

        // 未知类型
        _ => {
            bail!(
                "未知的钩子类型: '{}'。支持的类型: round/r, error/e, time/t, duration/d",
                hook_type_lower
            );
        }
    }
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::CliStatus;

    // -----------------------------------------------------------------------
    // RoundHook 测试
    // -----------------------------------------------------------------------

    /// 测试 RoundHook 在未达到轮次上限时返回 Continue
    #[test]
    fn test_round_hook_continue() {
        let mut hook = RoundHook::new(5);
        let status = CliStatus::Idle;

        // 轮次 0~4 都应返回 Continue
        for round in 0..5 {
            assert_eq!(hook.check(round, &status), HookVerdict::Continue);
        }
    }

    /// 测试 RoundHook 在达到/超过轮次上限时返回 Stop
    #[test]
    fn test_round_hook_stop() {
        let mut hook = RoundHook::new(3);
        let status = CliStatus::Idle;

        // 轮次 3 及以上应返回 Stop
        assert_eq!(hook.check(3, &status), HookVerdict::Stop);
        assert_eq!(hook.check(10, &status), HookVerdict::Stop);
        assert_eq!(hook.check(100, &status), HookVerdict::Stop);
    }

    /// 测试 RoundHook 边界条件：max_rounds = 1
    #[test]
    fn test_round_hook_boundary() {
        let mut hook = RoundHook::new(1);
        let status = CliStatus::Idle;

        // 轮次 0 应继续
        assert_eq!(hook.check(0, &status), HookVerdict::Continue);
        // 轮次 1 应停止
        assert_eq!(hook.check(1, &status), HookVerdict::Stop);
    }

    /// 测试 RoundHook 名称（含参数值）
    #[test]
    fn test_round_hook_name() {
        let hook = RoundHook::new(5);
        assert_eq!(hook.name(), "轮次限制(5)");
    }

    // -----------------------------------------------------------------------
    // ErrorHook 测试
    // -----------------------------------------------------------------------

    /// 测试 ErrorHook 在状态为 Error 时返回 Stop
    #[test]
    fn test_error_hook_stop_on_error() {
        let mut hook = ErrorHook::new();
        let status = CliStatus::Error {
            message: "API 超时".to_string(),
        };

        assert_eq!(hook.check(0, &status), HookVerdict::Stop);
    }

    /// 测试 ErrorHook 在非 Error 状态时返回 Continue
    #[test]
    fn test_error_hook_continue_on_non_error() {
        let mut hook = ErrorHook::new();

        assert_eq!(hook.check(0, &CliStatus::Idle), HookVerdict::Continue);
        assert_eq!(hook.check(0, &CliStatus::Busy), HookVerdict::Continue);
        assert_eq!(hook.check(0, &CliStatus::Unknown), HookVerdict::Continue);
    }

    /// 测试 ErrorHook 名称
    #[test]
    fn test_error_hook_name() {
        let hook = ErrorHook::new();
        assert_eq!(hook.name(), "错误中断");
    }

    // -----------------------------------------------------------------------
    // DurationHook 测试
    // -----------------------------------------------------------------------

    /// 测试 DurationHook 刚创建时应返回 Continue（未超时）
    #[test]
    fn test_duration_hook_initial_continue() {
        let mut hook = DurationHook::new(3600); // 1 小时
        let status = CliStatus::Idle;

        assert_eq!(hook.check(0, &status), HookVerdict::Continue);
    }

    /// 测试 DurationHook 在超时后返回 Stop
    #[test]
    fn test_duration_hook_stop_after_elapsed() {
        // 创建一个已经"过期"的钩子（通过直接设置 start 为过去的时间点）
        let hook_inner = DurationHook {
            start: Instant::now() - Duration::from_secs(100),
            max_duration: Duration::from_secs(50),
            name_str: "持续时间(50s)".to_string(),
        };
        let mut hook: Box<dyn StopHook> = Box::new(hook_inner);
        let status = CliStatus::Idle;

        assert_eq!(hook.check(0, &status), HookVerdict::Stop);
    }

    /// 测试 DurationHook 名称（含参数值）
    #[test]
    fn test_duration_hook_name() {
        let hook = DurationHook::new(300);
        assert_eq!(hook.name(), "持续时间(300s)");
    }

    // -----------------------------------------------------------------------
    // TimeHook 测试
    // -----------------------------------------------------------------------

    /// 测试 TimeHook 在截止时间为过去时返回 Stop
    #[test]
    fn test_time_hook_stop_past_deadline() {
        // 使用 UNIX_EPOCH 作为截止时间（肯定已经过了）
        let mut hook = TimeHook::new(SystemTime::UNIX_EPOCH);
        let status = CliStatus::Idle;

        assert_eq!(hook.check(0, &status), HookVerdict::Stop);
    }

    /// 测试 TimeHook 在截止时间为遥远未来时返回 Continue
    #[test]
    fn test_time_hook_continue_future_deadline() {
        // 设置一个遥远未来的截止时间（当前时间 + 1 天）
        let deadline = SystemTime::now() + Duration::from_secs(86400);
        let mut hook = TimeHook::new(deadline);
        let status = CliStatus::Idle;

        assert_eq!(hook.check(0, &status), HookVerdict::Continue);
    }

    /// 测试 TimeHook 名称
    #[test]
    fn test_time_hook_name() {
        let hook = TimeHook::new(SystemTime::now());
        assert_eq!(hook.name(), "定时中断");
    }

    // -----------------------------------------------------------------------
    // StopHookManager 测试
    // -----------------------------------------------------------------------

    /// 测试空管理器的行为
    #[test]
    fn test_manager_empty() {
        let mut manager = StopHookManager::new();
        let status = CliStatus::Idle;

        // 空管理器不应触发停止
        assert!(!manager.should_stop(0, &status));
    }

    /// 测试管理器注册钩子后的基本功能
    #[test]
    fn test_manager_add_and_check() {
        let mut manager = StopHookManager::new();
        manager.add(Box::new(RoundHook::new(3)));

        let status = CliStatus::Idle;
        // 轮次未到不触发
        assert!(!manager.should_stop(0, &status));
        // 轮次到达触发
        assert!(manager.should_stop(3, &status));
    }

    /// 测试管理器多钩子组合：任意一个触发即停止
    #[test]
    fn test_manager_any_stop() {
        let mut manager = StopHookManager::new();
        // RoundHook: 5 轮后停止
        manager.add(Box::new(RoundHook::new(5)));
        // ErrorHook: 错误时停止
        manager.add(Box::new(ErrorHook::new()));

        let idle = CliStatus::Idle;
        let error = CliStatus::Error {
            message: "test".to_string(),
        };

        // 轮次 0 + Idle → 不停止
        assert!(!manager.should_stop(0, &idle));

        // 轮次 0 + Error → ErrorHook 触发停止
        assert!(manager.should_stop(0, &error));
    }

    /// 测试管理器多钩子：轮次到达时触发
    #[test]
    fn test_manager_round_triggers() {
        let mut manager = StopHookManager::new();
        manager.add(Box::new(RoundHook::new(3)));
        manager.add(Box::new(ErrorHook::new()));

        let idle = CliStatus::Idle;

        // 轮次 0-2 不触发
        assert!(!manager.should_stop(0, &idle));
        assert!(!manager.should_stop(1, &idle));
        assert!(!manager.should_stop(2, &idle));

        // 轮次 3 触发
        assert!(manager.should_stop(3, &idle));
    }

    // -----------------------------------------------------------------------
    // parse_stop_when 测试
    // -----------------------------------------------------------------------

    /// 测试解析 round 钩子（完整形式和缩写形式）
    #[test]
    fn test_parse_round() {
        let hook = parse_stop_when("<round=5>").unwrap();
        assert_eq!(hook.name(), "轮次限制(5)");

        let hook = parse_stop_when("<r=10>").unwrap();
        assert_eq!(hook.name(), "轮次限制(10)");
    }

    /// 测试解析 error 钩子（完整形式和缩写形式）
    #[test]
    fn test_parse_error() {
        let hook = parse_stop_when("<error>").unwrap();
        assert_eq!(hook.name(), "错误中断");

        let hook = parse_stop_when("<e>").unwrap();
        assert_eq!(hook.name(), "错误中断");
    }

    /// 测试解析 time 钩子
    #[test]
    fn test_parse_time() {
        let hook = parse_stop_when("<time=2026-04-28 18:00:00>").unwrap();
        assert_eq!(hook.name(), "定时中断");

        let hook = parse_stop_when("<t=2030-12-31 23:59:59>").unwrap();
        assert_eq!(hook.name(), "定时中断");
    }

    /// 测试解析 duration 钩子（完整形式和缩写形式）
    #[test]
    fn test_parse_duration() {
        let hook = parse_stop_when("<duration=300>").unwrap();
        assert_eq!(hook.name(), "持续时间(300s)");

        let hook = parse_stop_when("<d=60>").unwrap();
        assert_eq!(hook.name(), "持续时间(60s)");
    }

    /// 测试解析带空白的规格字符串
    #[test]
    fn test_parse_with_whitespace() {
        let hook = parse_stop_when("  <round=5>  ").unwrap();
        assert_eq!(hook.name(), "轮次限制(5)");
    }

    /// 测试解析错误：未知类型
    #[test]
    fn test_parse_unknown_type() {
        let result = parse_stop_when("<unknown=5>");
        assert!(result.is_err());
    }

    /// 测试解析错误：round 值不是数字
    #[test]
    fn test_parse_round_invalid_value() {
        let result = parse_stop_when("<round=abc>");
        assert!(result.is_err());
    }

    /// 测试解析错误：round 值为 0
    #[test]
    fn test_parse_round_zero() {
        let result = parse_stop_when("<round=0>");
        assert!(result.is_err());
    }

    /// 测试解析错误：duration 值为 0
    #[test]
    fn test_parse_duration_zero() {
        let result = parse_stop_when("<duration=0>");
        assert!(result.is_err());
    }

    /// 测试解析错误：error 带了不该有的参数
    #[test]
    fn test_parse_error_with_value() {
        let result = parse_stop_when("<error=something>");
        assert!(result.is_err());
    }

    /// 测试解析错误：time 缺少值
    #[test]
    fn test_parse_time_missing_value() {
        let result = parse_stop_when("<time>");
        assert!(result.is_err());
    }

    /// 测试解析错误：time 格式不正确
    #[test]
    fn test_parse_time_invalid_format() {
        let result = parse_stop_when("<time=not-a-date>");
        assert!(result.is_err());
    }

    /// 测试解析错误：time 月份超出范围
    #[test]
    fn test_parse_time_invalid_month() {
        let result = parse_stop_when("<time=2026-13-01 00:00:00>");
        assert!(result.is_err());
    }

    /// 测试解析错误：time 日期超出范围（2 月 30 日不存在）
    #[test]
    fn test_parse_time_invalid_day() {
        let result = parse_stop_when("<time=2026-02-30 00:00:00>");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // 时间解析辅助函数测试
    // -----------------------------------------------------------------------

    /// 测试闰年判断
    #[test]
    fn test_is_leap_year() {
        assert!(is_leap_year(2000)); // 能被 400 整除
        assert!(is_leap_year(2024)); // 能被 4 整除，不能被 100 整除
        assert!(!is_leap_year(1900)); // 能被 100 整除，不能被 400 整除
        assert!(!is_leap_year(2023)); // 不能被 4 整除
    }

    /// 测试各月天数
    #[test]
    fn test_days_in_month() {
        assert_eq!(days_in_month(2024, 1), 31);
        assert_eq!(days_in_month(2024, 2), 29); // 闰年 2 月
        assert_eq!(days_in_month(2023, 2), 28); // 非闰年 2 月
        assert_eq!(days_in_month(2024, 4), 30);
        assert_eq!(days_in_month(2024, 12), 31);
    }

    /// 测试 UNIX Epoch 的解析
    #[test]
    fn test_parse_datetime_epoch() {
        let result = parse_datetime_to_systemtime("1970-01-01 00:00:00").unwrap();
        assert_eq!(result, SystemTime::UNIX_EPOCH);
    }

    /// 测试具体日期的解析
    #[test]
    fn test_parse_datetime_specific() {
        let result = parse_datetime_to_systemtime("2026-04-28 18:00:00").unwrap();
        // 验证解析出的时间在合理范围内（大于 UNIX_EPOCH）
        assert!(result > SystemTime::UNIX_EPOCH);
    }

    /// 测试解析格式错误的时间字符串
    #[test]
    fn test_parse_datetime_invalid() {
        assert!(parse_datetime_to_systemtime("not-a-date").is_err());
        assert!(parse_datetime_to_systemtime("2026-13-01 00:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-02-30 00:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-01-01 25:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-01-01 00:61:00").is_err());
    }

    // -----------------------------------------------------------------------
    // HookVerdict 测试
    // -----------------------------------------------------------------------

    /// 测试 HookVerdict 的 PartialEq 和 Copy
    #[test]
    fn test_hook_verdict_traits() {
        let v1 = HookVerdict::Continue;
        let v2 = v1; // Copy
        assert_eq!(v1, v2);

        let v3 = HookVerdict::Stop;
        assert_ne!(v1, v3);
    }

    // -----------------------------------------------------------------------
    // CommandHook 测试
    // -----------------------------------------------------------------------

    /// 测试 CommandHook 输出 "0" 时返回 Stop
    #[test]
    fn test_command_hook_echo_zero_stops() {
        let mut hook = CommandHook::new("echo 0").unwrap();
        let status = CliStatus::Idle;

        // 等待子进程输出到达 channel
        std::thread::sleep(Duration::from_millis(500));

        // 子进程输出 "0" 并退出，应返回 Stop
        assert_eq!(hook.check(0, &status), HookVerdict::Stop);
    }

    /// 测试 CommandHook 输出 "1" 时返回 Continue
    #[test]
    fn test_command_hook_echo_one_continues() {
        let mut hook = CommandHook::new("echo 1").unwrap();
        let status = CliStatus::Idle;

        // 等待子进程输出到达 channel
        std::thread::sleep(Duration::from_millis(500));

        // 子进程输出 "1"，但因为进程也退出了，进程退出触发 Stop 优先级更高
        // 所以实际结果取决于 check 的逻辑：先消费输出设 last_verdict=Continue，再检测进程退出→Stop
        assert_eq!(hook.check(0, &status), HookVerdict::Stop);
    }

    /// 测试 CommandHook 名称包含命令内容
    #[test]
    fn test_command_hook_name() {
        let hook = CommandHook::new("echo test").unwrap();
        assert!(hook.name().contains("echo test"));
    }

    // -----------------------------------------------------------------------
    // parse_stop_when 宽容性测试
    // -----------------------------------------------------------------------

    /// 测试不带尖括号的输入
    #[test]
    fn test_parse_without_angle_brackets() {
        let hook = parse_stop_when("round=5").unwrap();
        assert_eq!(hook.name(), "轮次限制(5)");

        let hook = parse_stop_when("error").unwrap();
        assert_eq!(hook.name(), "错误中断");
    }

    /// 测试带引号包裹的输入
    #[test]
    fn test_parse_with_quotes() {
        let hook = parse_stop_when("\"<round=5>\"").unwrap();
        assert_eq!(hook.name(), "轮次限制(5)");

        let hook = parse_stop_when("'<error>'").unwrap();
        assert_eq!(hook.name(), "错误中断");
    }

    /// 测试大小写不敏感
    #[test]
    fn test_parse_case_insensitive() {
        let hook = parse_stop_when("<Round=5>").unwrap();
        assert_eq!(hook.name(), "轮次限制(5)");

        let hook = parse_stop_when("<ERROR>").unwrap();
        assert_eq!(hook.name(), "错误中断");

        let hook = parse_stop_when("<Duration=60>").unwrap();
        assert_eq!(hook.name(), "持续时间(60s)");

        let hook = parse_stop_when("<R=3>").unwrap();
        assert_eq!(hook.name(), "轮次限制(3)");
    }

    // -----------------------------------------------------------------------
    // 时间解析边界值测试
    // -----------------------------------------------------------------------

    /// 测试闰年 2 月 29 日应成功解析
    #[test]
    fn test_parse_datetime_leap_year_feb29() {
        assert!(parse_datetime_to_systemtime("2024-02-29 00:00:00").is_ok());
        assert!(parse_datetime_to_systemtime("2000-02-29 12:30:00").is_ok());
    }

    /// 测试非闰年 2 月 29 日应失败
    #[test]
    fn test_parse_datetime_non_leap_year_feb29() {
        assert!(parse_datetime_to_systemtime("2023-02-29 00:00:00").is_err());
        assert!(parse_datetime_to_systemtime("1900-02-29 00:00:00").is_err());
    }

    /// 测试月份边界值：0 月和 13 月应失败
    #[test]
    fn test_parse_datetime_month_boundaries() {
        assert!(parse_datetime_to_systemtime("2026-00-15 12:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-13-15 12:00:00").is_err());
    }

    /// 测试日期边界值：0 日和 32 日应失败
    #[test]
    fn test_parse_datetime_day_boundaries() {
        assert!(parse_datetime_to_systemtime("2026-01-00 12:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-01-32 12:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-04-31 12:00:00").is_err());
    }

    /// 测试时分秒边界值
    #[test]
    fn test_parse_datetime_time_boundaries() {
        // 合法边界
        assert!(parse_datetime_to_systemtime("2026-01-01 23:59:59").is_ok());
        assert!(parse_datetime_to_systemtime("2026-01-01 00:00:00").is_ok());
        // 非法值
        assert!(parse_datetime_to_systemtime("2026-01-01 24:00:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-01-01 00:60:00").is_err());
        assert!(parse_datetime_to_systemtime("2026-01-01 00:00:60").is_err());
    }

    // -----------------------------------------------------------------------
    // 多钩子组合优先级测试
    // -----------------------------------------------------------------------

    /// 测试 DurationHook 过期时优先于 RoundHook 未到触发
    #[test]
    fn test_manager_duration_overrides_round() {
        let mut manager = StopHookManager::new();
        // RoundHook 设置 100 轮（不会触发）
        manager.add(Box::new(RoundHook::new(100)));
        // DurationHook 已过期
        let expired = DurationHook {
            start: Instant::now() - Duration::from_secs(200),
            max_duration: Duration::from_secs(100),
            name_str: "持续时间(100s)".to_string(),
        };
        manager.add(Box::new(expired));

        let status = CliStatus::Idle;
        // 虽然轮次只有 0（RoundHook 不触发），但 DurationHook 已过期应触发停止
        assert!(manager.should_stop(0, &status));
    }

    /// 测试所有钩子都不触发时不停止
    #[test]
    fn test_manager_all_continue() {
        let mut manager = StopHookManager::new();
        manager.add(Box::new(RoundHook::new(100)));
        manager.add(Box::new(ErrorHook::new()));
        manager.add(Box::new(DurationHook::new(99999)));

        let status = CliStatus::Idle;
        assert!(!manager.should_stop(0, &status));
        assert!(!manager.should_stop(50, &status));
    }
}
