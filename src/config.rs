//! # 配置管理模块 (config.rs)
//!
//! 该模块负责管理AutoContinue的配置，包括：
//! - 加载提示词（从命令行参数或文件）
//! - 支持IO模式：每次使用时动态读取文件
//! - 支持Pipe模式：每次使用时执行命令获取提示词
//! - 支持Format提取：从管道输出中按前缀后缀提取内容
//! - 中断钩子（stop-hook / stop-when）：传递命令/条件字符串，由 main.rs 负责解析和调度
//! - 管理默认值
//! - 配置验证
//!
//! ## 默认值
//! - `continue_prompt`: "继续"
//! - `retry_prompt`: "重试"
//! - `sleep_time`: 15秒
//!
//! ## 提示词模式优先级
//! pipe > io > direct > file > default
//!
//! ## Pipe模式
//! 使用 -cpp 或 -rpp 参数时，提示词会在每次使用时执行指定命令，
//! 将命令的 stdout 输出作为提示词内容。配合 --cformat / --rformat
//! 可从输出中提取特定标签包裹的内容。

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 默认的继续提示词
const DEFAULT_CONTINUE_PROMPT: &str = "继续";

/// 默认的重试提示词
const DEFAULT_RETRY_PROMPT: &str = "重试";

/// 默认的等待时间（秒）
pub const DEFAULT_SLEEP_TIME: u64 = 15;

/// 默认的静默阈值（秒）
pub const DEFAULT_SILENCE_THRESHOLD: u64 = 30;

/// 默认的最大轮次限制（-1 表示无限制）
pub const DEFAULT_LIMIT: i64 = -1;

/// AutoContinue配置结构体
///
/// 该结构体包含所有运行时需要的配置信息。
/// 配置可以从命令行参数或文件加载。
/// 支持多种提示词模式：直接、文件、IO、管道（Pipe）。
#[derive(Debug, Clone)]
pub struct Config {
    /// 要运行的CLI程序名称
    pub cli: String,

    /// 传递给CLI程序的参数
    pub cli_args: Vec<String>,

    /// 继续的提示词（静态模式）
    /// 当CLI正常结束时发送此提示词
    /// 如果设置了 io/pipe 模式，此字段为空
    pub continue_prompt: String,

    /// 继续提示词的IO文件路径（动态模式）
    /// 如果设置了此字段，每次使用时会重新读取文件
    pub continue_prompt_io: Option<String>,

    /// 继续提示词的管道命令（Pipe模式）
    /// 如果设置了此字段，每次使用时执行命令获取提示词
    pub continue_prompt_pipe: Option<String>,

    /// 重试的提示词（静态模式）
    /// 当CLI出错时发送此提示词
    /// 如果设置了 io/pipe 模式，此字段为空
    pub retry_prompt: String,

    /// 重试提示词的IO文件路径（动态模式）
    /// 如果设置了此字段，每次使用时会重新读取文件
    pub retry_prompt_io: Option<String>,

    /// 重试提示词的管道命令（Pipe模式）
    /// 如果设置了此字段，每次使用时执行命令获取提示词
    pub retry_prompt_pipe: Option<String>,

    /// 继续管道输出格式提取标签 [前缀, 后缀]
    /// 仅在 pipe 模式下生效，从输出中提取最后一组匹配内容
    pub cformat: Option<(String, String)>,

    /// 重试管道输出格式提取标签 [前缀, 后缀]
    /// 仅在 pipe 模式下生效，从输出中提取最后一组匹配内容
    pub rformat: Option<(String, String)>,

    /// 等待时间（秒）
    /// 在自动发送提示词之前等待的时间
    /// 给用户自主回复的机会
    pub sleep_time: u64,

    /// 静默阈值（秒）
    /// CLI无输入/输出超过此时间后开始计算等待时间
    /// 总等待时间 = 静默阈值 + 等待时间
    pub silence_threshold: u64,

    /// 最大自动发送轮次限制
    /// -1 表示无限制，正数表示最大发送次数
    /// 达到限制后程序将停止自动发送并退出
    pub limit: i64,

    /// 中断钩子命令列表
    /// 每个字符串是一个 shell 命令，启动时 spawn 为长驻进程
    pub stop_hooks: Vec<String>,

    /// 预设中断钩子规格列表
    /// 每个字符串是一个预设条件，如 "<round=5>", "<error>", "<time=...>"
    pub stop_whens: Vec<String>,
}

