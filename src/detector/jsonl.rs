//! # JSONL 通用检测适配器 (detector/jsonl.rs)
//!
//! 通过监控 JSONL 会话文件来精确检测 CLI 状态。
//! 本模块是 ClaudeDetector 和 CodexDetector 的统一实现，
//! 通过 `JsonlDetectorConfig` 配置两者之间的差异。
//!
//! ## 支持的 CLI 工具
//!
//! - **Claude Code**: 会话文件位于 `~/.claude/projects/{projectHash}/{sessionId}.jsonl`
//! - **Codex**: 会话文件位于 `~/.codex/sessions/{subdir}/{prefix}-{sessionId}.jsonl`
//!
//! ## 共同的检测逻辑
//!
//! - 文件正在增长 → `Busy`
//! - 最后一条消息为 `assistant` 且文件停止增长 → `Idle`
//! - 消息内容包含错误关键词 → `Error`
//! - 文件不存在或无法解析 → `Unknown`（fallback 到静默超时）
//!
//! ## 配置差异
//!
//! | 配置项 | Claude | Codex |
//! |--------|--------|-------|
//! | 会话目录 | `~/.claude/projects/` | `~/.codex/sessions/` |
//! | 递归扫描 | 否（仅扫描一层子目录） | 是（递归扫描，限制深度 5 层） |
//! | 识别 summary | 是（summary 类型视为 Idle） | 否 |
//! | 错误关键词 | Claude 特有的错误短语 | Codex 特有的错误短语 |

use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::{CliStatus, Detector};

/// JSONL 文件轮询间隔（秒）
///
/// 每隔此时间重新扫描 JSONL 文件。
/// 参考 Happy 的 SessionScanner 使用 3 秒间隔。
const POLL_INTERVAL_SECS: u64 = 3;

/// 文件"停止增长"判定时间（秒）
///
/// 如果 JSONL 文件在此时间内没有变化，视为 CLI 已完成输出。
const FILE_STABLE_SECS: u64 = 2;

/// JSONL 消息的最小化解析结构
///
/// 兼容 Claude Code 和 Codex 两种格式。
/// Claude 的消息结构: `{ "type": "...", "message": { "role": "...", "content": ... } }`
/// Codex 的消息结构: `{ "type": "...", "role": "...", "content": ..., "message": { ... } }`
///
/// 使用宽松的 Option 字段，同时适配两种格式。
#[derive(Debug, Deserialize)]
struct JsonlMessage {
    /// 消息类型：user / assistant / system / summary
    #[serde(rename = "type")]
    msg_type: Option<String>,

    /// 顶层角色字段（Codex 格式使用）
    role: Option<String>,

    /// 顶层内容字段（Codex 格式使用）
    #[serde(default)]
    content: serde_json::Value,

    /// 嵌套的消息内容结构（Claude 格式使用，Codex 也可能有）
    message: Option<MessageContent>,
}

/// 嵌套消息内容结构
///
/// 同时适配 Claude 和 Codex 的嵌套消息格式。
#[derive(Debug, Deserialize)]
struct MessageContent {
    /// 角色：user / assistant
    role: Option<String>,

    /// 文本内容（可能是字符串或 TextBlock 数组）
    #[serde(default)]
    content: serde_json::Value,
}

/// JSONL 检测器的差异化配置
///
/// 通过此结构体配置 Claude 和 Codex 之间的行为差异，
/// 避免为两个高度相似的检测器编写重复代码。
pub struct JsonlDetectorConfig {
    /// 检测器显示名称（用于日志输出，如 "ClaudeDetector" 或 "CodexDetector"）
    pub name: String,

    /// 会话文件所在目录的路径
    /// Claude: `~/.claude/projects/`
    /// Codex: `~/.codex/sessions/`
    pub sessions_dir: PathBuf,

    /// 是否递归扫描子目录查找 JSONL 文件
    /// Claude 仅在项目子目录的第一层中查找 .jsonl 文件，
    /// Codex 则需要递归扫描子目录。
    pub recursive_scan: bool,

    /// 递归扫描的最大深度限制
    /// 防止在极深的目录结构中浪费时间。
    /// 仅当 `recursive_scan` 为 true 时生效。
    pub max_scan_depth: usize,

