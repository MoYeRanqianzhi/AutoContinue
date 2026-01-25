//! # 虚拟终端模块 (terminal.rs)
//!
//! 该模块实现虚拟终端模拟器，用于：
//! - 解析 ANSI 转义序列
//! - 维护终端屏幕缓冲区（内容 + 颜色属性）
//! - 通过 Diff 检测实际新增内容
//! - 判断新增内容是否包含红色文本（错误检测）
//!
//! ## 核心原理
//! UI 重绘会清除并重写相同内容，Diff 无实质变化；
//! 实际输出会产生新增内容，可通过颜色判断是否为错误。

use std::sync::{Arc, Mutex};

/// 终端单元格，包含字符和颜色属性
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// 字符内容
    pub ch: char,
    /// 前景色（ANSI 颜色码）
    pub fg: Color,
    /// 背景色
    pub bg: Color,
    /// 是否加粗
    pub bold: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
        }
    }
}

/// ANSI 颜色
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    /// 默认颜色
    Default,
    /// 标准 16 色（0-15）
    Ansi(u8),
    /// 256 色
    Palette(u8),
    /// 24位真彩色 RGB
    Rgb(u8, u8, u8),
}

impl Color {
    /// 检查是否为红色系
    /// 包括：标准红色(1,9)、256色中的红色、RGB红色
    pub fn is_red(&self) -> bool {
        match self {
            // 标准红色：1=红色, 9=亮红色
            Color::Ansi(1) | Color::Ansi(9) => true,
            // 其他 ANSI 颜色不是红色
            Color::Ansi(_) => false,
            // 256 色调色板中的红色范围
            // 基本红色：1, 9
            // 扩展红色：52-57, 88-93, 124-129, 160-165, 196-201
            Color::Palette(n) => {
                matches!(n, 1 | 9 | 52..=57 | 88..=93 | 124..=129 | 160..=165 | 196..=201)
            }
            // RGB 红色：R 分量显著大于 G 和 B
            // 放宽条件：R > 128 且是主导色（R > G + 30 且 R > B + 30）
            Color::Rgb(r, g, b) => {
                *r > 128 && *r > *g + 30 && *r > *b + 30
            }
            Color::Default => false,
        }
    }
}

/// 解析器状态
#[derive(Clone, Debug, PartialEq)]
enum ParserState {
    /// 正常文本
    Normal,
    /// 收到 ESC (0x1B)
    Escape,
    /// 收到 CSI (ESC [)
    Csi,
    /// 收到 OSC (ESC ])
    Osc,
}

/// 虚拟终端
/// 维护终端屏幕缓冲区和状态，包含内置的 ANSI 解析器
pub struct VirtualTerminal {
    /// 屏幕宽度
    pub width: usize,
    /// 屏幕高度
    pub height: usize,
    /// 屏幕缓冲区
    pub buffer: Vec<Vec<Cell>>,
    /// 光标行
    pub cursor_row: usize,
    /// 光标列
    pub cursor_col: usize,
    /// 保存的光标行
    saved_cursor_row: usize,
    /// 保存的光标列
    saved_cursor_col: usize,
    /// 当前前景色
    pub current_fg: Color,
    /// 当前背景色
    pub current_bg: Color,
    /// 当前是否粗体
    pub current_bold: bool,
    /// 滚动区域顶部
    pub scroll_top: usize,
    /// 滚动区域底部
    pub scroll_bottom: usize,
    /// 新增内容追踪器
    /// 记录自上次快照以来新增的带颜色的字符
    new_content: Vec<(char, Color)>,
    /// 是否有新内容（非 UI 重绘）
    has_new_content: bool,
    // 解析器状态（集成到 VirtualTerminal 中）
    /// 解析器状态
    parser_state: ParserState,
    /// CSI 参数缓冲区
    parser_params: Vec<u16>,
    /// 当前参数值
    parser_current_param: u16,
    /// 中间字符
    parser_intermediate: Vec<char>,
}

impl VirtualTerminal {
    /// 创建新的虚拟终端
    pub fn new(width: usize, height: usize) -> Self {
        let buffer = vec![vec![Cell::default(); width]; height];
        VirtualTerminal {
            width,
            height,
            buffer,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            current_fg: Color::Default,
            current_bg: Color::Default,
            current_bold: false,
            scroll_top: 0,
            scroll_bottom: height,
            new_content: Vec::new(),
            has_new_content: false,
            parser_state: ParserState::Normal,
            parser_params: Vec::new(),
            parser_current_param: 0,
            parser_intermediate: Vec::new(),
        }
    }