impl Config {
    /// 从命令行参数创建配置
    ///
    /// # 参数
    /// - `args`: 解析后的命令行参数
    ///
    /// # 返回值
    /// 成功返回Config实例，失败返回错误
    ///
    /// # 错误
    /// - 当指定的提示词文件不存在或无法读取时返回错误
    ///
    /// # 示例
    /// ```
    /// let args = parse_args();
    /// let config = Config::from_args(&args)?;
    /// ```
    pub fn from_args(args: &crate::args::Args) -> Result<Self> {
        // 处理继续提示词：使用统一方法解析提示词来源
        let (continue_prompt, continue_prompt_io, continue_prompt_pipe) =
            resolve_prompt_source(
                args.continue_prompt_pipe.as_ref(),
                args.continue_prompt_io.as_ref(),
                args.continue_prompt_file.as_ref(),
                args.continue_prompt.as_ref(),
                DEFAULT_CONTINUE_PROMPT,
                "继续提示词",
            )?;

        // 处理重试提示词：使用统一方法解析提示词来源
        let (retry_prompt, retry_prompt_io, retry_prompt_pipe) =
            resolve_prompt_source(
                args.retry_prompt_pipe.as_ref(),
                args.retry_prompt_io.as_ref(),
                args.retry_prompt_file.as_ref(),
                args.retry_prompt.as_ref(),
                DEFAULT_RETRY_PROMPT,
                "重试提示词",
            )?;

        // 处理格式提取参数：将 Vec<String> 转换为 (prefix, suffix) 元组
        let cformat = parse_format_pair(args.cformat.as_ref());
        let rformat = parse_format_pair(args.rformat.as_ref());

        // 如果指定了 --cformat 但没有 -cpp，发出警告
        if cformat.is_some() && continue_prompt_pipe.is_none() {
            eprintln!("[AC] 警告: --cformat 仅在 -cpp (管道模式) 下生效，当前未使用管道模式");
        }
        // 如果指定了 --rformat 但没有 -rpp，发出警告
        if rformat.is_some() && retry_prompt_pipe.is_none() {
            eprintln!("[AC] 警告: --rformat 仅在 -rpp (管道模式) 下生效，当前未使用管道模式");
        }

        // 将CLI参数从OsString转换为String
        // 使用 to_string_lossy() 处理非 UTF-8 字符（Windows 上 OsString 可能包含 WTF-16 序列），
        // 避免 filter_map 静默丢弃含非法字符的参数
        let cli_args: Vec<String> = args
            .cli_args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();

        Ok(Config {
            cli: args.cli.clone(),
            cli_args,
            continue_prompt,
            continue_prompt_io,
            continue_prompt_pipe,
            retry_prompt,
            retry_prompt_io,
            retry_prompt_pipe,
            cformat,
            rformat,
            sleep_time: args.sleep_time,
            silence_threshold: args.silence_threshold,
            limit: args.limit,
            stop_hooks: args.stop_hook.clone(),
            stop_whens: args.stop_when.clone(),
        })
    }

    /// 获取当前的继续提示词
    ///
    /// 优先级：pipe > io > static
    /// - Pipe模式：执行命令，可选 format 提取
    /// - IO模式：重新读取文件
    /// - 静态模式：返回缓存内容
    ///
    /// # 返回值
    /// 成功返回提示词内容，失败返回错误
    pub fn get_continue_prompt(&self) -> Result<String> {
        get_prompt(
            self.continue_prompt_pipe.as_ref(),
            self.continue_prompt_io.as_ref(),
            &self.continue_prompt,
            self.cformat.as_ref(),
            "继续提示词",
        )
    }

    /// 获取当前的重试提示词
    ///
    /// 优先级：pipe > io > static
    /// - Pipe模式：执行命令，可选 format 提取
    /// - IO模式：重新读取文件
    /// - 静态模式：返回缓存内容
    ///
    /// # 返回值
    /// 成功返回提示词内容，失败返回错误
    pub fn get_retry_prompt(&self) -> Result<String> {
        get_prompt(
            self.retry_prompt_pipe.as_ref(),
            self.retry_prompt_io.as_ref(),
            &self.retry_prompt,
            self.rformat.as_ref(),
            "重试提示词",
        )
    }

    /// 检查是否使用继续提示词IO模式
    pub fn is_continue_prompt_io(&self) -> bool {
        self.continue_prompt_io.is_some()
    }

    /// 检查是否使用继续提示词Pipe模式
    pub fn is_continue_prompt_pipe(&self) -> bool {
        self.continue_prompt_pipe.is_some()
    }
}

