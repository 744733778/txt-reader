use crate::config::{Config, ProgressRecord};
use crate::fonts;
use crate::pagination;
use crate::toc::{ChapterMatcher, TocItem};
use eframe::egui;
use egui::{Color32, FontFamily, FontId, Pos2, Vec2};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

const FONT_MIN: f32 = 14.0;
const FONT_MAX: f32 = 32.0;
const TITLEBAR_H: f32 = 38.0;
/// 固定老板键：全局快捷键，按一下隐藏/显示窗口
const BOSS_KEY: &str = "Alt+Z";

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

fn color_to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn parse_hotkey(s: &str) -> Option<HotKey> {
    HotKey::from_str(s.trim()).ok()
}

// ---------------------------------------------------------------------------
// Win32 窗口直控：老板键事件由专用线程作为通道唯一消费者直接操作原生窗口。
// 背景：eframe 只在 RedrawRequested 事件时运行帧，而 Windows 隐藏窗口收不到
// WM_PAINT，事件循环会睡死（帧不运行、update() 不执行），通道事件若只靠
// update() 轮询将永远无法处理（表现为：老板键隐藏窗口后无法唤回）。
// 因此事件到达时由线程直接调用 Win32 API 显示/隐藏，完全不依赖帧循环。
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod win32 {
    use std::sync::atomic::{AtomicIsize, Ordering};

    const SW_HIDE: i32 = 0;
    const SW_SHOW: i32 = 5;

    #[link(name = "user32")]
    extern "system" {
        fn IsWindowVisible(h_wnd: isize) -> i32;
        fn ShowWindow(h_wnd: isize, n_cmd_show: i32) -> i32;
        fn SetForegroundWindow(h_wnd: isize) -> i32;
        fn EnumWindows(
            lp_enum_func: Option<unsafe extern "system" fn(isize, isize) -> i32>,
            l_param: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(h_wnd: isize, lpdw_process_id: *mut u32) -> u32;
    }

    static FOUND_HWND: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn enum_cb(h_wnd: isize, l_param: isize) -> i32 {
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(h_wnd, &mut pid) };
        if pid as isize == l_param {
            FOUND_HWND.store(h_wnd, Ordering::SeqCst);
            return 0; // 停止枚举
        }
        1
    }

    /// 通过进程 ID 找到本应用的主窗口句柄（隐藏窗口同样会被枚举到）
    fn find_window() -> Option<isize> {
        FOUND_HWND.store(0, Ordering::SeqCst);
        unsafe {
            EnumWindows(Some(enum_cb), std::process::id() as isize);
        }
        let h = FOUND_HWND.load(Ordering::SeqCst);
        if h == 0 {
            None
        } else {
            Some(h)
        }
    }

    /// 切换窗口可见状态：隐藏 ⇄ 显示
    pub fn toggle_window() {
        let Some(h) = find_window() else { return };
        unsafe {
            if IsWindowVisible(h) != 0 {
                ShowWindow(h, SW_HIDE);
            } else {
                ShowWindow(h, SW_SHOW);
                SetForegroundWindow(h);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod win32 {
    pub fn toggle_window() {}
}

pub struct ReaderApp {
    pub config: Config,
    text: String,
    file_name: String,
    file_path: Option<PathBuf>,
    page_starts: Vec<usize>,
    k: usize,
    font_size: f32,
    line_height: f32,
    fg: Color32,
    bg: Color32,
    font_name: String,
    font_family: FontFamily,
    enc: String,
    menu_open: bool,
    toc_open: bool,
    toast: Option<String>,
    toast_until: f64,
    auto_ms: u64,
    auto_running: bool,
    auto_last_tick: Option<Instant>,
    toc: Vec<TocItem>,
    chapter_matcher: ChapterMatcher,
    available_fonts: BTreeMap<String, PathBuf>,
    /// 持有 GlobalHotKeyManager 使全局热键注册保持有效（Drop 时会注销热键）
    _hotkey_manager: Option<GlobalHotKeyManager>,
    _current_hotkey: Option<HotKey>,
    pending_open: Option<PathBuf>,
    last_bounds_save: Instant,
}

impl ReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        let available_fonts = fonts::available_chinese_fonts();
        {
            let mut fonts = egui::FontDefinitions::default();
            fonts::install_fonts(&mut fonts, &available_fonts);
            cc.egui_ctx.set_fonts(fonts);
        }

        let hotkey_manager = GlobalHotKeyManager::new().ok();
        let current_hotkey = parse_hotkey(BOSS_KEY);
        if let (Some(mgr), Some(hk)) = (&hotkey_manager, &current_hotkey) {
            let _ = mgr.register(*hk);
        }

        // 老板键事件：由专用线程作为通道唯一消费者处理，直接调用 Win32 操作窗口。
        // 原因：eframe 只在 RedrawRequested 时运行帧，隐藏窗口收不到 WM_PAINT，
        // 事件循环会睡死，通道事件若只靠 update() 轮询将永远无法处理
        // （表现：老板键隐藏窗口后无法唤回）。
        {
            let rx = GlobalHotKeyEvent::receiver().clone();
            std::thread::spawn(move || {
                while let Ok(ev) = rx.recv() {
                    if ev.state == global_hotkey::HotKeyState::Pressed {
                        win32::toggle_window();
                    }
                }
            });
        }

        let mut app = Self {
            config: config.clone(),
            text: String::new(),
            file_name: String::new(),
            file_path: None,
            page_starts: vec![0],
            k: 0,
            font_size: 19.0,
            line_height: 1.9,
            fg: Color32::from_rgb(0x33, 0x2d, 0x24),
            bg: Color32::from_rgb(0xf3, 0xec, 0xdd),
            font_name: String::new(),
            font_family: FontFamily::Proportional,
            enc: "utf-8".to_string(),
            menu_open: false,
            toc_open: false,
            toast: None,
            toast_until: 0.0,
            auto_ms: 1000,
            auto_running: false,
            auto_last_tick: None,
            toc: Vec::new(),
            chapter_matcher: ChapterMatcher::new(),
            available_fonts,
            _hotkey_manager: hotkey_manager,
            _current_hotkey: current_hotkey,
            pending_open: None,
            last_bounds_save: Instant::now(),
        };

        // 默认使用系统中文字体（若有）
        if app.font_name.is_empty() {
            if let Some(first_font) = app.available_fonts.keys().next().cloned() {
                app.set_font(&first_font);
            }
        }

        if app.config.auto_open_last {
            if let Some(last) = app.config.last_file.clone() {
                if last.exists() {
                    app.pending_open = Some(last);
                }
            }
        }

        app
    }

    fn line_height_px(&self) -> f32 {
        self.font_size * self.line_height
    }

    fn cur_start(&self) -> usize {
        self.page_starts.get(self.k).copied().unwrap_or(0)
    }

    fn total(&self) -> usize {
        self.text.len()
    }

    fn current_time(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    fn show_toast(&mut self, msg: String) {
        self.toast = Some(msg);
        self.toast_until = self.current_time() + 2.6;
    }

    fn set_font(&mut self, name: &str) {
        self.font_name = name.to_string();
        if name.is_empty() {
            self.font_family = FontFamily::Proportional;
        } else {
            self.font_family = FontFamily::Name(name.to_string().into());
        }
    }

    fn load_file(&mut self, path: &Path) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.show_toast(format!("读取文件失败：{}", e));
                return;
            }
        };
        let (text, enc_used) = if self.enc == "gbk" {
            let (cow, _, _) = encoding_rs::GBK.decode(&bytes);
            (cow.into_owned(), "gbk")
        } else {
            match std::str::from_utf8(&bytes) {
                Ok(s) => (s.to_string(), "utf-8"),
                Err(_) => {
                    let (cow, _, _) = encoding_rs::GBK.decode(&bytes);
                    (cow.into_owned(), "gbk")
                }
            }
        };
        self.enc = enc_used.to_string();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.apply_text(name, text, Some(path.to_path_buf()));
    }

    fn apply_text(&mut self, name: String, text: String, path: Option<PathBuf>) {
        self.stop_auto();
        let text = text.replace('\u{FEFF}', "").replace("\r\n", "\n").replace('\r', "\n");
        self.file_name = name;
        self.file_path = path.clone();
        self.text = text;
        self.page_starts = vec![0];
        self.k = 0;
        self.toc.clear();

        if self.text.is_empty() {
            self.show_toast("文件内容为空".to_string());
            return;
        }

        if let Some(p) = &path {
            self.config.last_file = Some(p.clone());
        }

        let key = path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file_name.clone());
        let rec = self.config.progress.get(&key).cloned();

        if let Some(rec) = &rec {
            if !rec.font.is_empty() && self.available_fonts.contains_key(&rec.font) {
                self.set_font(&rec.font);
            }
            if (200..=2000).contains(&rec.auto_ms) {
                self.auto_ms = rec.auto_ms;
            }
        }

        self.toc = self.chapter_matcher.build(&self.text);

        if let Some(rec) = rec {
            if rec.total_length == self.total() {
                self.font_size = rec.font_size.clamp(FONT_MIN, FONT_MAX);
                self.line_height = rec.line_height.clamp(1.4, 2.6);
                if let Some(c) = parse_hex(&rec.fg) {
                    self.fg = c;
                }
                if let Some(c) = parse_hex(&rec.bg) {
                    self.bg = c;
                }
                let offset = rec.offset.min(self.total());
                self.page_starts = vec![offset];
                self.show_toast(format!(
                    "已恢复上次阅读进度（{:.1}%）",
                    offset as f64 / self.total() as f64 * 100.0
                ));
            } else {
                self.font_size = rec.font_size.clamp(FONT_MIN, FONT_MAX);
                self.line_height = rec.line_height.clamp(1.4, 2.6);
                if let Some(c) = parse_hex(&rec.fg) {
                    self.fg = c;
                }
                if let Some(c) = parse_hex(&rec.bg) {
                    self.bg = c;
                }
                self.show_toast("文件内容已变化，阅读进度已作废".to_string());
            }
        }

        self.save_progress();
    }

    fn save_progress(&mut self) {
        let key = self
            .file_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file_name.clone());
        if key.is_empty() || self.total() == 0 {
            return;
        }
        let rec = ProgressRecord {
            name: self.file_name.clone(),
            offset: self.cur_start(),
            total_length: self.total(),
            font_size: self.font_size,
            line_height: self.line_height,
            fg: color_to_hex(self.fg),
            bg: color_to_hex(self.bg),
            font: self.font_name.clone(),
            auto_ms: self.auto_ms,
            saved_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        };
        self.config.progress.insert(key, rec);
        self.config.save();
    }

    fn next_page(&mut self, ctx: &egui::Context, max_width: f32, page_height: f32) {
        if self.text.is_empty() {
            return;
        }
        self.stop_auto();
        if self.k + 1 < self.page_starts.len() {
            self.k += 1;
        } else {
            let start = self.cur_start();
            let end = pagination::page_end_at(
                ctx,
                &self.text,
                start,
                self.font_size,
                self.line_height_px(),
                max_width,
                page_height,
                &self.font_family,
            );
            if end == 0 {
                self.show_toast("已到全书末尾".to_string());
                return;
            }
            self.page_starts.push(start + end);
            self.k += 1;
        }
        self.save_progress();
    }

    fn prev_page(&mut self, ctx: &egui::Context, max_width: f32, page_height: f32) {
        if self.text.is_empty() {
            return;
        }
        self.stop_auto();
        if self.k > 0 {
            self.k -= 1;
        } else {
            let t = self.cur_start();
            if t == 0 {
                self.show_toast("已在全书开头".to_string());
                return;
            }
            let b = pagination::prev_boundary(
                ctx,
                &self.text,
                t,
                self.font_size,
                self.line_height_px(),
                max_width,
                page_height,
                &self.font_family,
            );
            self.page_starts.insert(0, b);
            self.k = 0;
        }
        self.save_progress();
    }

    fn jump_to(&mut self, offset: usize) {
        self.stop_auto();
        let offset = offset.min(self.total());
        self.page_starts = vec![offset];
        self.k = 0;
        self.save_progress();
    }

    fn start_auto(&mut self) {
        if self.text.is_empty() || self.auto_running {
            return;
        }
        self.auto_running = true;
        self.auto_last_tick = Some(Instant::now());
        self.menu_open = false;
        self.show_toast(format!(
            "自动翻页已开启（{:.2}s/行），空格键停止",
            self.auto_ms as f64 / 1000.0
        ));
    }

    fn stop_auto(&mut self) {
        if self.auto_running {
            self.auto_running = false;
            self.auto_last_tick = None;
            self.save_progress();
        }
    }

    fn toggle_auto(&mut self) {
        if self.auto_running {
            self.stop_auto();
            self.show_toast("自动翻页已停止".to_string());
        } else {
            self.start_auto();
        }
    }

    fn auto_tick(&mut self, ctx: &egui::Context, max_width: f32) {
        if self.text.is_empty() {
            return;
        }
        let line_end = pagination::line_end_at(
            ctx,
            &self.text,
            self.cur_start(),
            self.font_size,
            self.line_height_px(),
            max_width,
            &self.font_family,
        );
        if line_end >= self.total() {
            self.stop_auto();
            self.show_toast("已到全书末尾".to_string());
            return;
        }
        self.page_starts.push(line_end);
        self.k += 1;
        if self.page_starts.len() > 4000 {
            self.page_starts.drain(0..2000);
            self.k = self.k.saturating_sub(2000);
        }
        if self.k % 8 == 0 {
            self.save_progress();
        }
    }
}