    /// 重置解析器状态
    fn reset_parser(&mut self) {
        self.parser_state = ParserState::Normal;
        self.parser_params.clear();
        self.parser_current_param = 0;
        self.parser_intermediate.clear();
    }

    /// 重置终端状态
    pub fn reset(&mut self) {
        for row in &mut self.buffer {
            for cell in row {
                *cell = Cell::default();
            }
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.current_bold = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.height;
    }

    /// 处理输入数据
    pub fn process(&mut self, data: &[u8]) {
        for &byte in data {
            self.parse_byte(byte);
        }
    }

    /// 解析单个字节
    fn parse_byte(&mut self, byte: u8) {
        match self.parser_state {
            ParserState::Normal => {
                match byte {
                    0x1B => {
                        // ESC
                        self.parser_state = ParserState::Escape;
                    }
                    0x07 => {
                        // BEL - 忽略
                    }
                    0x08 => {
                        // BS - 退格
                        if self.cursor_col > 0 {
                            self.cursor_col -= 1;
                        }
                    }
                    0x09 => {
                        // TAB
                        self.cursor_col = (self.cursor_col + 8) & !7;
                        if self.cursor_col >= self.width {
                            self.cursor_col = self.width - 1;
                        }
                    }
                    0x0A | 0x0B | 0x0C => {
                        // LF, VT, FF - 换行
                        self.linefeed();
                    }
                    0x0D => {
                        // CR - 回车
                        self.cursor_col = 0;
                    }
                    0x00..=0x1F => {
                        // 其他控制字符 - 忽略
                    }
                    _ => {
                        // 可打印字符
                        self.put_char(byte as char);
                    }
                }
            }
            ParserState::Escape => {
                match byte {
                    b'[' => {
                        // CSI
                        self.parser_state = ParserState::Csi;
                        self.parser_params.clear();
                        self.parser_current_param = 0;
                        self.parser_intermediate.clear();
                    }
                    b']' => {
                        // OSC - 忽略
                        self.parser_state = ParserState::Osc;
                    }
                    b'7' => {
                        // DECSC - 保存光标
                        self.save_cursor();
                        self.reset_parser();
                    }
                    b'8' => {
                        // DECRC - 恢复光标
                        self.restore_cursor();
                        self.reset_parser();
                    }
                    b'M' => {
                        // RI - 反向换行
                        if self.cursor_row > 0 {
                            self.cursor_row -= 1;
                        }
                        self.reset_parser();
                    }
                    b'c' => {
                        // RIS - 重置终端
                        self.reset();
                        self.reset_parser();
                    }
                    _ => {
                        // 未知序列，忽略并重置
                        self.reset_parser();
                    }
                }
            }
            ParserState::Csi => {
                match byte {
                    b'0'..=b'9' => {
                        // 参数数字
                        self.parser_current_param = self.parser_current_param * 10 + (byte - b'0') as u16;
                    }
                    b';' | b':' => {
                        // 参数分隔符
                        self.parser_params.push(self.parser_current_param);
                        self.parser_current_param = 0;
                    }
                    b' '..=b'/' | b'?' | b'>' => {
                        // 中间字符或私有模式前缀
                        // 注：b'!' (33) 已包含在 b' '..=b'/' (32-47) 范围内
                        self.parser_intermediate.push(byte as char);
                    }
                    b'@'..=b'~' => {
                        // 终结符
                        self.parser_params.push(self.parser_current_param);
                        self.execute_csi(byte as char);
                        self.reset_parser();
                    }
                    _ => {
                        // 无效字符，重置
                        self.reset_parser();
                    }
                }
            }
            ParserState::Osc => {
                // OSC 序列，等待 ST (ESC \) 或 BEL
                if byte == 0x07 || byte == 0x1B {
                    self.reset_parser();
                }
            }
        }
    }

    /// 执行 CSI 序列
    fn execute_csi(&mut self, cmd: char) {
        let is_private = self.parser_intermediate.contains(&'?');

        match cmd {
            'A' => {
                // CUU - 光标上移
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                // CUD - 光标下移
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.height - 1);
            }
            'C' => {
                // CUF - 光标右移
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.width - 1);
            }
            'D' => {
                // CUB - 光标左移
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'E' => {
                // CNL - 光标下移到行首
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.height - 1);
                self.cursor_col = 0;
            }
            'F' => {
                // CPL - 光标上移到行首
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
                self.cursor_col = 0;
            }
            'G' => {
                // CHA - 光标移动到列
                let col = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = (col - 1).min(self.width - 1);
            }
            'H' | 'f' => {
                // CUP - 光标定位
                let row = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                let col = self.parser_params.get(1).copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = (row - 1).min(self.height - 1);
                self.cursor_col = (col - 1).min(self.width - 1);
            }
            'J' => {
                // ED - 清除屏幕
                let mode = self.parser_params.first().copied().unwrap_or(0);
                self.erase_display(mode);
            }
            'K' => {
                // EL - 清除行
                let mode = self.parser_params.first().copied().unwrap_or(0);
                self.erase_line(mode);
            }
            'L' => {
                // IL - 插入行
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.insert_lines(n);
            }
            'M' => {
                // DL - 删除行
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.delete_lines(n);
            }
            'P' => {
                // DCH - 删除字符
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.delete_chars(n);
            }
            'S' => {
                // SU - 向上滚动
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.scroll_up(n);
            }
            'T' => {
                // SD - 向下滚动
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.scroll_down(n);
            }
            'd' => {
                // VPA - 光标移动到行
                let row = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = (row - 1).min(self.height - 1);
            }
            'm' => {
                // SGR - 设置图形属性
                self.execute_sgr();
            }
            's' => {
                // SCP - 保存光标位置
                self.save_cursor();
            }
            'u' => {
                // RCP - 恢复光标位置
                self.restore_cursor();
            }
            'h' | 'l' => {
                // SM/RM - 设置/重置模式
                // 私有模式（如 ?1049 备用屏幕）暂时忽略
                let _ = is_private;
            }
            '@' => {
                // ICH - 插入空格
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.insert_chars(n);
            }
            'X' => {
                // ECH - 擦除字符
                let n = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                self.erase_chars(n);
            }
            'r' => {
                // DECSTBM - 设置滚动区域
                let top = self.parser_params.first().copied().unwrap_or(1).max(1) as usize;
                let bottom = self.parser_params.get(1).copied().unwrap_or(self.height as u16) as usize;
                self.scroll_top = top - 1;
                self.scroll_bottom = bottom.min(self.height);
            }
            _ => {
                // 未实现的 CSI 命令，忽略
            }
        }
    }

    /// 执行 SGR (Select Graphic Rendition) 序列
    fn execute_sgr(&mut self) {
        if self.parser_params.is_empty() || (self.parser_params.len() == 1 && self.parser_params[0] == 0) {
            // 重置所有属性
            self.current_fg = Color::Default;
            self.current_bg = Color::Default;
            self.current_bold = false;
            return;
        }

        let mut i = 0;
        while i < self.parser_params.len() {
            match self.parser_params[i] {
                0 => {
                    // 重置
                    self.current_fg = Color::Default;
                    self.current_bg = Color::Default;
                    self.current_bold = false;
                }
                1 => {
                    // 粗体
                    self.current_bold = true;
                }
                22 => {
                    // 取消粗体
                    self.current_bold = false;
                }
                30..=37 => {
                    // 标准前景色
                    self.current_fg = Color::Ansi((self.parser_params[i] - 30) as u8);
                }
                38 => {
                    // 扩展前景色
                    if i + 1 < self.parser_params.len() {
                        match self.parser_params[i + 1] {
                            5 if i + 2 < self.parser_params.len() => {
                                // 256 色
                                self.current_fg = Color::Palette(self.parser_params[i + 2] as u8);
                                i += 2;
                            }
                            2 if i + 4 < self.parser_params.len() => {
                                // RGB 真彩色
                                self.current_fg = Color::Rgb(
                                    self.parser_params[i + 2] as u8,
                                    self.parser_params[i + 3] as u8,
                                    self.parser_params[i + 4] as u8,
                                );
                                i += 4;
                            }
                            _ => {}
                        }
                    }
                }
                39 => {
                    // 默认前景色
                    self.current_fg = Color::Default;
                }
                40..=47 => {
                    // 标准背景色
                    self.current_bg = Color::Ansi((self.parser_params[i] - 40) as u8);
                }
                48 => {
                    // 扩展背景色
                    if i + 1 < self.parser_params.len() {
                        match self.parser_params[i + 1] {
                            5 if i + 2 < self.parser_params.len() => {
                                // 256 色
                                self.current_bg = Color::Palette(self.parser_params[i + 2] as u8);
                                i += 2;
                            }
                            2 if i + 4 < self.parser_params.len() => {
                                // RGB 真彩色
                                self.current_bg = Color::Rgb(
                                    self.parser_params[i + 2] as u8,
                                    self.parser_params[i + 3] as u8,
                                    self.parser_params[i + 4] as u8,
                                );
                                i += 4;
                            }
                            _ => {}
                        }
                    }
                }
                49 => {
                    // 默认背景色
                    self.current_bg = Color::Default;
                }
                90..=97 => {
                    // 亮色前景
                    self.current_fg = Color::Ansi((self.parser_params[i] - 90 + 8) as u8);
                }
                100..=107 => {
                    // 亮色背景
                    self.current_bg = Color::Ansi((self.parser_params[i] - 100 + 8) as u8);
                }
                _ => {
                    // 其他属性，忽略
                }
            }
            i += 1;
        }
    }

    /// 输出一个字符到当前位置
    fn put_char(&mut self, ch: char) {
        if self.cursor_col >= self.width {
            // 自动换行
            self.cursor_col = 0;
            self.linefeed();
        }

        if self.cursor_row < self.height && self.cursor_col < self.width {
            let cell = &mut self.buffer[self.cursor_row][self.cursor_col];
            cell.ch = ch;
            cell.fg = self.current_fg;
            cell.bg = self.current_bg;
            cell.bold = self.current_bold;

            // 记录新增内容（非空格字符）
            if ch != ' ' {
                self.new_content.push((ch, self.current_fg));
                self.has_new_content = true;
            }

            self.cursor_col += 1;
        }
    }

    /// 换行
    fn linefeed(&mut self) {
        if self.cursor_row + 1 >= self.scroll_bottom {
            // 需要滚动
            self.scroll_up(1);
        } else {
            self.cursor_row += 1;
        }
    }

    /// 向上滚动
    fn scroll_up(&mut self, n: usize) {
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;

        for _ in 0..n {
            if top + 1 < bottom {
                // 移除顶行，底部添加空行
                for row in top..bottom - 1 {
                    self.buffer[row] = self.buffer[row + 1].clone();
                }
                self.buffer[bottom - 1] = vec![Cell::default(); self.width];
            }
        }
    }

    /// 向下滚动
    fn scroll_down(&mut self, n: usize) {
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;

        for _ in 0..n {
            if top + 1 < bottom {
                // 移除底行，顶部添加空行
                for row in (top + 1..bottom).rev() {
                    self.buffer[row] = self.buffer[row - 1].clone();
                }
                self.buffer[top] = vec![Cell::default(); self.width];
            }
        }
    }

    /// 保存光标位置
    fn save_cursor(&mut self) {
        self.saved_cursor_row = self.cursor_row;
        self.saved_cursor_col = self.cursor_col;
    }

    /// 恢复光标位置
    fn restore_cursor(&mut self) {
        self.cursor_row = self.saved_cursor_row;
        self.cursor_col = self.saved_cursor_col;
    }

    /// 清除屏幕
    fn erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                // 从光标到屏幕末尾
                for col in self.cursor_col..self.width {
                    self.buffer[self.cursor_row][col] = Cell::default();
                }
                for row in self.cursor_row + 1..self.height {
                    self.buffer[row] = vec![Cell::default(); self.width];
                }
            }
            1 => {
                // 从屏幕开头到光标
                for row in 0..self.cursor_row {
                    self.buffer[row] = vec![Cell::default(); self.width];
                }
                for col in 0..=self.cursor_col.min(self.width - 1) {
                    self.buffer[self.cursor_row][col] = Cell::default();
                }
            }
            2 | 3 => {
                // 整个屏幕
                for row in &mut self.buffer {
                    *row = vec![Cell::default(); self.width];
                }
            }
            _ => {}
        }
    }

    /// 清除行
    fn erase_line(&mut self, mode: u16) {
        let row = self.cursor_row;
        if row >= self.height {
            return;
        }

        match mode {
            0 => {
                // 从光标到行尾
                for col in self.cursor_col..self.width {
                    self.buffer[row][col] = Cell::default();
                }
            }
            1 => {
                // 从行首到光标
                for col in 0..=self.cursor_col.min(self.width - 1) {
                    self.buffer[row][col] = Cell::default();
                }
            }
            2 => {
                // 整行
                self.buffer[row] = vec![Cell::default(); self.width];
            }
            _ => {}
        }
    }

    /// 插入行
    fn insert_lines(&mut self, n: usize) {
        let row = self.cursor_row;
        let bottom = self.scroll_bottom;

        for _ in 0..n {
            if row < bottom {
                // 底部行移出，当前行插入空行
                for r in (row + 1..bottom).rev() {
                    self.buffer[r] = self.buffer[r - 1].clone();
                }
                self.buffer[row] = vec![Cell::default(); self.width];
            }
        }
    }

    /// 删除行
    fn delete_lines(&mut self, n: usize) {
        let row = self.cursor_row;
        let bottom = self.scroll_bottom;

        for _ in 0..n {
            if row < bottom {
                // 当前行移出，底部添加空行
                for r in row..bottom - 1 {
                    self.buffer[r] = self.buffer[r + 1].clone();
                }
                self.buffer[bottom - 1] = vec![Cell::default(); self.width];
            }
        }
    }

    /// 删除字符
    fn delete_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;

        if row >= self.height {
            return;
        }

        for _ in 0..n {
            if col < self.width - 1 {
                for c in col..self.width - 1 {
                    self.buffer[row][c] = self.buffer[row][c + 1].clone();
                }
            }
            self.buffer[row][self.width - 1] = Cell::default();
        }
    }

    /// 插入空格
    fn insert_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;

        if row >= self.height {
            return;
        }

        for _ in 0..n {
            for c in (col + 1..self.width).rev() {
                self.buffer[row][c] = self.buffer[row][c - 1].clone();
            }
            self.buffer[row][col] = Cell::default();
        }
    }

    /// 擦除字符
    fn erase_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;

        if row >= self.height {
            return;
        }

        for i in 0..n {
            if col + i < self.width {
                self.buffer[row][col + i] = Cell::default();
            }
        }
    }

    /// 检查新增内容中是否包含红色文本
    /// 返回 true 表示检测到错误
    pub fn has_red_content(&self) -> bool {
        self.new_content.iter().any(|(_, color)| color.is_red())
    }

    /// 获取新增内容中的红色文本
    pub fn get_red_content(&self) -> String {
        self.new_content
            .iter()
            .filter(|(_, color)| color.is_red())
            .map(|(ch, _)| *ch)
            .collect()
    }

    /// 获取新增内容中所有非默认颜色的调试信息
    /// 用于诊断颜色检测问题
    pub fn get_color_debug_info(&self) -> String {
        use std::collections::HashMap;
        let mut color_counts: HashMap<String, usize> = HashMap::new();

        for (_, color) in &self.new_content {
            if *color != Color::Default {
                let key = format!("{:?}", color);
                *color_counts.entry(key).or_insert(0) += 1;
            }
        }

        if color_counts.is_empty() {
            return String::from("无非默认颜色");
        }

        color_counts
            .into_iter()
            .map(|(color, count)| format!("{}({})", color, count))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// 清除新增内容追踪器（在发送提示词后调用）
    /// 同时重置颜色状态，避免残留的红色状态影响后续检测
    pub fn clear_new_content(&mut self) {
        self.new_content.clear();
        self.has_new_content = false;
        // 重置颜色状态，避免CLI未重置颜色导致后续误判
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.current_bold = false;
    }

    /// 检查是否有新内容
    pub fn has_new_output(&self) -> bool {
        self.has_new_content
    }
}