/// 解析提示词来源（构建 Config 时调用）
///
/// 按优先级决定使用哪种提示词模式：
/// 1. pipe（管道命令）：存储命令，运行时每次执行
/// 2. io（IO 文件）：存储路径，运行时每次读取
/// 3. direct（命令行直接指定）：使用用户提供的文本
/// 4. file（一次性文件）：立即读取文件内容，转为 static 模式
/// 5. default（默认值）：使用内置默认提示词
///
/// # 参数
/// - `pipe`: 管道命令参数
/// - `io`: IO 文件路径参数
/// - `file`: 一次性文件路径参数
/// - `direct`: 命令行直接指定的提示词
/// - `default_prompt`: 默认提示词文本
/// - `label`: 提示词类型标签，用于错误信息（如 "继续提示词"）
///
/// # 返回值
/// 成功返回 (static_prompt, io_path, pipe_cmd) 三元组，失败返回错误
///
/// # 错误
/// - IO 文件不存在时返回错误
/// - 一次性文件读取失败时返回错误
fn resolve_prompt_source(
    pipe: Option<&String>,
    io: Option<&String>,
    file: Option<&String>,
    direct: Option<&String>,
    default_prompt: &str,
    label: &str,
) -> Result<(String, Option<String>, Option<String>)> {
    if let Some(pipe_cmd) = pipe {
        // Pipe模式：存储命令，每次使用时执行
        return Ok((String::new(), None, Some(pipe_cmd.clone())));
    }
    if let Some(io_path) = io {
        // IO模式：验证文件存在，存储路径
        if !Path::new(io_path).exists() {
            anyhow::bail!("{}IO文件不存在: {}", label, io_path);
        }
        return Ok((String::new(), Some(io_path.clone()), None));
    }
    if let Some(prompt) = direct {
        // 从命令行参数直接读取提示词
        return Ok((prompt.clone(), None, None));
    }
    if let Some(file_path) = file {
        // 从文件一次性读取提示词，转为静态模式
        let prompt = load_prompt_from_file(file_path)
            .with_context(|| format!("无法从文件加载{}: {}", label, file_path))?;
        return Ok((prompt, None, None));
    }
    // 使用默认提示词
    Ok((default_prompt.to_string(), None, None))
}

/// 统一的提示词获取逻辑
///
/// 按优先级依次尝试三种模式获取提示词：
/// 1. 管道命令（pipe）：执行命令获取输出，可选 format 标签提取
/// 2. IO 文件：每次动态读取文件内容
/// 3. 静态文本：返回预设的提示词字符串
///
/// # 参数
/// - `pipe_cmd`: 管道命令（最高优先级）
/// - `io_path`: IO 文件路径（次优先级）
/// - `static_prompt`: 静态提示词文本（最低优先级）
/// - `format`: 格式提取标签对 (前缀, 后缀)，仅 pipe 模式下使用
/// - `label`: 提示词类型标签，用于错误信息（如 "继续提示词"）
///
/// # 返回值
/// 成功返回提示词内容，失败返回错误
fn get_prompt(
    pipe_cmd: Option<&String>,
    io_path: Option<&String>,
    static_prompt: &str,
    format: Option<&(String, String)>,
    label: &str,
) -> Result<String> {
    if let Some(cmd) = pipe_cmd {
        // Pipe模式：执行命令获取输出
        let output = execute_pipe_command(cmd)
            .with_context(|| format!("执行{}管道命令失败: {}", label, cmd))?;
        // 如果配置了格式提取标签，从输出中提取匹配内容
        if let Some((prefix, suffix)) = format {
            if let Some(content) = extract_format(&output, prefix, suffix) {
                return Ok(content.to_string());
            }
            eprintln!("[AC] 警告: 管道输出中未找到格式标签 {}...{}，使用完整输出", prefix, suffix);
        }
        return Ok(output);
    }
    if let Some(path) = io_path {
        // IO模式：每次重新读取文件
        return load_prompt_from_file(path)
            .with_context(|| format!("读取{}IO文件失败: {}", label, path));
    }
    // 静态模式：返回预设的提示词
    Ok(static_prompt.to_string())
}

/// 解析格式提取标签对
///
/// 将 clap 返回的 Option<Vec<String>> 转换为 Option<(String, String)>。
/// 仅当 Vec 恰好包含 2 个元素时，将其视为 (前缀, 后缀) 标签对。
///
/// # 参数
/// - `v`: clap 解析出的可选字符串向量引用
///
/// # 返回值
/// 恰好 2 个元素时返回 Some((前缀, 后缀))，否则返回 None
fn parse_format_pair(v: Option<&Vec<String>>) -> Option<(String, String)> {
    v.and_then(|v| {
        if v.len() == 2 {
            Some((v[0].clone(), v[1].clone()))
        } else {
            None
        }
    })
}