    /// 错误关键词列表
    /// 不同的 CLI 工具可能有不同的错误报告格式，
    /// 通过配置不同的关键词列表来适配。
    pub error_keywords: Vec<&'static str>,

    /// 是否将 summary 消息类型识别为 Idle 状态
    /// Claude Code 在会话初始化完成时会发送 summary 消息，
    /// 此时应视为 Idle 状态。Codex 不使用此机制。
    pub recognize_summary: bool,
}

/// JSONL 通用检测适配器
///
/// 通过监控 CLI 工具的 JSONL 会话文件来检测状态。
/// 使用 `JsonlDetectorConfig` 配置不同 CLI 之间的差异行为。
/// 替代了原有的 ClaudeDetector 和 CodexDetector 两个独立实现。
pub struct JsonlDetector {
    /// 差异化配置
    config: JsonlDetectorConfig,

    /// 当前监控的 JSONL 文件路径
    current_session_file: Option<PathBuf>,

    /// 上次扫描文件时读取到的文件大小（字节）
    last_file_size: u64,

    /// 上次文件大小发生变化的时间
    last_size_change_time: Instant,

    /// 上次轮询扫描文件的时间
    last_poll_time: Instant,

    /// 解析到的最后一条消息的类型（如 "assistant", "summary"）
    last_message_type: Option<String>,

    /// 解析到的最后一条消息的角色（如 "user", "assistant"）
    last_message_role: Option<String>,

    /// 是否检测到错误
    error_detected: bool,

    /// 错误信息摘要
    error_message: String,

    /// 读取 JSONL 文件失败的警告是否已经输出过
    ///
    /// `parse_last_messages` 会被频繁调用（轮询触发），如果文件每次都打开失败，
    /// 会刷屏污染日志。该标志保证仅首次失败时输出 eprintln 警告。
    /// 在 `reset()` 中不会清空该标志（避免重置后再次刷屏）。
    read_error_warned: bool,
}

impl JsonlDetector {
    /// 根据配置创建新的 JSONL 检测器
    ///
    /// # 参数
    /// - `config`: 差异化配置，指定检测器名称、目录路径、扫描方式等
    ///
    /// # 返回值
    /// 配置好的 JsonlDetector 实例
    pub fn new(config: JsonlDetectorConfig) -> Self {
        JsonlDetector {
            config,
            current_session_file: None,
            last_file_size: 0,
            last_size_change_time: Instant::now(),
            last_poll_time: Instant::now(),
            last_message_type: None,
            last_message_role: None,
            error_detected: false,
            error_message: String::new(),
            // 读取失败警告标志：初始为 false，首次失败后置为 true
            read_error_warned: false,
        }
    }