/// 共享的终端状态，用于多线程访问
pub type SharedTerminal = Arc<Mutex<VirtualTerminal>>;

/// 创建共享终端
pub fn create_shared_terminal(width: usize, height: usize) -> SharedTerminal {
    Arc::new(Mutex::new(VirtualTerminal::new(width, height)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_is_red() {
        assert!(Color::Ansi(1).is_red());
        assert!(Color::Ansi(9).is_red());
        assert!(!Color::Ansi(2).is_red());
        assert!(Color::Rgb(200, 50, 50).is_red());
        assert!(!Color::Rgb(50, 200, 50).is_red());
    }

    #[test]
    fn test_basic_text() {
        let mut term = VirtualTerminal::new(80, 24);
        term.process(b"Hello");

        assert_eq!(term.buffer[0][0].ch, 'H');
        assert_eq!(term.buffer[0][4].ch, 'o');
        assert_eq!(term.cursor_col, 5);
    }

    #[test]
    fn test_red_detection() {
        let mut term = VirtualTerminal::new(80, 24);
        // ESC[31m = 红色前景
        term.process(b"\x1b[31mError\x1b[0m");

        assert!(term.has_red_content());
        assert_eq!(term.get_red_content(), "Error");
    }

    #[test]
    fn test_cursor_movement() {
        let mut term = VirtualTerminal::new(80, 24);
        term.process(b"\x1b[5;10H"); // 移动到第5行第10列

        assert_eq!(term.cursor_row, 4);
        assert_eq!(term.cursor_col, 9);
    }

    #[test]
    fn test_clear_screen() {
        let mut term = VirtualTerminal::new(80, 24);
        term.process(b"Hello\x1b[2J");

        // 屏幕应该被清空
        assert_eq!(term.buffer[0][0].ch, ' ');
    }
}