/// 从文件加载提示词
///
/// # 参数
/// - `path`: 提示词文件路径
///
/// # 返回值
/// 成功返回文件内容（去除首尾空白，标准化换行符），失败返回错误
///
/// # 错误
/// - 文件不存在
/// - 文件无法读取
///
/// # 注意
/// 该函数会标准化换行符：
/// - Windows换行符 `\r\n` 会被转换为 `\n`
/// - 单独的 `\r` 也会被转换为 `\n`
/// 这确保了跨平台的一致行为
fn load_prompt_from_file<P: AsRef<Path>>(path: P) -> Result<String> {
    let content = fs::read_to_string(path.as_ref())
        .with_context(|| "读取文件失败")?;

    // 标准化换行符：将 \r\n 和单独的 \r 都转换为 \n
    // 这样可以避免在PTY中换行被重复处理
    let normalized = content
        .replace("\r\n", "\n")  // Windows换行符 -> Unix换行符
        .replace("\r", "\n");   // 旧Mac换行符 -> Unix换行符

    // 去除首尾空白字符
    Ok(normalized.trim().to_string())
}

/// 执行管道命令并返回 stdout 输出
///
/// 跨平台实现：
/// - Windows: 使用 cmd /C 执行命令
/// - Unix: 使用 sh -c 执行命令
///
/// # 参数
/// - `command`: 要执行的 shell 命令字符串
///
/// # 返回值
/// 成功返回命令的 stdout 输出（去除首尾空白），失败返回错误
///
/// # 错误
/// - 命令执行失败（找不到命令等）
/// - 命令返回非零退出码
fn execute_pipe_command(command: &str) -> Result<String> {
    // 解析命令字符串为程序名 + 参数列表
    let args = parse_command(command)
        .with_context(|| format!("解析管道命令失败: {}", command))?;

    eprintln!("[AC] [PIPE] 执行: {} (参数: {:?})", args[0], &args[1..]);

    // 启动子进程：使用 spawn() + try_wait() 实现超时控制
    // 避免 .output() 在子进程长时间不退出时无限期阻塞主线程
    let mut child = build_command(&args[0], &args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("无法执行程序: {}", args[0]))?;

    // 等待子进程退出，最多 30 秒
    // 超时则强制 kill 并报错，避免管道命令挂起导致整个 AC 主循环卡住
    let timeout = Duration::from_secs(30);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // 进程已退出，跳出循环并交由后续逻辑收集 stdout/stderr
                break status;
            }
            Ok(None) => {
                // 进程仍在运行：检查是否超时
                if start.elapsed() > timeout {
                    // 超时强杀子进程，避免僵尸进程残留
                    let _ = child.kill();
                    // 尝试 wait 一次回收进程资源（忽略错误）
                    let _ = child.wait();
                    anyhow::bail!(
                        "管道命令执行超时（{}秒）: {}",
                        timeout.as_secs(),
                        command
                    );
                }
                // 短暂休眠避免 CPU 忙等
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                anyhow::bail!("等待管道命令失败: {}", e);
            }
        }
    };

    // 收集子进程的 stdout/stderr 输出
    // 注意：必须在 try_wait 返回 Some 之后再读取，否则 stdout/stderr 管道可能未关闭
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        std::io::Read::read_to_end(&mut stdout, &mut stdout_buf).ok();
    }
    if let Some(mut stderr) = child.stderr.take() {
        std::io::Read::read_to_end(&mut stderr, &mut stderr_buf).ok();
    }

    // 检查退出状态
    if !status.success() {
        let stderr = decode_command_output(&stderr_buf);
        let code = status.code().unwrap_or(-1);
        eprintln!("[AC] [PIPE] 命令失败: 退出码={}, stderr=[{}]", code, stderr.trim());
        anyhow::bail!("管道命令退出码 {}，stderr: {}", code, stderr.trim());
    }

    // 解码 stdout 输出（自动处理 UTF-8 / UTF-16 / 系统代码页）
    let result = decode_command_output(&stdout_buf);
    let result = result.trim().to_string();

    // 截取前 100 字节用于日志显示：必须按字符截取避免在多字节 UTF-8 字符中间切片导致 panic
    // 33 个字符 × 3 字节/中文字符 ≈ 99 字节，足够日志预览使用
    let display = if result.len() > 100 {
        let truncated: String = result.chars().take(33).collect();
        format!("{}...", truncated)
    } else {
        result.clone()
    };
    eprintln!("[AC] [PIPE] 命令成功，输出: {}", display);

    if result.is_empty() {
        anyhow::bail!("管道命令输出为空");
    }

    Ok(result)
}