    /// 扫描会话目录，找到最新的 JSONL 文件
    ///
    /// 根据配置的 `recursive_scan` 标志决定扫描方式：
    /// - 非递归模式（Claude）：仅扫描每个项目子目录中的第一层文件
    /// - 递归模式（Codex）：递归扫描子目录，受 `max_scan_depth` 限制
    ///
    /// # 返回值
    /// 找到最新的 JSONL 文件路径，未找到则返回 None
    fn find_latest_session_file(&self) -> Option<PathBuf> {
        // 如果会话目录不存在，直接返回 None
        if !self.config.sessions_dir.exists() {
            return None;
        }

        let mut latest_file: Option<PathBuf> = None;
        let mut latest_time = SystemTime::UNIX_EPOCH;

        if self.config.recursive_scan {
            // 递归扫描模式（Codex）：从根目录开始递归，深度从 0 开始
            self.scan_dir_recursive(
                &self.config.sessions_dir,
                &mut latest_file,
                &mut latest_time,
                0,
            );
        } else {
            // 非递归扫描模式（Claude）：遍历每个项目子目录的第一层
            let entries = match std::fs::read_dir(&self.config.sessions_dir) {
                Ok(e) => e,
                Err(_) => return None,
            };

            for entry in entries.flatten() {
                let project_path = entry.path();
                if !project_path.is_dir() {
                    continue;
                }

                // 在项目子目录中查找 .jsonl 文件（不递归）
                let sub_entries = match std::fs::read_dir(&project_path) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                for sub_entry in sub_entries.flatten() {
                    let file_path = sub_entry.path();
                    if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }

                    // 比较修改时间，保留最新的文件
                    if let Ok(metadata) = file_path.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            if modified > latest_time {
                                latest_time = modified;
                                latest_file = Some(file_path);
                            }
                        }
                    }
                }
            }
        }

        latest_file
    }

    /// 递归扫描目录中的 JSONL 文件（带深度限制）
    ///
    /// 用于 Codex 等需要在子目录中查找会话文件的 CLI 工具。
    ///
    /// # 参数
    /// - `dir`: 当前扫描的目录
    /// - `latest_file`: 迄今找到的最新文件（输出参数）
    /// - `latest_time`: 迄今找到的最新修改时间（输出参数）
    /// - `current_depth`: 当前递归深度（从 0 开始）
    fn scan_dir_recursive(
        &self,
        dir: &Path,
        latest_file: &mut Option<PathBuf>,
        latest_time: &mut SystemTime,
        current_depth: usize,
    ) {
        // 超过最大扫描深度时停止递归
        if current_depth > self.config.max_scan_depth {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // 递归进入子目录，深度 +1
                self.scan_dir_recursive(&path, latest_file, latest_time, current_depth + 1);
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                // 比较修改时间，保留最新的文件
                if let Ok(metadata) = path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if modified > *latest_time {
                            *latest_time = modified;
                            *latest_file = Some(path);
                        }
                    }
                }
            }
        }
    }

    /// 轮询检查 JSONL 文件的变化
    ///
    /// 定期（每 POLL_INTERVAL_SECS 秒）检查当前监控的 JSONL 文件：
    /// 1. 如果没有当前文件，尝试发现新文件
    /// 2. 检查是否有更新的会话文件（可能是新会话）
    /// 3. 记录文件大小变化
    /// 4. 解析文件末尾的最新消息
    fn poll_session_file(&mut self) {
        // 检查轮询间隔，避免过于频繁地扫描文件系统
        if self.last_poll_time.elapsed() < Duration::from_secs(POLL_INTERVAL_SECS) {
            return;
        }
        self.last_poll_time = Instant::now();

        // 如果没有当前文件，尝试发现
        if self.current_session_file.is_none() {
            self.current_session_file = self.find_latest_session_file();
        }

        // 获取当前文件路径
        let file_path = match &self.current_session_file {
            Some(path) => path.clone(),
            None => return,
        };

        // 获取文件大小，如果文件已被删除则尝试重新发现
        let current_size = match std::fs::metadata(&file_path) {
            Ok(meta) => meta.len(),
            Err(_) => {
                // 文件可能被删除，尝试重新发现
                self.current_session_file = self.find_latest_session_file();
                return;
            }
        };

        // 检查是否有更新的会话文件（可能是新会话启动了）
        if let Some(latest) = self.find_latest_session_file() {
            if latest != file_path {
                // 发现了更新的会话文件，切换监控目标
                self.current_session_file = Some(latest);
                self.last_file_size = 0;
                self.last_size_change_time = Instant::now();
                // 用新文件重新轮询
                self.poll_session_file();
                return;
            }
        }

        // 记录文件大小变化
        if current_size != self.last_file_size {
            self.last_file_size = current_size;
            self.last_size_change_time = Instant::now();
        }

        // 解析文件末尾的最新消息
        self.parse_last_messages(&file_path);
    }

    /// 解析 JSONL 文件末尾的最后一条消息
    ///
    /// 仅读取文件**末尾约 8KB** 的内容（足以涵盖最后若干条 JSONL 消息），
    /// 从后向前遍历行，找到最后一条能成功解析的 JSON 消息。
    ///
    /// # 为什么只读末尾
    /// 长会话的 JSONL 文件可达数十 MB，每次轮询都全量 `read_to_string`
    /// 会带来显著的 I/O 与内存开销。我们只关心"最后一条消息"，
    /// 因此通过 `seek(End-TAIL_SIZE)` + `read_to_end` 仅读取尾部即可。
    ///
    /// # 撕裂读取（torn read）说明
    /// 注意：外部 CLI 可能正在并发写入同一 JSONL 文件。
    /// `seek + read` 与 CLI 写入之间存在天然的竞态：
    ///   1. 我们 seek 到的位置可能落在某条 UTF-8 多字节字符或 JSONL 行的中间；
    ///   2. 读取过程中 CLI 可能正好追加新数据，导致最后一行不完整。
    /// 解析时对每行使用 `serde_json::from_str`，**解析失败的行会被安全跳过**，
    /// 不会导致检测器崩溃；最坏情况下只是延迟一个轮询周期才看到最新消息。
    ///
    /// # 参数
    /// - `path`: JSONL 文件路径
    fn parse_last_messages(&mut self, path: &Path) {
        use std::io::{Read, Seek, SeekFrom};

        /// 仅读取文件末尾的字节数。
        /// 8KB 通常足以覆盖最后数十条 JSONL 消息（每条几百字节～几 KB）。
        const TAIL_SIZE: u64 = 8192;

        // 打开文件；失败时仅在首次输出警告，避免轮询期间刷屏。
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                if !self.read_error_warned {
                    self.read_error_warned = true;
                    eprintln!(
                        "[AC] 警告: 打开 JSONL 文件失败（仅提示一次） path={} err={}",
                        path.display(),
                        e
                    );
                }
                return;
            }
        };

        // 取文件总长度，用于决定是全量读取还是 seek 到尾部
        let file_len = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                if !self.read_error_warned {
                    self.read_error_warned = true;
                    eprintln!(
                        "[AC] 警告: 读取 JSONL 元数据失败（仅提示一次） path={} err={}",
                        path.display(),
                        e
                    );
                }
                return;
            }
        };

        // 根据文件大小选择读取策略：
        //   - 文件 ≤ 8KB：直接全量读取
        //   - 文件 > 8KB：seek 到末尾 -8KB 处，仅读取尾部
        let content: String = if file_len <= TAIL_SIZE {
            // 小文件：全量读取
            let mut s = String::new();
            if let Err(e) = file.read_to_string(&mut s) {
                if !self.read_error_warned {
                    self.read_error_warned = true;
                    eprintln!(
                        "[AC] 警告: 读取 JSONL 文件失败（仅提示一次） path={} err={}",
                        path.display(),
                        e
                    );
                }
                return;
            }
            s
        } else {
            // 大文件：seek 到 (末尾 - TAIL_SIZE) 位置
            // SeekFrom::End 接受负偏移；这里强转为 i64 是安全的（TAIL_SIZE 为常量 8192）
            if let Err(e) = file.seek(SeekFrom::End(-(TAIL_SIZE as i64))) {
                if !self.read_error_warned {
                    self.read_error_warned = true;
                    eprintln!(
                        "[AC] 警告: 定位 JSONL 文件末尾失败（仅提示一次） path={} err={}",
                        path.display(),
                        e
                    );
                }
                return;
            }
            // 读取剩余字节（约 TAIL_SIZE 大小）到 Vec<u8>
            let mut buf: Vec<u8> = Vec::with_capacity(TAIL_SIZE as usize);
            if let Err(e) = file.read_to_end(&mut buf) {
                if !self.read_error_warned {
                    self.read_error_warned = true;
                    eprintln!(
                        "[AC] 警告: 读取 JSONL 尾部字节失败（仅提示一次） path={} err={}",
                        path.display(),
                        e
                    );
                }
                return;
            }
            // 将字节解码为字符串：
            //   - seek 位置可能落在 UTF-8 多字节字符中间，因此 from_utf8 可能失败；
            //   - 失败时退回到 from_utf8_lossy，将非法字节替换为 U+FFFD。
            // 然后跳过第一行（很可能不完整）。
            let s = match String::from_utf8(buf) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            };
            // 跳过首行：seek 位置可能落在某条 JSONL 行的中间，
            // 第一个换行之前的内容大概率是半截行，丢弃以免污染解析。
            if let Some(pos) = s.find('\n') {
                s[pos + 1..].to_string()
            } else {
                // 整段没有换行：可能是单行特别长。保留原内容交给 serde_json::from_str 兜底，
                // 解析失败的行会被下方循环安全跳过。
                s
            }
        };

        // 从后向前遍历行，找到最后一条有效消息
        for line in content.lines().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // 尝试解析 JSON
            let msg: JsonlMessage = match serde_json::from_str(line) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // 提取角色信息
            // 兼容两种格式：
            //   Claude: role 在 msg.message.role 中
            //   Codex: role 可能在顶层 msg.role 或 msg.message.role 中
            let role = msg
                .role
                .clone()
                .or_else(|| msg.message.as_ref().and_then(|m| m.role.clone()));

            // 提取消息类型
            if let Some(ref msg_type) = msg.msg_type {
                self.last_message_type = Some(msg_type.clone());
            }

            // 记录角色
            if let Some(ref r) = role {
                self.last_message_role = Some(r.clone());
            }

            // 检查是否有错误内容
            self.check_for_errors(&msg);

            // 只需要最后一条有效消息
            break;
        }
    }

    /// 检查消息内容中是否包含错误信息
    ///
    /// 从消息中提取文本内容，与配置的错误关键词列表进行匹配。
    /// 文本内容可能来自以下位置：
    /// - 顶层 `content` 字段（Codex 格式）
    /// - 嵌套 `message.content` 字段（Claude 格式）
    /// - `content` 为 TextBlock 数组时，提取每个 block 的 `text` 字段
    ///
    /// # 参数
    /// - `msg`: 解析后的 JSONL 消息
    fn check_for_errors(&mut self, msg: &JsonlMessage) {
        // 尝试从多个位置提取文本内容
        let content_str = self.extract_content_text(msg);
        let content_str = match content_str {
            Some(s) if !s.is_empty() => s,
            _ => return,
        };

        // 将内容转为小写进行不区分大小写的匹配
        let lower = content_str.to_lowercase();

        // 遍历配置的错误关键词列表
        for indicator in &self.config.error_keywords {
            if lower.contains(indicator) {
                self.error_detected = true;
                self.error_message = indicator.to_string();
                return;
            }
        }
    }

    /// 从 JSONL 消息中提取文本内容
    ///
    /// 兼容多种消息格式：
    /// 1. 顶层 content 为字符串
    /// 2. 嵌套 message.content 为字符串
    /// 3. content 为 TextBlock 数组（提取每个 block 的 text 字段）
    ///
    /// # 参数
    /// - `msg`: 解析后的 JSONL 消息
    ///
    /// # 返回值
    /// 提取到的文本内容，提取失败返回 None
    fn extract_content_text(&self, msg: &JsonlMessage) -> Option<String> {
        // 优先检查顶层 content 字段（Codex 格式）
        if let serde_json::Value::String(ref s) = msg.content {
            return Some(s.clone());
        }

        // 检查顶层 content 为数组的情况
        if let serde_json::Value::Array(ref arr) = msg.content {
            let text = Self::extract_text_from_array(arr);
            if !text.is_empty() {
                return Some(text);
            }
        }

        // 检查嵌套 message.content 字段（Claude 格式）
        if let Some(ref message) = msg.message {
            match &message.content {
                serde_json::Value::String(s) => return Some(s.clone()),
                serde_json::Value::Array(arr) => {
                    let text = Self::extract_text_from_array(arr);
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// 从 TextBlock 数组中提取文本
    ///
    /// JSONL 中的 content 可能是 TextBlock 数组，
    /// 每个 block 可能包含一个 "text" 字段。
    ///
    /// # 参数
    /// - `arr`: JSON 数组
    ///
    /// # 返回值
    /// 拼接所有 TextBlock 的文本内容
    fn extract_text_from_array(arr: &[serde_json::Value]) -> String {
        let mut text = String::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                text.push_str(t);
                text.push('\n');
            }
        }
        text
    }
}

impl Detector for JsonlDetector {
    /// 初始化 JSONL 检测器
    ///
    /// 尝试定位最新的会话文件，并记录当前文件大小作为基准。
    fn init(&mut self, _cli_name: &str, _cli_args: &[String]) -> Result<()> {
        // 尝试找到最新的会话文件
        self.current_session_file = self.find_latest_session_file();

        // 记录当前文件大小作为基准
        if let Some(ref path) = self.current_session_file {
            if let Ok(meta) = std::fs::metadata(path) {
                self.last_file_size = meta.len();
            }
        }

        Ok(())
    }

    /// 处理输出数据
    ///
    /// JSONL 检测器主要依赖文件监控，但在收到输出时触发文件轮询，
    /// 以更及时地发现文件变化。
    fn feed_output(&mut self, _data: &[u8]) {
        // 触发文件轮询（会受轮询间隔限制）
        self.poll_session_file();
    }

    /// 查询当前 CLI 状态
    ///
    /// 判断逻辑优先级：
    /// 1. 检测到错误 → `Error`
    /// 2. 没有会话文件 → `Unknown`（fallback 到静默超时）
    /// 3. JSONL 文件正在增长（FILE_STABLE_SECS 秒内有变化） → `Busy`
    /// 4. 文件已稳定 + 超过静默阈值：
    ///    a. 最后消息角色为 assistant → `Idle`
    ///    b. 配置启用 summary 识别 + 最后消息类型为 summary → `Idle`
    ///    c. 其他 → `Unknown`
    /// 5. 文件已稳定但未超过静默阈值 → `Busy`（还没到发送时机）
    fn status(&self, silence_duration: Duration, silence_threshold: Duration) -> CliStatus {
        // 优先检查错误
        if self.error_detected {
            return CliStatus::Error {
                message: self.error_message.clone(),
            };
        }

        // 没有会话文件，无法精确判断
        if self.current_session_file.is_none() {
            return CliStatus::Unknown;
        }

        // 文件最近有变化 → 忙碌
        let file_stable_duration = self.last_size_change_time.elapsed();
        if file_stable_duration < Duration::from_secs(FILE_STABLE_SECS) {
            return CliStatus::Busy;
        }

        // 文件已稳定 + 超过静默阈值
        if silence_duration >= silence_threshold {
            // 最后一条消息是 assistant → CLI 已完成回复，处于空闲状态
            if let Some(ref role) = self.last_message_role {
                if role == "assistant" {
                    return CliStatus::Idle;
                }
            }

            // 如果配置了识别 summary 类型，则 summary 也视为空闲
            if self.config.recognize_summary {
                if let Some(ref msg_type) = self.last_message_type {
                    if msg_type == "summary" {
                        return CliStatus::Idle;
                    }
                }
            }

            // 其他情况，fallback
            return CliStatus::Unknown;
        }

        // 文件已稳定但未超过静默阈值 → 可能在等待，但还没到发送时机
        CliStatus::Busy
    }

    /// 重置检测状态
    ///
    /// 在发送 prompt 后调用，清除之前的检测结果，
    /// 准备检测下一轮输出。
    fn reset(&mut self) {
        self.error_detected = false;
        self.error_message.clear();
        self.last_message_type = None;
        self.last_message_role = None;
        // 重置文件变化时间，准备检测新的变化
        self.last_size_change_time = Instant::now();
    }

    /// 返回检测器名称（来自配置）
    fn name(&self) -> &str {
        &self.config.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建一个用于测试的 Claude 配置
    fn test_claude_config() -> JsonlDetectorConfig {
        JsonlDetectorConfig {
            name: "ClaudeDetector".to_string(),
            sessions_dir: PathBuf::from("/tmp/nonexistent_claude_test"),
            recursive_scan: false,
            max_scan_depth: 5,
            error_keywords: vec![
                "error occurred",
                "failed to",
                "i encountered an error",
                "an error happened",
                "i'm unable to",
                "cannot complete",
                "command failed",
                "compilation error",
                "build failed",
            ],
            recognize_summary: true,
        }
    }

    /// 创建一个用于测试的 Codex 配置
    fn test_codex_config() -> JsonlDetectorConfig {
        JsonlDetectorConfig {
            name: "CodexDetector".to_string(),
            sessions_dir: PathBuf::from("/tmp/nonexistent_codex_test"),
            recursive_scan: true,
            max_scan_depth: 5,
            error_keywords: vec![
                "error occurred",
                "failed to",
                "command failed",
                "compilation error",
                "build failed",
                "runtime error",
                "exit code",
            ],
            recognize_summary: false,
        }
    }

    /// 测试 Claude 配置的检测器名称
    #[test]
    fn test_claude_detector_name() {
        let detector = JsonlDetector::new(test_claude_config());
        assert_eq!(detector.name(), "ClaudeDetector");
    }

    /// 测试 Codex 配置的检测器名称
    #[test]
    fn test_codex_detector_name() {
        let detector = JsonlDetector::new(test_codex_config());
        assert_eq!(detector.name(), "CodexDetector");
    }

    /// 测试没有会话文件时返回 Unknown
    #[test]
    fn test_no_session_file_returns_unknown() {
        let detector = JsonlDetector::new(test_claude_config());
        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Unknown => {} // 预期结果
            _ => panic!("没有会话文件时应返回 Unknown"),
        }
    }

    /// 测试 Codex 配置下没有会话文件时返回 Unknown
    #[test]
    fn test_codex_no_session_file_returns_unknown() {
        let detector = JsonlDetector::new(test_codex_config());
        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Unknown => {} // 预期结果
            _ => panic!("没有会话文件时应返回 Unknown"),
        }
    }

    /// 测试 Claude 格式的错误检测
    #[test]
    fn test_claude_error_check() {
        let mut detector = JsonlDetector::new(test_claude_config());
        let msg = JsonlMessage {
            msg_type: Some("assistant".to_string()),
            role: None,
            content: serde_json::Value::Null,
            message: Some(MessageContent {
                role: Some("assistant".to_string()),
                content: serde_json::Value::String(
                    "I encountered an error while running the build".to_string(),
                ),
            }),
        };
        detector.check_for_errors(&msg);
        assert!(detector.error_detected);
    }

    /// 测试 Codex 格式的错误检测（顶层 content）
    #[test]
    fn test_codex_error_check() {
        let mut detector = JsonlDetector::new(test_codex_config());
        let msg = JsonlMessage {
            msg_type: Some("message".to_string()),
            role: Some("assistant".to_string()),
            content: serde_json::Value::String(
                "runtime error: null pointer dereference".to_string(),
            ),
            message: None,
        };
        detector.check_for_errors(&msg);
        assert!(detector.error_detected);
    }

    /// 测试正常消息不触发错误
    #[test]
    fn test_normal_message_no_error() {
        let mut detector = JsonlDetector::new(test_claude_config());
        let msg = JsonlMessage {
            msg_type: Some("assistant".to_string()),
            role: None,
            content: serde_json::Value::Null,
            message: Some(MessageContent {
                role: Some("assistant".to_string()),
                content: serde_json::Value::String(
                    "I've successfully completed the task".to_string(),
                ),
            }),
        };
        detector.check_for_errors(&msg);
        assert!(!detector.error_detected);
    }

    /// 测试重置功能
    #[test]
    fn test_reset() {
        let mut detector = JsonlDetector::new(test_claude_config());
        detector.error_detected = true;
        detector.error_message = "test error".to_string();
        detector.last_message_type = Some("assistant".to_string());
        detector.last_message_role = Some("assistant".to_string());

        detector.reset();

        assert!(!detector.error_detected);
        assert!(detector.error_message.is_empty());
        assert!(detector.last_message_type.is_none());
        assert!(detector.last_message_role.is_none());
    }

    /// 测试 TextBlock 数组内容提取
    #[test]
    fn test_extract_text_from_array() {
        let arr = vec![
            serde_json::json!({"type": "text", "text": "Hello "}),
            serde_json::json!({"type": "text", "text": "World"}),
        ];
        let result = JsonlDetector::extract_text_from_array(&arr);
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
    }

    /// 测试 summary 消息在 Claude 配置下被识别为 Idle
    #[test]
    fn test_summary_recognized_as_idle_for_claude() {
        let mut detector = JsonlDetector::new(test_claude_config());
        // 模拟有会话文件且文件已稳定
        detector.current_session_file = Some(PathBuf::from("/tmp/test.jsonl"));
        detector.last_size_change_time = Instant::now() - Duration::from_secs(10);
        detector.last_message_type = Some("summary".to_string());

        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Idle => {} // Claude 配置下 summary 应识别为 Idle
            _ => panic!("Claude 配置下 summary 消息应返回 Idle，但得到了: {:?}", status),
        }
    }

    /// 测试 summary 消息在 Codex 配置下不被识别为 Idle
    #[test]
    fn test_summary_not_recognized_for_codex() {
        let mut detector = JsonlDetector::new(test_codex_config());
        // 模拟有会话文件且文件已稳定
        detector.current_session_file = Some(PathBuf::from("/tmp/test.jsonl"));
        detector.last_size_change_time = Instant::now() - Duration::from_secs(10);
        detector.last_message_type = Some("summary".to_string());

        let status = detector.status(Duration::from_secs(60), Duration::from_secs(30));
        match status {
            CliStatus::Unknown => {} // Codex 配置下 summary 不应被识别
            _ => panic!(
                "Codex 配置下 summary 消息应返回 Unknown，但得到了: {:?}",
                status
            ),
        }
    }
}