impl eframe::App for ReaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(p) = self.pending_open.take() {
            self.load_file(&p);
        }

        // 自动翻页
        if self.auto_running {
            if let Some(last) = self.auto_last_tick {
                if last.elapsed() >= Duration::from_millis(self.auto_ms) {
                    let max_w = self.reader_max_width(ctx);
                    self.auto_tick(ctx, max_w);
                    self.auto_last_tick = Some(Instant::now());
                }
            }
            ctx.request_repaint();
        }

        // 拖拽文件
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(p) = dropped.first() {
            self.load_file(p);
        }

        // 标题栏
        self.draw_titlebar(ctx);

        // 主区域
        if self.text.is_empty() {
            self.draw_landing(ctx);
        } else {
            self.draw_reader(ctx);
        }

        if self.menu_open {
            self.draw_menu(ctx);
        }
        if self.toc_open {
            self.draw_toc(ctx);
        }

        self.draw_toast(ctx);

        // 窗口边界保存（防抖）
        if self.last_bounds_save.elapsed() > Duration::from_millis(300) {
            self.save_window_bounds(ctx);
            self.last_bounds_save = Instant::now();
        }

        // 始终置顶
        if self.config.always_on_top {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        }

        if self.auto_running || self.toast.is_some() {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_progress();
        self.config.save();
    }
}