/// 根据平台构建 Command 对象
///
/// Windows 上 npm/pip 等包管理器安装的 CLI 工具是 .cmd/.bat 脚本，
/// CreateProcessW 无法直接执行这类脚本，必须通过 cmd.exe /C 调用。
///
/// 关键难点：cmd.exe 会解释 <>&| 等元字符，但双引号内的元字符是安全的。
/// 使用 `cmd /S /C "..."` 配合 raw_arg 精确控制命令行：
///   cmd /S /C ""codex.CMD" "exec" "输出<continue>继续</continue>""
/// /S /C 会剥离最外层引号对，剩余部分中每个参数的引号保护了 <> 等字符。
///
/// Unix 上直接执行即可，shell 脚本有 shebang 由内核处理。
fn build_command(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        // 在 PATH 中解析完整路径（带 PATHEXT 扩展名搜索）
        if let Some(resolved) = resolve_command_windows(program) {
            let ext = resolved.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            if ext == "cmd" || ext == "bat" {
                // .cmd/.bat 必须通过 cmd.exe 执行
                // 使用 raw_arg 绕过 Rust 的参数转义，直接控制 Windows 命令行
                eprintln!("[AC] [PIPE] 检测到 .{} 脚本，通过 cmd.exe /S /C 执行: {}",
                    ext, resolved.display());

                let mut cmd = Command::new("cmd");

                // 构建 cmd /S /C "..." 命令行
                // /S 强制 cmd.exe 剥离最外层引号，保留内部结构
                // 格式: /S /C ""path\to\program.cmd" "arg1" "arg2""
                let resolved_str = resolved.to_string_lossy();
                // cmd.exe 双引号内仍会展开 %VAR% 环境变量和解释 ^ 转义符
                // 当 delayed expansion 开启时，! 还会触发 !VAR! 变量展开
                // 转义顺序很关键：
                //   1. 先 ^ → ^^（防止后续插入的 ^ 被再次处理）
                //   2. 再 ! → ^!（在 delayed expansion 开启时安全转义 !）
                //   3. 然后 % → %%（防止环境变量展开）
                //   4. 最后 " → ""（cmd.exe 双引号内的转义方式）
                let args_str: Vec<String> = args.iter()
                    .map(|a| format!(
                        "\"{}\"",
                        a.replace('^', "^^")
                            .replace('!', "^!")
                            .replace('%', "%%")
                            .replace('"', "\"\"")
                    ))
                    .collect();
                let inner = if args_str.is_empty() {
                    format!("/S /C \"\"{}\"\"", resolved_str)
                } else {
                    format!("/S /C \"\"{}\" {}\"", resolved_str, args_str.join(" "))
                };

                cmd.raw_arg(&inner);
                return cmd;
            }

            // .exe/.com 等可直接执行的格式
            let mut cmd = Command::new(&resolved);
            cmd.args(args);
            return cmd;
        }

        // 解析失败，仍尝试直接执行（让 OS 报错）
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}