impl ReaderApp {
    fn reader_page_height(&self, ctx: &egui::Context) -> f32 {
        let screen = ctx.screen_rect();
        (screen.height() - TITLEBAR_H - 60.0).max(40.0)
    }

    fn reader_max_width(&self, ctx: &egui::Context) -> f32 {
        let screen = ctx.screen_rect();
        (screen.width() - 60.0).max(100.0)
    }

    fn save_window_bounds(&mut self, ctx: &egui::Context) {
        let rect = ctx.screen_rect();
        self.config.window_width = rect.width();
        self.config.window_height = rect.height();
        // 用视图外框（含标题栏，逻辑点坐标）记忆窗口位置；
        // ctx.screen_rect() 始终以 (0,0) 为原点，不能用于位置记忆
        if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
            self.config.window_x = Some(outer.min.x.round() as i32);
            self.config.window_y = Some(outer.min.y.round() as i32);
        }
    }

    fn draw_titlebar(&mut self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::new("titlebar"))
            .fixed_pos(Pos2::new(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(ctx.screen_rect().width(), TITLEBAR_H),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add_space(8.0);
                        if ui.add(egui::Button::new("打开").min_size(Vec2::new(50.0, 26.0))).clicked() {
                            self.open_file_dialog();
                        }
                        ui.add_space(6.0);
                        let mut enc = self.enc.clone();
                        egui::ComboBox::from_id_salt("enc_cb")
                            .selected_text(self.enc.to_uppercase())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut enc, "utf-8".to_string(), "UTF-8");
                                ui.selectable_value(&mut enc, "gbk".to_string(), "GBK");
                            });
                        if enc != self.enc {
                            self.enc = enc;
                            if let Some(p) = self.file_path.clone() {
                                self.load_file(&p);
                            }
                        }

                        ui.add_space(8.0);
                        let fname = if self.file_name.is_empty() {
                            "未打开文件".to_string()
                        } else {
                            self.file_name.clone()
                        };
                        ui.label(
                            egui::RichText::new(&fname)
                                .size(13.0)
                                .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                        );

                        ui.add_space(12.0);
                        if ui.add(egui::Button::new("老板键").min_size(Vec2::new(60.0, 26.0))).clicked() {
                            win32::toggle_window();
                        }
                        ui.add_space(8.0);
                        let pct = if self.total() > 0 {
                            self.cur_start() as f64 / self.total() as f64 * 100.0
                        } else {
                            0.0
                        };
                        ui.label(
                            egui::RichText::new(format!("{:.1}%", pct))
                                .size(12.0)
                                .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                        );
                        ui.add_space(8.0);
                    },
                );
            });
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("文本文件", &["txt", "text"])
            .add_filter("所有文件", &["*"])
            .pick_file()
        {
            self.load_file(&path);
        }
    }

    fn draw_landing(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.bg))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.heading(
                        egui::RichText::new("本地 TXT 阅读器")
                            .size(28.0)
                            .color(Color32::from_rgb(0x33, 0x2d, 0x24)),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("Rust 版 · 老板键 · 窗口记忆 · 自动恢复进度 · 极小窗口")
                            .size(14.0)
                            .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                    );
                    ui.add_space(24.0);
                    if ui.add(egui::Button::new("打开 TXT 文件").min_size(Vec2::new(160.0, 44.0))).clicked() {
                        self.open_file_dialog();
                    }
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("也可将 TXT 文件直接拖入窗口")
                            .size(12.0)
                            .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                    );
                });
            });
    }

    fn draw_reader(&mut self, ctx: &egui::Context) {
        let page_height = self.reader_page_height(ctx);
        let max_width = self.reader_max_width(ctx);

        if !self.menu_open && !self.toc_open {
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.prev_page(ctx, max_width, page_height);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.next_page(ctx, max_width, page_height);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
                self.toggle_auto();
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
                self.open_file_dialog();
            }
        }

        let scroll = ctx.input(|i| i.raw_scroll_delta.y);
        if !self.menu_open && !self.toc_open && scroll.abs() > 0.0 {
            if scroll < 0.0 {
                self.next_page(ctx, max_width, page_height);
            } else {
                self.prev_page(ctx, max_width, page_height);
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.bg))
            .show(ctx, |ui| {
                ui.add_space(TITLEBAR_H);
                let avail = ui.available_rect_before_wrap();
                let left = avail.left() + 30.0;
                let top = avail.top() + 24.0;
                let content_w = (avail.width() - 60.0).max(100.0);
                let content_h = (avail.height() - 48.0).max(40.0);

                // 点击翻页（在借用 self.text 之前处理，避免借用冲突）
                // 菜单/目录打开期间不响应全局点击，否则点击菜单里的下拉框等控件
                // 会被这里当作“点中间区域翻页/开关菜单”而误关菜单
                let click = if !self.menu_open && !self.toc_open {
                    ctx.input(|i| {
                        if i.pointer.any_click() {
                            i.pointer.hover_pos()
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                if let Some(pos) = click {
                    if pos.y > TITLEBAR_H && pos.y < avail.bottom() {
                        let rel_x = pos.x - left;
                        if rel_x >= 0.0 && rel_x <= content_w {
                            if rel_x < content_w / 3.0 {
                                self.prev_page(ctx, max_width, page_height);
                            } else if rel_x < content_w * 2.0 / 3.0 {
                                self.menu_open = !self.menu_open;
                            } else {
                                self.next_page(ctx, max_width, page_height);
                            }
                        }
                    }
                }

                let start = self.cur_start();
                let end = pagination::page_end_at(
                    ctx,
                    &self.text,
                    start,
                    self.font_size,
                    self.line_height_px(),
                    content_w,
                    content_h,
                    &self.font_family,
                );
                let page_text = &self.text[start..(start + end).min(self.text.len())];

                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = content_w;
                job.wrap.break_anywhere = false;
                job.append(
                    page_text,
                    0.0,
                    egui::text::TextFormat {
                        font_id: FontId::new(self.font_size, self.font_family.clone()),
                        color: self.fg,
                        line_height: Some(self.line_height_px()),
                        ..Default::default()
                    },
                );
                let galley = ctx.fonts(|f| f.layout_job(job));
                let text_pos = Pos2::new(left, top);
                ui.painter().galley(text_pos, galley, self.fg);

                let pct = if self.total() > 0 {
                    self.cur_start() as f32 / self.total() as f32
                } else {
                    0.0
                };
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        Pos2::new(avail.left(), avail.bottom() - 3.0),
                        Vec2::new(avail.width() * pct, 3.0),
                    ),
                    0.0,
                    Color32::from_rgb(0x3e, 0x6b, 0x57),
                );

                if self.auto_running {
                    let pill_text = format!("自动翻页 · {:.2}s/行", self.auto_ms as f64 / 1000.0);
                    let pill_pos = Pos2::new(avail.center().x - 70.0, top - 4.0);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(pill_pos, Vec2::new(140.0, 22.0)),
                        11.0,
                        Color32::from_rgb(0x3e, 0x6b, 0x57),
                    );
                    ui.painter().text(
                        pill_pos + Vec2::new(70.0, 11.0),
                        egui::Align2::CENTER_CENTER,
                        pill_text,
                        FontId::new(12.0, FontFamily::Proportional),
                        Color32::from_rgb(0xfd, 0xfa, 0xf1),
                    );
                }
            });
    }

    fn draw_menu(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        let panel_w = 420.0_f32.min(screen.width() - 40.0);
        let panel_h = (screen.height() - 80.0).min(600.0);

        // 背景点击捕获层：用 Middle 顺序位于菜单面板与 ComboBox 弹出层（Foreground）之下。
        // 这样点击菜单面板或下拉选项时，会被前景组件“消费”，本层不会触发；
        // 只有点击在所有前景组件之外的空白区域时，本层才会返回 clicked()，从而关闭菜单。
        let bg_clicked = egui::Area::new(egui::Id::new("menu_bg"))
            .order(egui::Order::Middle)
            .interactable(true)
            .fixed_pos(Pos2::ZERO)
            .show(ctx, |ui| ui.allocate_rect(screen, egui::Sense::click()).clicked())
            .inner;

        egui::Area::new(egui::Id::new("menu_overlay"))
            .fixed_pos(Pos2::new((screen.width() - panel_w) / 2.0, (screen.height() - panel_h) / 2.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Color32::from_rgb(0xfd, 0xfa, 0xf1))
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        ui.set_width(panel_w - 32.0);
                        ui.set_height(panel_h - 32.0);
                        ui.heading(
                            egui::RichText::new("阅读设置")
                                .size(17.0)
                                .color(Color32::from_rgb(0x33, 0x2d, 0x24)),
                        );
                        ui.separator();
                        let scroll_h = panel_h - 80.0;
                        egui::ScrollArea::vertical().max_height(scroll_h).show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("阅读")
                                    .size(12.0)
                                    .color(Color32::from_rgb(0x3e, 0x6b, 0x57))
                                    .strong(),
                            );

                            ui.horizontal(|ui| {
                                ui.label("字号");
                                ui.add_space(20.0);
                                if ui.button("A−").clicked() {
                                    self.font_size = (self.font_size - 2.0).max(FONT_MIN);
                                    self.save_progress();
                                }
                                ui.label(format!("{:.0}px", self.font_size));
                                if ui.button("A+").clicked() {
                                    self.font_size = (self.font_size + 2.0).min(FONT_MAX);
                                    self.save_progress();
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label("行距");
                                ui.add_space(20.0);
                                ui.add(egui::Slider::new(&mut self.line_height, 1.4..=2.6).step_by(0.05));
                                ui.label(format!("{:.2}", self.line_height));
                                if ui.input(|i| i.pointer.any_released()) {
                                    self.save_progress();
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label("文字色");
                                ui.add_space(20.0);
                                let mut rgb = [self.fg.r(), self.fg.g(), self.fg.b()];
                                ui.color_edit_button_srgb(&mut rgb);
                                self.fg = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                let mut hex = color_to_hex(self.fg);
                                let resp = ui.add(egui::TextEdit::singleline(&mut hex).desired_width(80.0));
                                if resp.changed() {
                                    if let Some(c) = parse_hex(&hex) {
                                        self.fg = c;
                                    }
                                }
                                self.save_progress();
                            });

                            ui.horizontal(|ui| {
                                ui.label("背景色");
                                ui.add_space(20.0);
                                let mut rgb = [self.bg.r(), self.bg.g(), self.bg.b()];
                                ui.color_edit_button_srgb(&mut rgb);
                                self.bg = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                let mut hex = color_to_hex(self.bg);
                                let resp = ui.add(egui::TextEdit::singleline(&mut hex).desired_width(80.0));
                                if resp.changed() {
                                    if let Some(c) = parse_hex(&hex) {
                                        self.bg = c;
                                    }
                                }
                                self.save_progress();
                            });

                            ui.horizontal(|ui| {
                                ui.label("字体");
                                ui.add_space(20.0);
                                let selected = self.font_name.clone();
                                let mut name = selected.clone();
                                egui::ComboBox::from_id_salt("font_cb")
                                    .selected_text(if self.font_name.is_empty() {
                                        "默认".to_string()
                                    } else {
                                        self.font_name.clone()
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut name, String::new(), "默认");
                                        for n in self.available_fonts.keys() {
                                            ui.selectable_value(&mut name, n.clone(), n.clone());
                                        }
                                    });
                                if name != selected {
                                    self.set_font(&name);
                                    self.save_progress();
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label("自动翻页");
                                ui.add_space(20.0);
                                let secs = (self.auto_ms as f64 / 1000.0) as f32;
                                let mut val = secs;
                                egui::ComboBox::from_id_salt("auto_cb")
                                    .selected_text(format!("{:.1}s", secs))
                                    .show_ui(ui, |ui| {
                                        for &s in &[0.2, 0.3, 0.5, 0.8, 1.0, 1.5, 2.0] {
                                            ui.selectable_value(&mut val, s, format!("{:.1}s", s));
                                        }
                                    });
                                self.auto_ms = (val * 1000.0) as u64;
                                ui.label(if self.auto_running { "运行中" } else { "已停止" });
                                self.save_progress();
                            });
                            ui.label(
                                egui::RichText::new("空格键 启动/停止，每次上移一行")
                                    .size(11.0)
                                    .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                            );

                            ui.horizontal(|ui| {
                                ui.label("目录");
                                ui.add_space(20.0);
                                if ui.button("打开目录").clicked() {
                                    self.toc_open = true;
                                    self.menu_open = false;
                                }
                                ui.label(format!("{} 章", self.toc.len()));
                            });

                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("桌面")
                                    .size(12.0)
                                    .color(Color32::from_rgb(0x3e, 0x6b, 0x57))
                                    .strong(),
                            );

                            ui.label(
                                egui::RichText::new("老板键 Alt+Z：全局快捷键，按一下隐藏/显示窗口")
                                    .size(11.0)
                                    .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                            );

                            ui.horizontal(|ui| {
                                ui.label("始终置顶");
                                ui.add_space(20.0);
                                ui.checkbox(&mut self.config.always_on_top, "");
                            });

                            ui.horizontal(|ui| {
                                ui.label("启动恢复");
                                ui.add_space(20.0);
                                ui.checkbox(&mut self.config.auto_open_last, "自动打开上次文件");
                            });

                            ui.horizontal(|ui| {
                                ui.label("最小窗口");
                                ui.add_space(20.0);
                                ui.add(egui::DragValue::new(&mut self.config.min_width).range(100.0..=800.0));
                                ui.label("×");
                                ui.add(egui::DragValue::new(&mut self.config.min_height).range(80.0..=600.0));
                            });
                            ui.label(
                                egui::RichText::new("允许窗口缩到极小，突破浏览器限制")
                                    .size(11.0)
                                    .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                            );

                            ui.add_space(12.0);
                            if ui
                                .add(egui::Button::new("关闭菜单").min_size(Vec2::new(panel_w - 64.0, 36.0)))
                                .clicked()
                            {
                                self.menu_open = false;
                            }
                        });
                    });
            });

        if bg_clicked {
            self.menu_open = false;
        }
        self.config.save();
    }

    fn draw_toc(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        let panel_w = 480.0_f32.min(screen.width() - 40.0);
        let panel_h = (screen.height() - 100.0).min(500.0);
        egui::Area::new(egui::Id::new("toc_overlay"))
            .fixed_pos(Pos2::new(
                (screen.width() - panel_w) / 2.0,
                (screen.height() - panel_h) / 2.0,
            ))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Color32::from_rgb(0xfd, 0xfa, 0xf1))
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_width(panel_w - 24.0);
                        ui.horizontal(|ui| {
                            ui.heading(format!("目录 {}（{} 章）", self.file_name, self.toc.len()));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("✕").clicked() {
                                        self.toc_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height(panel_h - 70.0)
                            .show(ui, |ui| {
                                let cur = self.cur_start();
                                let toc_snapshot: Vec<(usize, usize, String)> = self
                                    .toc
                                    .iter()
                                    .enumerate()
                                    .map(|(i, it)| (i, it.offset, it.title.clone()))
                                    .collect();
                                for (i, offset, title) in toc_snapshot {
                                    let is_cur = offset <= cur
                                        && (i + 1 >= self.toc.len() || self.toc[i + 1].offset > cur);
                                    let label = format!("{}. {}", i + 1, title);
                                    let resp = ui.add(
                                        egui::Button::new(
                                            egui::RichText::new(label)
                                                .color(if is_cur {
                                                    Color32::from_rgb(0xfd, 0xfa, 0xf1)
                                                } else {
                                                    self.fg
                                                })
                                                .size(13.5),
                                        )
                                        .fill(if is_cur {
                                            Color32::from_rgb(0x3e, 0x6b, 0x57)
                                        } else {
                                            Color32::TRANSPARENT
                                        })
                                        .min_size(Vec2::new(panel_w - 48.0, 28.0)),
                                    );
                                    if resp.clicked() {
                                        self.jump_to(offset);
                                        self.toc_open = false;
                                    }
                                }
                            });
                    });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.toc_open = false;
        }
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
        if let Some(msg) = self.toast.clone() {
            if self.current_time() < self.toast_until {
                let screen = ctx.screen_rect();
                let pos = Pos2::new(screen.width() / 2.0 - 100.0, screen.height() - 50.0);
                egui::Area::new(egui::Id::new("toast"))
                    .fixed_pos(pos)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(Color32::from_rgba_unmultiplied(40, 32, 20, 240))
                            .inner_margin(egui::Margin::same(10.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&msg)
                                        .size(13.0)
                                        .color(Color32::from_rgb(0xf7, 0xf2, 0xe4)),
                                );
                            });
                    });
                ctx.request_repaint();
            } else {
                self.toast = None;
            }
        }
    }
}