/// Windows：在 PATH 中搜索命令，考虑 PATHEXT 扩展名
///
/// 模拟 cmd.exe 的命令查找行为：
/// 1. 如果程序名已含扩展名，直接在 PATH 中查找
/// 2. 否则，依次附加 PATHEXT 中的扩展名（.COM;.EXE;.BAT;.CMD 等）查找
#[cfg(windows)]
fn resolve_command_windows(program: &str) -> Option<PathBuf> {
    let program_path = Path::new(program);

    // 如果是绝对路径，直接检查是否存在，不继续搜索 PATH
    if program_path.is_absolute() {
        return if program_path.exists() {
            Some(program_path.to_path_buf())
        } else {
            None
        };
    }

    // 安全检查：程序名包含路径分隔符（如 "../foo" 或 "subdir\\bar"）
    // 视为相对/绝对路径直接处理，禁止在 PATH 中搜索
    // 防止恶意构造的程序名通过 PATH 搜索意外解析到其他位置（路径遍历防御）
    if program.contains('/') || program.contains('\\') {
        let path = PathBuf::from(program);
        return if path.is_file() { Some(path) } else { None };
    }

    // 如果已包含扩展名（如 "codex.cmd"），只在 PATH 中查找该名称
    let has_extension = program_path.extension().is_some();

    // 获取 PATHEXT（默认值覆盖常见场景）
    let pathext = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let extensions: Vec<&str> = pathext.split(';').collect();

    // 获取 PATH
    let path_var = match std::env::var("PATH") {
        Ok(p) => p,
        Err(_) => return None,
    };

    // 遍历 PATH 目录
    for dir in std::env::split_paths(&path_var) {
        if has_extension {
            // 已有扩展名，直接查找
            let candidate = dir.join(program);
            if candidate.is_file() {
                return Some(candidate);
            }
        } else {
            // 无扩展名，依次尝试 PATHEXT 中的每个扩展名
            for ext in &extensions {
                let ext = ext.trim_start_matches('.');
                if ext.is_empty() {
                    continue;
                }
                let name = format!("{}.{}", program, ext);
                let candidate = dir.join(&name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}


/// 解析命令字符串为程序名和参数列表
///
/// 支持 shell 风格的引号语法：
/// - 单引号 `'...'`：内容原样保留，不转义
/// - 双引号 `"..."`：内容原样保留，`\"` 和 `\\` 可转义
/// - 反斜杠：转义下一个字符
/// - 空白分隔参数
///
/// # 示例
/// ```
/// parse_command("codex exec '输出<continue>继续</continue>'")
/// // → ["codex", "exec", "输出<continue>继续</continue>"]
///
/// parse_command("echo \"hello world\"")
/// // → ["echo", "hello world"]
/// ```
fn parse_command(cmd: &str) -> Result<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(c) = chars.next() {
        if in_single_quote {
            // 单引号内：除了闭合引号，一切原样保留
            if c == '\'' {
                in_single_quote = false;
            } else {
                current.push(c);
            }
        } else if in_double_quote {
            // 双引号内：支持 \" 和 \\ 转义
            if c == '"' {
                in_double_quote = false;
            } else if c == '\\' {
                if let Some(&next) = chars.peek() {
                    if next == '"' || next == '\\' {
                        current.push(chars.next().unwrap());
                    } else {
                        current.push(c);
                    }
                } else {
                    // 字符串末尾的 \，原样保留（后续检测未闭合引号会报错）
                    current.push(c);
                }
            } else {
                current.push(c);
            }
        } else {
            // 引号外
            match c {
                '\'' => in_single_quote = true,
                '"' => in_double_quote = true,
                '\\' => {
                    // 转义下一个字符
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                c if c.is_whitespace() => {
                    // 空白分隔参数
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            }
        }
    }

    // 最后一个参数
    if !current.is_empty() {
        args.push(current);
    }

    if in_single_quote || in_double_quote {
        anyhow::bail!("未闭合的引号");
    }

    if args.is_empty() {
        anyhow::bail!("空命令");
    }

    Ok(args)
}

/// 将命令输出字节流解码为 UTF-8 字符串
///
/// 策略：
/// 1. 尝试 UTF-8（最常见，快速路径）
/// 2. 检测 UTF-16LE BOM 或特征（WSL 等环境可能输出 UTF-16LE）
/// 3. Windows: 调用 MultiByteToWideChar 按系统代码页转换
/// 4. 其他平台: lossy 兜底
fn decode_command_output(bytes: &[u8]) -> String {
    // 空输入
    if bytes.is_empty() {
        return String::new();
    }

    // 1. UTF-8（最常见）
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }

    // 2. 检测 UTF-16LE
    if bytes.len() >= 2 {
        // 优先检测 BOM（Byte Order Mark: 0xFF 0xFE）
        if bytes[0] == 0xFF && bytes[1] == 0xFE {
            let wide: Vec<u16> = bytes[2..].chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let decoded = String::from_utf16_lossy(&wide);
            if !decoded.is_empty() {
                return decoded;
            }
        }

        // 启发式检测：超过 30% 的奇数位字节是 \x00（ASCII 高字节为 0）
        let null_count = bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
        if null_count > bytes.len() * 3 / 10 {
            let wide: Vec<u16> = bytes.chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let decoded = String::from_utf16_lossy(&wide);
            if !decoded.is_empty() {
                return decoded;
            }
        }
    }

    // 3. Windows: 系统代码页转换
    #[cfg(windows)]
    {
        win_decode::from_system_codepage(bytes)
    }

    // 4. 其他平台: lossy 兜底
    #[cfg(not(windows))]
    {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Windows 平台：通过 MultiByteToWideChar API 将系统代码页字节转为 UTF-8
///
/// 自动适配任意 Windows 系统代码页（GBK、Big5、Shift-JIS、EUC-KR 等），
/// 零额外依赖，直接调用 kernel32.dll。
#[cfg(windows)]
mod win_decode {
    use std::ffi::c_int;

    /// Windows ANSI 代码页常量
    const CP_ACP: u32 = 0;

    unsafe extern "system" {
        /// 将多字节字符串转换为 UTF-16 宽字符串
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            src: *const u8,
            src_len: c_int,
            dst: *mut u16,
            dst_len: c_int,
        ) -> c_int;
    }

    /// 将系统代码页编码的字节流转换为 UTF-8 字符串
    ///
    /// 流程：系统代码页字节 → UTF-16（via MultiByteToWideChar）→ UTF-8（via Rust String）
    ///
    /// # 安全加固
    /// - 输入长度上限：bytes.len() 不能超过 c_int::MAX（约 2GB），否则 as 转换会溢出/截断
    /// - 第二次 API 调用同样检查返回值，避免静默使用未初始化数据
    pub fn from_system_codepage(bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }

        // 长度上限检查：防止 bytes.len() as c_int 溢出截断
        // c_int 在 Windows 上为 i32，最大约 2GB
        if bytes.len() > c_int::MAX as usize {
            eprintln!("[AC] [PIPE] 警告: 输出超过 2GB，使用 lossy 解码");
            return String::from_utf8_lossy(bytes).into_owned();
        }

        unsafe {
            // 第一次调用：获取所需的 UTF-16 缓冲区大小
            let wide_len = MultiByteToWideChar(
                CP_ACP, 0,
                bytes.as_ptr(), bytes.len() as c_int,
                std::ptr::null_mut(), 0,
            );

            if wide_len <= 0 {
                // API 调用失败，fallback 到 lossy
                eprintln!("[AC] [PIPE] 警告: MultiByteToWideChar 失败，使用 lossy 解码");
                return String::from_utf8_lossy(bytes).into_owned();
            }

            // 第二次调用：实际转换
            let mut wide_buf = vec![0u16; wide_len as usize];
            let actual_len = MultiByteToWideChar(
                CP_ACP, 0,
                bytes.as_ptr(), bytes.len() as c_int,
                wide_buf.as_mut_ptr(), wide_len,
            );

            // 检查第二次调用的返回值：必须 > 0 且与预期长度一致
            // 否则缓冲区可能未完全填充，使用未初始化数据会产生乱码或错误结果
            if actual_len <= 0 || actual_len != wide_len {
                eprintln!("[AC] [PIPE] 警告: MultiByteToWideChar 转换失败，使用 lossy 解码");
                return String::from_utf8_lossy(bytes).into_owned();
            }

            // UTF-16 → UTF-8
            String::from_utf16_lossy(&wide_buf)
        }
    }
}

/// 从文本中提取最后一组前缀后缀包裹的内容
///
/// 在输出中查找最后一个 `prefix...suffix` 模式，返回中间内容。
/// 用于从管道命令输出中提取特定标签包裹的提示词，过滤多余文本。
///
/// # 参数
/// - `output`: 完整输出文本
/// - `prefix`: 前缀标签（如 `<continue>`）
/// - `suffix`: 后缀标签（如 `</continue>`）
///
/// # 返回值
/// 找到匹配返回 Some(内容)（去除首尾空白），未找到返回 None
///
/// # 示例
/// ```
/// let output = "some text <continue>real prompt</continue> more text";
/// let result = extract_format(output, "<continue>", "</continue>");
/// assert_eq!(result, Some("real prompt".to_string()));
/// ```
pub fn extract_format(output: &str, prefix: &str, suffix: &str) -> Option<String> {
    // 从后往前查找最后一个 prefix 的位置
    let prefix_pos = output.rfind(prefix)?;

    // 从 prefix 之后开始查找 suffix
    let content_start = prefix_pos + prefix.len();
    let remaining = &output[content_start..];
    let suffix_pos = remaining.find(suffix)?;

    // 提取并返回中间内容
    let content = &remaining[..suffix_pos];
    Some(content.trim().to_string())
}

impl Default for Config {
    /// 创建默认配置
    ///
    /// 默认配置使用空CLI名称和默认提示词（静态模式）
    fn default() -> Self {
        Config {
            cli: String::new(),
            cli_args: Vec::new(),
            continue_prompt: DEFAULT_CONTINUE_PROMPT.to_string(),
            continue_prompt_io: None,
            continue_prompt_pipe: None,
            retry_prompt: DEFAULT_RETRY_PROMPT.to_string(),
            retry_prompt_io: None,
            retry_prompt_pipe: None,
            cformat: None,
            rformat: None,
            sleep_time: DEFAULT_SLEEP_TIME,
            silence_threshold: DEFAULT_SILENCE_THRESHOLD,
            limit: DEFAULT_LIMIT,
            stop_hooks: Vec::new(),
            stop_whens: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// 测试从文件加载提示词
    #[test]
    fn test_load_prompt_from_file() -> Result<()> {
        // 创建临时文件
        let mut file = NamedTempFile::new()?;
        writeln!(file, "  测试提示词  ")?;

        // 加载并验证
        let prompt = load_prompt_from_file(file.path())?;
        assert_eq!(prompt, "测试提示词");

        Ok(())
    }

    /// 测试换行符标准化（Windows格式 \r\n）
    #[test]
    fn test_normalize_crlf() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        // 写入Windows格式换行符
        file.write_all(b"Line1\r\nLine2\r\nLine3")?;

        let prompt = load_prompt_from_file(file.path())?;
        // 应该只有 \n，没有 \r
        assert!(!prompt.contains('\r'));
        assert_eq!(prompt, "Line1\nLine2\nLine3");

        Ok(())
    }

    /// 测试换行符标准化（单独的 \r）
    #[test]
    fn test_normalize_cr() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        // 写入旧Mac格式换行符
        file.write_all(b"Line1\rLine2\rLine3")?;

        let prompt = load_prompt_from_file(file.path())?;
        // \r 应该被转换为 \n
        assert!(!prompt.contains('\r'));
        assert_eq!(prompt, "Line1\nLine2\nLine3");

        Ok(())
    }

    /// 测试默认配置
    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.continue_prompt, DEFAULT_CONTINUE_PROMPT);
        assert_eq!(config.retry_prompt, DEFAULT_RETRY_PROMPT);
        assert_eq!(config.sleep_time, DEFAULT_SLEEP_TIME);
        assert_eq!(config.silence_threshold, DEFAULT_SILENCE_THRESHOLD);
        assert_eq!(config.limit, DEFAULT_LIMIT);
        assert!(config.continue_prompt_pipe.is_none());
        assert!(config.retry_prompt_pipe.is_none());
        assert!(config.cformat.is_none());
        assert!(config.rformat.is_none());
    }

    /// 测试 extract_format 基本提取
    #[test]
    fn test_extract_format_basic() {
        let output = "some text <continue>real prompt</continue> more text";
        let result = extract_format(output, "<continue>", "</continue>");
        assert_eq!(result, Some("real prompt".to_string()));
    }

    /// 测试 extract_format 多组匹配取最后一组
    #[test]
    fn test_extract_format_last_match() {
        let output = "<c>first</c> middle <c>second</c> end";
        let result = extract_format(output, "<c>", "</c>");
        assert_eq!(result, Some("second".to_string()));
    }

    /// 测试 extract_format 无匹配返回 None
    #[test]
    fn test_extract_format_no_match() {
        let output = "no tags here";
        let result = extract_format(output, "<c>", "</c>");
        assert_eq!(result, None);
    }

    /// 测试 extract_format 只有前缀没有后缀
    #[test]
    fn test_extract_format_no_suffix() {
        let output = "text <c>content without closing";
        let result = extract_format(output, "<c>", "</c>");
        assert_eq!(result, None);
    }

    /// 测试 extract_format 多行内容
    #[test]
    fn test_extract_format_multiline() {
        let output = "header\n<continue>\nline1\nline2\n</continue>\nfooter";
        let result = extract_format(output, "<continue>", "</continue>");
        assert_eq!(result, Some("line1\nline2".to_string()));
    }

    /// 测试 execute_pipe_command 基本执行
    #[test]
    fn test_execute_pipe_command_echo() {
        let result = execute_pipe_command("echo hello").unwrap();
        assert_eq!(result, "hello");
    }

    /// 测试 execute_pipe_command 找不到程序
    #[test]
    fn test_execute_pipe_command_failure() {
        // 执行一个不存在的程序
        let result = execute_pipe_command("nonexistent_program_xyz_12345");
        assert!(result.is_err());
    }

    /// 测试 parse_command 基本解析
    #[test]
    fn test_parse_command_basic() {
        let args = parse_command("echo hello").unwrap();
        assert_eq!(args, vec!["echo", "hello"]);
    }

    /// 测试 parse_command 单引号
    #[test]
    fn test_parse_command_single_quotes() {
        let args = parse_command("codex exec '输出<continue>继续</continue>'").unwrap();
        assert_eq!(args, vec!["codex", "exec", "输出<continue>继续</continue>"]);
    }

    /// 测试 parse_command 双引号
    #[test]
    fn test_parse_command_double_quotes() {
        let args = parse_command(r#"echo "hello world""#).unwrap();
        assert_eq!(args, vec!["echo", "hello world"]);
    }

    /// 测试 parse_command 混合引号和普通参数
    #[test]
    fn test_parse_command_mixed() {
        let args = parse_command("cmd arg1 'arg 2' arg3").unwrap();
        assert_eq!(args, vec!["cmd", "arg1", "arg 2", "arg3"]);
    }

    /// 测试 parse_command 空命令报错
    #[test]
    fn test_parse_command_empty() {
        assert!(parse_command("").is_err());
        assert!(parse_command("   ").is_err());
    }

    /// 测试 parse_command 未闭合引号报错
    #[test]
    fn test_parse_command_unclosed_quote() {
        assert!(parse_command("echo 'hello").is_err());
    }
}
