use crate::config::{Config, ProgressRecord};
use crate::fonts;
use crate::log;
use crate::pagination;
use crate::toc::{ChapterMatcher, TocItem};
use eframe::egui;
use egui::{Color32, FontFamily, FontId, Pos2, Vec2};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
// Linux autohide 需从 eframe Frame 取 winit 原生窗口句柄（X11 Window id）
#[cfg(target_os = "linux")]
use raw_window_handle::HasWindowHandle;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    use global_hotkey::GlobalHotKeyEvent;
    use std::sync::atomic::{AtomicIsize, Ordering};

    const SW_HIDE: i32 = 0;
    const SW_SHOW: i32 = 5;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MSG {
        hwnd: isize,
        message: u32,
        w_param: isize,
        l_param: isize,
        time: u32,
        pt: POINT,
    }

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
        fn GetCursorPos(lp_point: *mut POINT) -> i32;
        fn GetWindowRect(h_wnd: isize, lp_rect: *mut RECT) -> i32;
        fn SetCursorPos(x: i32, y: i32) -> i32;
        fn PtInRect(lp_rect: *const RECT, pt: POINT) -> i32;
        fn ReleaseCapture() -> i32;
        fn SendMessageW(h_wnd: isize, msg: u32, w_param: isize, l_param: isize) -> isize;
        fn BringWindowToTop(h_wnd: isize) -> i32;
        fn GetForegroundWindow() -> isize;
        fn GetCurrentThreadId() -> u32;
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, f_attach: i32) -> i32;
        fn IsWindow(h_wnd: isize) -> i32;
        fn GetMessageW(lp_msg: *mut MSG, h_wnd: isize, w_msg_filter_min: u32, w_msg_filter_max: u32) -> i32;
        fn TranslateMessage(lp_msg: *const MSG) -> i32;
        fn DispatchMessageW(lp_msg: *const MSG) -> isize;
        fn PostMessageW(h_wnd: isize, msg: u32, w_param: isize, l_param: isize) -> i32;
        fn SetWindowPos(h_wnd: isize, h_wnd_insert_after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }

    const WM_NCLBUTTONDOWN: u32 = 0x00A1;
    const HTCAPTION: isize = 0x0002;
    const WM_NULL: u32 = 0x0000;
    const WM_MOUSEMOVE: u32 = 0x0200;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const VK_LBUTTON: i32 = 0x01;

    static FOUND_HWND: AtomicIsize = AtomicIsize::new(0);

    /// 枚举时的收集状态（通过 l_param 传入回调）。
    /// 本进程存在两个顶层窗口：主阅读窗口（大尺寸）与全局热键辅助窗口（(0,0) 极小）。
    /// 必须按“面积最大”选取主窗口——按 Z 序第一个可见窗口会捡到辅助窗口，
    /// 导致拖动/老板键/聚焦全部操作在辅助窗口上（表现为全部失效）。
    #[repr(C)]
    struct FindState {
        target_pid: isize,
        best_visible: isize,
        best_visible_area: i64,
        best_any: isize,
        best_any_area: i64,
    }

    unsafe extern "system" fn enum_cb(h_wnd: isize, l_param: isize) -> i32 {
        let st = &mut *(l_param as *mut FindState);
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(h_wnd, &mut pid) };
        if pid as isize == st.target_pid {
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if unsafe { GetWindowRect(h_wnd, &mut r) } != 0 {
                let w = (r.right - r.left).max(0) as i64;
                let h = (r.bottom - r.top).max(0) as i64;
                let area = w * h;
                if area > st.best_any_area {
                    st.best_any = h_wnd;
                    st.best_any_area = area;
                }
                if IsWindowVisible(h_wnd) != 0 && area > st.best_visible_area {
                    st.best_visible = h_wnd;
                    st.best_visible_area = area;
                }
            }
        }
        1 // 继续扫描：需要面积最大，不能提前停止
    }

    /// 通过进程 ID 找到本应用的主窗口句柄：取“面积最大”的窗口
    /// （主阅读窗口 ~733x821，辅助窗口极小），可见者优先并缓存。
    /// 全部隐藏时返回面积最大的窗口但不缓存（下次重新枚举）。
    /// 面积阈值 20000 排除极小辅助窗口。
    fn find_window() -> Option<isize> {
        let cached = FOUND_HWND.load(Ordering::SeqCst);
        if cached != 0 && unsafe { IsWindow(cached) } != 0 {
            return Some(cached);
        }
        let mut st = FindState {
            target_pid: std::process::id() as isize,
            best_visible: 0,
            best_visible_area: 0,
            best_any: 0,
            best_any_area: 0,
        };
        unsafe {
            EnumWindows(Some(enum_cb), &mut st as *mut FindState as isize);
        }
        let result = if st.best_visible_area >= 20_000 {
            FOUND_HWND.store(st.best_visible, Ordering::SeqCst);
            Some(st.best_visible)
        } else if st.best_any_area >= 20_000 {
            // 主窗口当前隐藏：返回但不缓存，下次重新枚举
            Some(st.best_any)
        } else {
            None
        };
        crate::log::app_log(&format!(
            "find_window: vis=({:x},{}px) any=({:x},{}px) result={}",
            st.best_visible,
            st.best_visible_area,
            st.best_any,
            st.best_any_area,
            match result {
                Some(h) => format!("Some({:x})", h),
                None => "None".to_string(),
            }
        ));
        result
    }

    /// 切换窗口可见状态：隐藏 ⇄ 显示
    pub fn toggle_window() {
        let Some(h) = find_window() else {
            crate::log::app_log("toggle_window: find_window returned None");
            return;
        };
        unsafe {
            let visible = IsWindowVisible(h) != 0;
            crate::log::app_log(&format!(
                "toggle_window: hwnd={:x} was_visible={} -> {}",
                h,
                visible,
                if visible { "hide" } else { "show" }
            ));
            if visible {
                ShowWindow(h, SW_HIDE);
            } else {
                ShowWindow(h, SW_SHOW);
                SetForegroundWindow(h);
                // 可靠唤醒 UI 线程：WM_NULL 会被 winit 静默吞掉（不产生事件，
                // eframe 不重绘、悬停聚焦不触发，表现为显示后需先单击才能用），
                // WM_MOUSEMOVE 会让 winit 发出 CursorMoved -> eframe 运行 update()
                // -> 悬停聚焦 + 重绘。用真实鼠标位置投递。
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                PostMessageW(
                    h,
                    WM_MOUSEMOVE,
                    0,
                    ((((pt.y as i32) & 0xFFFF) << 16) | ((pt.x as i32) & 0xFFFF)) as isize,
                );
                crate::log::app_log(&format!(
                    "boss show: vis={} fg={}",
                    IsWindowVisible(h),
                    if GetForegroundWindow() == h { 1 } else { 0 }
                ));
            }
        }
    }

    /// 直接隐藏窗口（老板键效果）
    pub fn hide_window() {
        let Some(h) = find_window() else { return };
        unsafe {
            ShowWindow(h, SW_HIDE);
        }
    }

    /// 判断鼠标是否位于窗口矩形内。
    /// 窗口已隐藏时一律视为“在窗口内”，避免反复触发隐藏造成抖动。
    pub fn is_cursor_inside() -> bool {
        let Some(h) = find_window() else { return true };
        unsafe {
            if IsWindowVisible(h) == 0 {
                return true;
            }
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetWindowRect(h, &mut r);
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            PtInRect(&r, p) != 0
        }
    }

    /// 将鼠标移动到窗口中心（用于程序启动时把鼠标置于界面内）
    pub fn place_cursor_inside() {
        let Some(h) = find_window() else { return };
        unsafe {
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetWindowRect(h, &mut r);
            let cx = (r.left + r.right) / 2;
            let cy = (r.top + r.bottom) / 2;
            SetCursorPos(cx, cy);
        }
    }

    /// 光标是否位于“可见”窗口内（用于悬停聚焦）。
    /// 窗口已隐藏时一律返回 false，避免对隐藏窗口抢焦点。
    pub fn cursor_over_visible_window() -> bool {
        let Some(h) = find_window() else { return false };
        unsafe {
            if IsWindowVisible(h) == 0 {
                return false;
            }
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetWindowRect(h, &mut r);
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            PtInRect(&r, p) != 0
        }
    }

    /// 主窗口当前是否可见（用于拖动延续判断：隐藏时立即终止拖拽）
    pub fn is_visible() -> bool {
        let Some(h) = find_window() else { return false };
        unsafe { IsWindowVisible(h) != 0 }
    }

    /// 专用热键消息泵：阻塞处理本线程队列消息。
    /// GlobalHotKeyManager 创建的消息窗口属于本线程，RegisterHotKey 的 WM_HOTKEY
    /// 会投递到本线程队列，必须由本线程 GetMessageW 泵出并派发到 global_hotkey_proc，
    /// 热键事件才会进入通道。主线程睡死与此泵无关。
    pub fn pump_hotkey_messages() {
        // 事件通道唯一消费者在本线程：派发（WM_HOTKEY→通道）与消费（→toggle_window）
        // 同线程完成，无跨线程依赖、无额外线程可死。窗口隐藏不影响本泵。
        let rx = GlobalHotKeyEvent::receiver().clone();
        // 200ms 防抖：Windows 按住热键时 WM_HOTKEY 会重复触发，
        // 一次按键若双发会导致"隐藏后立刻又显示"（表现为老板键失效）。
        let mut last_toggle = std::time::Instant::now() - std::time::Duration::from_millis(500);
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            loop {
                let ret = GetMessageW(&mut msg, 0, 0, 0);
                if ret > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    // ret<=0（错误或 WM_QUIT）：短暂重试，保持热键存活直到进程退出
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                while let Ok(ev) = rx.try_recv() {
                    if ev.state == global_hotkey::HotKeyState::Pressed {
                        let now = std::time::Instant::now();
                        if now.duration_since(last_toggle) < std::time::Duration::from_millis(200) {
                            crate::log::app_log("boss hotkey: debounced double-fire");
                            continue;
                        }
                        last_toggle = now;
                        crate::log::app_log("boss hotkey Alt+Z triggered");
                        toggle_window();
                    }
                }
            }
        }
    }

    /// 确保窗口获得焦点并置顶。SetForegroundWindow 可能被前台锁拒绝，
    /// 失败时附加到当前前台线程再试（经典 Workaround）。
    pub fn focus_window() {
        let Some(h) = find_window() else { return };
        unsafe {
            if SetForegroundWindow(h) == 0 {
                let fg = GetForegroundWindow();
                if fg != 0 && fg != h {
                    let cur = GetCurrentThreadId();
                    let fg_tid = GetWindowThreadProcessId(fg, std::ptr::null_mut());
                    if fg_tid != 0 && fg_tid != cur {
                        AttachThreadInput(cur, fg_tid, 1);
                        SetForegroundWindow(h);
                        AttachThreadInput(cur, fg_tid, 0);
                    }
                }
            }
            BringWindowToTop(h);
        }
    }

    /// 真实光标屏幕坐标（物理像素，与 SetWindowPos 同坐标系）
    pub fn cursor_pos() -> Option<(i32, i32)> {
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut pt) == 0 {
                None
            } else {
                Some((pt.x, pt.y))
            }
        }
    }

    /// 窗口左上角屏幕坐标（物理像素）
    pub fn window_pos() -> Option<(i32, i32)> {
        let Some(h) = find_window() else { return None };
        unsafe {
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if GetWindowRect(h, &mut r) == 0 {
                return None;
            }
            Some((r.left, r.top))
        }
    }

    /// 左键物理按下状态（GetAsyncKeyState，不依赖 egui 输入状态）
    pub fn left_button_down() -> bool {
        unsafe { GetAsyncKeyState(VK_LBUTTON) < 0 }
    }

    /// 窗口是否已是前台窗口（真实焦点，不依赖 egui 的 focused 状态）
    pub fn is_foreground() -> bool {
        let Some(h) = find_window() else { return false };
        unsafe { GetForegroundWindow() == h }
    }

    /// 拖动期间同步移动窗口：SetWindowPos 立即落位（无异步队列延迟），
    /// 参数为物理像素屏幕坐标。失败时记日志（供诊断）。
    pub fn move_window_to_xy(x: i32, y: i32) {
        let Some(h) = find_window() else { return };
        unsafe {
            let ret = SetWindowPos(h, 0, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            if ret == 0 {
                crate::log::app_log(&format!("move_window_to_xy: SetWindowPos failed ({},{})", x, y));
            }
        }
    }

    /// 检测 Windows 7（6.1）：GetVersionExW 在 Win7 上始终返回真实版本 6.1
    ///（manifest 遮蔽只影响 Win8.1+，不影响我们判断"是否 Win7"）。
    /// 用于字体策略：Win7 的 GDI 渲染下，中英文字体混排基线错位明显，
    /// 需让全部字符使用同一中文字体渲染。
    pub fn is_windows7() -> bool {
        #[repr(C)]
        struct OSVERSIONINFOEXW {
            dw_os_version_info_size: u32,
            dw_major_version: u32,
            dw_minor_version: u32,
            dw_build_number: u32,
            dw_platform_id: u32,
            sz_csd_version: [u16; 128],
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetVersionExW(lp_version_information: *mut OSVERSIONINFOEXW) -> i32;
        }
        let mut vi = OSVERSIONINFOEXW {
            dw_os_version_info_size: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
            dw_major_version: 0,
            dw_minor_version: 0,
            dw_build_number: 0,
            dw_platform_id: 0,
            sz_csd_version: [0; 128],
        };
        unsafe {
            if GetVersionExW(&mut vi) == 0 {
                return false;
            }
            vi.dw_major_version == 6 && vi.dw_minor_version == 1
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)] // Windows 专属裸 API 的 Linux 占位（拖动走 egui StartDrag、聚焦由窗口系统接管）
mod win32 {
    use std::sync::Mutex;
    use x11_dl::xlib::Xlib;

    /// 主线程每帧写入的 winit 原生窗口 id（X11 Window）。0 = 尚未拿到，此时一律视为"在窗口内"。
    static WINDOW_ID: Mutex<u64> = Mutex::new(0);
    /// X11 连接（懒初始化）。Display 是裸指针，包一层以安全放入 Mutex。
    struct SendDisplay(*mut x11_dl::xlib::Display);
    // SAFETY: Display 仅在同一进程内使用，跨线程只传递指针本身（不共享连接所有权）。
    unsafe impl Send for SendDisplay {}
    static XLIB: Mutex<Option<Xlib>> = Mutex::new(None);
    static DISPLAY: Mutex<Option<SendDisplay>> = Mutex::new(None);

    /// 主线程每帧把 winit 原生窗口句柄写入，供 is_cursor_inside 做屏幕级光标判断。
    pub fn set_window_id(id: u64) {
        *WINDOW_ID.lock().unwrap() = id;
    }

    /// 判断鼠标是否位于窗口矩形内（屏幕坐标，含 6px 边距容差）。
    /// 任何不可用（无窗口 id / 无 X 连接 / 查询失败）一律返回 true（保守：不触发隐藏）。
    /// 这是 v1.3.1 在 Linux 上仍是"恒 true"的空 stub；此处用 XQueryPointer 真实现，
    /// 使"鼠标离开窗口自动隐藏(autohide)"在 Linux/X11 上真正生效。
    pub fn is_cursor_inside() -> bool {
        let wid = *WINDOW_ID.lock().unwrap();
        if wid == 0 {
            return true;
        }
        let mut xlib_guard = XLIB.lock().unwrap();
        if xlib_guard.is_none() {
            *xlib_guard = Xlib::open().ok();
        }
        let Some(xlib) = xlib_guard.as_ref() else {
            return true;
        };
        let mut disp_guard = DISPLAY.lock().unwrap();
        if disp_guard.is_none() {
            let d = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
            if d.is_null() {
                return true;
            }
            *disp_guard = Some(SendDisplay(d));
        }
        let d = disp_guard.as_ref().unwrap().0;
        let win = wid as x11_dl::xlib::Window;

        unsafe {
            let mut root: x11_dl::xlib::Window = 0;
            let mut child: x11_dl::xlib::Window = 0;
            let mut rx: i32 = 0;
            let mut ry: i32 = 0;
            let mut _wx: i32 = 0;
            let mut _wy: i32 = 0;
            let mut mask: u32 = 0;
            // XQueryPointer 失败（例如指针不在本 X 屏幕）→ 保守返回 true
            if (xlib.XQueryPointer)(
                d, win, &mut root, &mut child, &mut rx, &mut ry, &mut _wx, &mut _wy, &mut mask,
            ) == 0
            {
                return true;
            }
            // 窗口左上角屏幕坐标（相对 root）：XTranslateCoordinates(win→root, 0,0)
            let root_win = (xlib.XDefaultRootWindow)(d);
            let mut ax: i32 = 0;
            let mut ay: i32 = 0;
            let mut _child2: x11_dl::xlib::Window = 0;
            if (xlib.XTranslateCoordinates)(d, win, root_win, 0, 0, &mut ax, &mut ay, &mut _child2) == 0
            {
                return true;
            }
            let mut attr: x11_dl::xlib::XWindowAttributes = std::mem::zeroed();
            (xlib.XGetWindowAttributes)(d, win, &mut attr);
            let pad = 6i32;
            let w = attr.width as i32;
            let h = attr.height as i32;
            rx >= ax - pad && rx <= ax + w + pad && ry >= ay - pad && ry <= ay + h + pad
        }
    }

    pub fn toggle_window() {}
    pub fn hide_window() {}
    pub fn place_cursor_inside() {}
    pub fn start_window_drag() {}
    pub fn focus_window() {}
    pub fn is_windows7() -> bool {
        false
    }
    pub fn cursor_over_visible_window() -> bool {
        false
    }
    pub fn is_visible() -> bool {
        true
    }
    pub fn cursor_pos() -> Option<(i32, i32)> {
        None
    }
    pub fn window_pos() -> Option<(i32, i32)> {
        None
    }
    pub fn left_button_down() -> bool {
        false
    }
    pub fn is_foreground() -> bool {
        true
    }
    pub fn move_window_to_xy(_x: i32, _y: i32) {}
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
    /// 老板模式开关：与 UI 线程共享，由专用检测线程读取
    auto_hide_enabled: Arc<AtomicBool>,
    /// 启动时是否已把鼠标置入窗口
    cursor_placed: bool,
    /// 是否已做首帧初始化（置入鼠标/位置安全/日志）
    first_frame_done: bool,
    /// 老板键：当前窗口是否可见（初始 true）
    boss_visible: bool,
    /// 老板键：隐藏前是否在自动翻页，唤回时恢复
    auto_was_running: bool,
    /// 老板键：后台热键线程置位，主线程在 update() 消费后执行隐藏/唤回
    boss_evt: Arc<AtomicBool>,
    /// 自动隐藏(autohide)：后台采样线程检测到鼠标移出窗口后置位，主线程消费后隐藏
    autohide_evt: Arc<AtomicBool>,
    /// 手动拖动窗口状态：是否正在拖动（Windows 自绘拖动专用；Linux 走 StartDrag）
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    drag_active: bool,
    /// 手动拖动窗口状态：按下时鼠标相对窗口左上角的偏移（Windows 专用）
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    drag_offset: Vec2,
}

impl ReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        log::app_log("ReaderApp::new start");
        let available_fonts = fonts::available_chinese_fonts();
        {
            let mut fonts = egui::FontDefinitions::default();
            // Win7 的 GDI 渲染下中英混排基线错位明显（菜单数字/字母/文字不在一条线），
            // 检测到 Win7 时把中文字体作为 UI 默认族的第一字体，全部字符统一渲染。
            let unify_baseline = win32::is_windows7();
            if unify_baseline {
                log::app_log("Windows 7 detected: unify UI font baseline");
            }
            fonts::install_fonts(&mut fonts, &available_fonts, unify_baseline);
            cc.egui_ctx.set_fonts(fonts);
        }

        // 老板键/自动隐藏事件标志：后台线程置位，主线程 update() 消费后执行隐藏/唤回。
        // 不在后台线程里直接操作窗口：隐藏/唤回需同时暂停/恢复自动翻页（UI 状态），
        // 而 UI 状态只在主线程帧内安全变更；egui Context 可跨线程 request_repaint 唤醒帧循环。
        let boss_evt = Arc::new(AtomicBool::new(false));
        let autohide_evt = Arc::new(AtomicBool::new(false));

        // 老板键热键注册：GlobalHotKeyManager 必须创建在“运行事件循环的线程”上，
        // WM_HOTKEY 由该线程的消息泵分发到 global_hotkey_proc。若建在主线程，
        // 主窗口隐藏后 eframe/winit 事件循环睡死、不再泵消息，Alt+Z 将不再送达
        // （实测表现：老板键隐藏后无法再次唤回）。
        // 因此改在专用线程创建管理器 + 注册热键 + 独立 GetMessageW 消息泵，与主线程解耦。
        #[cfg(target_os = "windows")]
        {
            std::thread::Builder::new()
                .name("hotkey-pump".to_string())
                .spawn(move || {
                    let Ok(mgr) = GlobalHotKeyManager::new() else {
                        crate::log::app_log("hotkey: GlobalHotKeyManager::new failed");
                        return;
                    };
                    let Some(hk) = parse_hotkey(BOSS_KEY) else {
                        crate::log::app_log("hotkey: parse BOSS_KEY failed");
                        return;
                    };
                    if let Err(e) = mgr.register(hk) {
                        crate::log::app_log(&format!("hotkey: register failed {:?}", e));
                        return;
                    }
                    crate::log::app_log("hotkey pump thread running (Alt+Z)");
                    win32::pump_hotkey_messages();
                })
                .ok();
        }
        // Linux（X11/Wayland）：GlobalHotKeyManager 在主线程创建并注册（X 服务器接管），
        // 这里只起一个 receiver 线程把热键事件转成 boss_evt 标志，真正隐藏/唤回
        // 由主线程 update() 消费后执行（egui ViewportCommand::Visible）。
        #[cfg(not(target_os = "windows"))]
        {
            let hotkey_manager = GlobalHotKeyManager::new().ok();
            let current_hotkey = parse_hotkey(BOSS_KEY);
            if let (Some(mgr), Some(hk)) = (&hotkey_manager, &current_hotkey) {
                let _ = mgr.register(*hk);
            }
            let _ = hotkey_manager; // 保持注册有效（Drop 注销热键）
            let boss_evt = boss_evt.clone();
            let egui_ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("hotkey-evt".to_string())
                .spawn(move || {
                    let rx = GlobalHotKeyEvent::receiver().clone();
                    while let Ok(ev) = rx.recv() {
                        if ev.state == global_hotkey::HotKeyState::Pressed {
                            boss_evt.store(true, Ordering::SeqCst);
                            egui_ctx.request_repaint();
                        }
                    }
                })
                .ok();
        }

        // 老板模式（鼠标离开窗口自动隐藏）：
        // 与老板键同理，隐藏窗口后 eframe 事件循环会睡死、update() 不执行，
        // 因此不能靠帧循环轮询，改用专用线程每 150ms 采样一次鼠标位置。
        // 连续 2 次检测到鼠标移出窗口才隐藏，避免瞬态抖动误触发。
        let auto_hide_enabled = Arc::new(AtomicBool::new(config.auto_hide_on_mouse_leave));
        {
            let enabled = auto_hide_enabled.clone();
            let autohide_evt = autohide_evt.clone();
            let egui_ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let mut consecutive_out = 0u32;
                loop {
                    std::thread::sleep(Duration::from_millis(150));
                    if !enabled.load(Ordering::SeqCst) {
                        consecutive_out = 0;
                        continue;
                    }
                    if win32::is_cursor_inside() {
                        consecutive_out = 0;
                    } else {
                        consecutive_out += 1;
                        if consecutive_out >= 2 {
                            // Windows：线程内直接调 Win32 隐藏（隐藏后事件循环睡死，
                            // 无法等 update() 消费；且隐藏不涉及 UI 状态暂停）。
                            #[cfg(target_os = "windows")]
                            win32::hide_window();
                            // Linux：置标志并唤醒主线程，由 update() 统一执行隐藏
                            // （隐藏需同时暂停自动翻页等 UI 状态，只在主线程安全变更）。
                            #[cfg(not(target_os = "windows"))]
                            {
                                autohide_evt.store(true, Ordering::SeqCst);
                                egui_ctx.request_repaint();
                            }
                        }
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
            _hotkey_manager: None,
            _current_hotkey: None,
            pending_open: None,
            last_bounds_save: Instant::now(),
            auto_hide_enabled,
            cursor_placed: false,
            first_frame_done: false,
            boss_visible: true,
            auto_was_running: false,
            boss_evt,
            autohide_evt,
            drag_active: false,
            drag_offset: Vec2::ZERO,
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

        log::app_log("ReaderApp::new done");
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
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(p) = self.pending_open.take() {
            self.load_file(&p);
        }

        // 记录原生窗口句柄（X11 Window id），供 Linux autohide 做屏幕级光标检测
        #[cfg(target_os = "linux")]
        if let Ok(wh) = frame.window_handle() {
            if let raw_window_handle::RawWindowHandle::Xlib(xh) = wh.as_raw() {
                win32::set_window_id(xh.window);
            }
        }

        // 首帧初始化：置入鼠标 + 窗口位置安全 + 记录日志
        if !self.first_frame_done {
            self.first_frame_done = true;
            // 强制让窗口获得焦点，避免首次拖动/缩放被“先聚焦窗口”吞掉
            win32::focus_window();
            log::app_log(&format!(
                "first frame: screen={:?} outer={:?} monitor={:?}",
                ctx.screen_rect(),
                ctx.input(|i| i.viewport().outer_rect),
                ctx.input(|i| i.viewport().monitor_size),
            ));
            if self.config.auto_hide_on_mouse_leave {
                win32::place_cursor_inside();
            }
            // 老板模式下先把鼠标置入窗口（配合“鼠标离开自动隐藏”）
            self.cursor_placed = true;
            // 无边框窗口若恢复到屏幕外的旧位置，会表现为“双击没反应”，这里拉回可见区域
            let (outer, monitor) =
                ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size));
            if let (Some(o), Some(ms)) = (outer, monitor) {
                if ms.x > 100.0 && ms.y > 100.0 {
                    let mon = egui::Rect::from_min_size(Pos2::ZERO, ms);
                    if !o.intersects(mon) {
                        log::app_log("window off-screen -> move to (30,30)");
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::Pos2::new(30.0, 30.0)));
                    }
                }
            }
        }

        // 悬停聚焦：用裸 Win32 判断“窗口不是前台”+“光标在窗口内”再抢焦点。
        // 不依赖 egui 的 focused（该状态会陈旧为 true 导致永不聚焦——表现为
        // ESC/右键每次都要先单击）。窗口隐藏时 cursor_over_visible_window 返回 false。
        // （Linux 分支的 cursor_over_visible_window 恒 false，不触发抢焦点。）
        if win32::cursor_over_visible_window() && !win32::is_foreground() {
            crate::log::app_log("hover-focus: cursor in window & not foreground -> focus_window()");
            win32::focus_window();
        }

        // 老板键：消费后台热键线程置位的标志，统一在主线程执行隐藏/唤回。
        // （Windows 走专用消息泵直接操作窗口，标志恒 false，此段无副作用。）
        if self.boss_evt.swap(false, Ordering::SeqCst) {
            self.toggle_boss_key(ctx);
        }
        // 自动隐藏(autohide)：后台采样线程检测到鼠标移出窗口后置位，这里在主线程执行隐藏
        if self.autohide_evt.swap(false, Ordering::SeqCst) {
            self.hide_window(ctx);
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

        // ESC：菜单/目录打开时先关闭，否则退出程序
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc {
            crate::log::app_log(&format!(
                "esc key event: menu={} toc={} fg={}",
                self.menu_open,
                self.toc_open,
                win32::is_foreground()
            ));
        }
        if esc {
            if self.menu_open {
                self.menu_open = false;
            } else if self.toc_open {
                self.toc_open = false;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // 标题栏
        self.draw_titlebar(ctx);

        // 主区域：不再有独立开屏页，未打开文件时直接显示阅读界面（内含使用说明）
        if self.text.is_empty() {
            self.draw_empty_guide(ctx);
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
    /// 隐藏窗口（老板键 / autohide 共用）：暂停自动翻页并记录状态，唤回时恢复。
    /// 仅应在主线程 update() 内调用（各事件标志已在此消费）。
    fn hide_window(&mut self, ctx: &egui::Context) {
        if !self.boss_visible {
            return; // 已隐藏，避免重复
        }
        self.boss_visible = false;
        self.auto_was_running = self.auto_running;
        if self.auto_running {
            self.auto_running = false;
            self.auto_last_tick = None;
            self.save_progress();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        log::app_log("window hidden (boss/autohide)");
        ctx.request_repaint();
    }

    /// 显示窗口（老板键唤回）：恢复隐藏前的自动翻页状态。
    fn show_window(&mut self, ctx: &egui::Context) {
        if self.boss_visible {
            return;
        }
        self.boss_visible = true;
        if self.auto_was_running {
            self.auto_running = true;
            self.auto_last_tick = Some(Instant::now());
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        log::app_log("window shown");
        ctx.request_repaint();
    }

    /// 老板键：切换窗口可见性。
    fn toggle_boss_key(&mut self, ctx: &egui::Context) {
        if self.boss_visible {
            self.hide_window(ctx);
        } else {
            self.show_window(ctx);
        }
    }

    fn reader_page_height(&self, ctx: &egui::Context) -> f32 {
        let screen = ctx.screen_rect();
        (screen.height() - self.titlebar_h() - 60.0).max(40.0)
    }

    /// 当前标题栏高度（隐藏时为 0）
    fn titlebar_h(&self) -> f32 {
        if self.config.show_titlebar {
            TITLEBAR_H
        } else {
            0.0
        }
    }

    /// 当前正在阅读的章节名（取目录中最后一个偏移不超过当前阅读位置的章节）
    fn current_chapter(&self) -> Option<&str> {
        let cur = self.cur_start();
        self.toc
            .iter()
            .rev()
            .find(|t| t.offset <= cur)
            .map(|t| t.title.as_str())
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
        if !self.config.show_titlebar {
            return;
        }
        // 标题栏主文案：优先显示当前章节名，未识别到章节时回退为文件名
        let title: String = self
            .current_chapter()
            .map(|c| c.to_string())
            .unwrap_or_else(|| {
                if self.file_name.is_empty() {
                    "未打开文件".to_string()
                } else {
                    self.file_name.clone()
                }
            });
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
                        ui.label(
                            egui::RichText::new(&title)
                                .size(13.0)
                                .strong()
                                .color(Color32::from_rgb(0x3e, 0x6b, 0x57)),
                        );

                        ui.add_space(12.0);
                        // 原老板键按钮位置：显示当前文件名（超长截断，避免挤压右侧进度）
                        if !self.file_name.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&self.file_name)
                                        .size(12.0)
                                        .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                                )
                                .truncate(),
                            );
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

    /// 未打开文件时的"阅读界面"：直接进入阅读区域（无独立开屏页），
    /// 显示简短使用说明。窗口/字号等布局与阅读时完全一致。
    fn draw_empty_guide(&mut self, ctx: &egui::Context) {
        let max_width = self.reader_max_width(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.bg))
            .show(ctx, |ui| {
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space((max_width - 220.0) / 2.0);
                    ui.label(
                        egui::RichText::new("本地 TXT 阅读器")
                            .size(24.0)
                            .color(Color32::from_rgb(0x33, 0x2d, 0x24)),
                    );
                });
                ui.add_space(12.0);
                let items: &[(&str, &str)] = &[
                    ("打开文件", "点击中间 1/3 弹出设置 → “打开 TXT 文件”，或直接按 Ctrl+O；也可将 TXT 拖入窗口"),
                    ("翻页", "点击阅读区左 1/3 上一页 / 右 1/3 下一页；键盘 ← / →；鼠标滚轮向下下一页"),
                    ("设置", "点击阅读区中间 1/3 弹出菜单：字号、字体、文字/背景色、自动翻页、跳转目录"),
                    ("自动翻页", "空格键启动 / 停止；间隔在设置菜单中调节（0.5s – 3s）"),
                    ("老板键", "Alt+Z 一键隐藏 / 唤回窗口；鼠标离开窗口可自动隐藏（设置菜单开关）"),
                    ("进度", "阅读位置按文件自动保存在本机，下次打开自动续读"),
                ];
                for (k, v) in items {
                    ui.horizontal(|ui| {
                        ui.add_space(30.0);
                        ui.label(
                            egui::RichText::new(*k)
                                .size(15.0)
                                .strong()
                                .color(Color32::from_rgb(0x3e, 0x6b, 0x57)),
                        );
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(*v)
                                .size(14.0)
                                .color(Color32::from_rgb(0x55, 0x4c, 0x3d)),
                        );
                    });
                    ui.add_space(10.0);
                }
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
                ui.add_space(self.titlebar_h());
                let avail = ui.available_rect_before_wrap();
                let left = avail.left() + 30.0;
                let top = avail.top() + 24.0;
                let content_w = (avail.width() - 60.0).max(100.0);
                let content_h = (avail.height() - 48.0).max(40.0);
                let tb_h = self.titlebar_h();

                // 左键点击：左/右 1/3 翻页；中间 1/3 为窗口拖动区，不做翻页
                let click = if !self.menu_open && !self.toc_open {
                    ctx.input(|i| {
                        if i.pointer.primary_clicked() {
                            i.pointer.interact_pos()
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                if let Some(pos) = click {
                    if pos.y > tb_h && pos.y < avail.bottom() {
                        let rel_x = pos.x - left;
                        if rel_x >= 0.0 && rel_x <= content_w {
                            if rel_x < content_w / 3.0 {
                                self.prev_page(ctx, max_width, page_height);
                            } else if rel_x >= content_w * 2.0 / 3.0 {
                                self.next_page(ctx, max_width, page_height);
                            }
                            // 中间 1/3：留给左键拖动窗口
                        }
                    }
                }

                // 右键点击阅读区任意位置：开关设置菜单
                let rclick = if !self.menu_open && !self.toc_open {
                    ctx.input(|i| {
                        if i.pointer.secondary_clicked() {
                            i.pointer.interact_pos()
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                if let Some(pos) = rclick {
                    if pos.y > tb_h && pos.y < avail.bottom() {
                        self.menu_open = !self.menu_open;
                    }
                }

                // 中间 1/3 左键拖动：无边框模式下拖动整个窗口
                if !self.menu_open && !self.toc_open {
                    let drag_rect = egui::Rect::from_min_size(
                        Pos2::new(left + content_w / 3.0, top),
                        Vec2::new(content_w / 3.0, content_h),
                    );
                    let drag_resp =
                        ui.interact(drag_rect, ui.id().with("win_drag"), egui::Sense::drag());
                    if drag_resp.drag_started() {
                        // Windows：偏移量用裸 Win32 坐标（物理像素，与 SetWindowPos 同坐标系），
                        // 不依赖 egui 的 interact_pos / outer_rect（逻辑坐标，DPI 缩放时错位）。
                        #[cfg(target_os = "windows")]
                        {
                            let (cp, wp) = (win32::cursor_pos(), win32::window_pos());
                            if let (Some((cx, cy)), Some((wx, wy))) = (cp, wp) {
                                self.drag_offset = Vec2::new(cx as f32 - wx as f32, cy as f32 - wy as f32);
                            }
                            self.drag_active = true;
                            crate::log::app_log(&format!(
                                "drag start: cur={:?} win={:?} off=({:.0},{:.0})",
                                cp, wp, self.drag_offset.x, self.drag_offset.y
                            ));
                        }
                        // Linux（X11/Wayland）：egui-winit 0.29 已将
                        // ViewportCommand::StartDrag 映射到 winit drag_window()，
                        // 原生接管窗口移动（无边框自绘标题栏的标准做法）。
                        #[cfg(not(target_os = "windows"))]
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    // Windows 拖动延续：用裸 Win32 物理按键状态逐帧手动跟随。
                    // （Linux 走 StartDrag 后由窗口系统接管，无此段。）
                    #[cfg(target_os = "windows")]
                    if self.drag_active {
                        // 延续用裸 Win32 物理按键状态（GetAsyncKeyState 反映真实按键）。
                        let down = win32::left_button_down();
                        let vis = win32::is_visible();
                        let cur = win32::cursor_pos();
                        if down && vis {
                            if let Some((cx, cy)) = cur {
                                win32::move_window_to_xy(
                                    (cx as f32 - self.drag_offset.x).round() as i32,
                                    (cy as f32 - self.drag_offset.y).round() as i32,
                                );
                            }
                            // 保证拖动期间每帧都执行 update（否则窗口移动后无事件驱动重绘）
                            ctx.request_repaint();
                        } else {
                            self.drag_active = false;
                            crate::log::app_log(&format!(
                                "drag end: down={} vis={} cur={:?}",
                                down, vis, cur
                            ));
                            win32::focus_window();
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

        // 无边框模式下右下角缩放柄：拖动可调整窗口大小
        self.draw_resize_grip(ctx);
    }

    /// 右下角缩放柄（无边框窗口调整大小用）
    fn draw_resize_grip(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        let size = 20.0;
        let grip_pos = Pos2::new(screen.right() - size, screen.bottom() - size);
        egui::Area::new(egui::Id::new("resize_grip"))
            .fixed_pos(grip_pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let resp = ui.allocate_response(Vec2::new(size, size), egui::Sense::drag());
                let rect = resp.rect;
                // 右下角小三角，提示可拖动
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(rect.right() - 14.0, rect.bottom()),
                        Pos2::new(rect.right(), rect.bottom()),
                        Pos2::new(rect.right(), rect.bottom() - 14.0),
                    ],
                    Color32::from_rgba_unmultiplied(0x3e, 0x6b, 0x57, 120),
                    egui::Stroke::NONE,
                ));
                // 用 egui 原生 BeginResize（→ winit 正规缩放路径），避免裸 SendMessage 模态循环导致的卡死
                if resp.drag_started() {
                    win32::focus_window();
                    ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(
                        egui::viewport::ResizeDirection::SouthEast,
                    ));
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
                                ui.label("翻页速度");
                                ui.add_space(20.0);
                                let mut secs = self.auto_ms as f32 / 1000.0;
                                ui.add(egui::Slider::new(&mut secs, 0.2..=2.0).step_by(0.1));
                                self.auto_ms = (secs * 1000.0).round() as u64;
                                ui.label(format!("{:.1}s", secs));
                                ui.label(if self.auto_running { "运行中" } else { "已停止" });
                                if ui.input(|i| i.pointer.any_released()) {
                                    self.save_progress();
                                }
                            });
                            ui.label(
                                egui::RichText::new("空格键 启动/停止，每次上移一行")
                                    .size(11.0)
                                    .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                            );

                            ui.horizontal(|ui| {
                                ui.label("文件");
                                ui.add_space(20.0);
                                if ui.button("打开 TXT 文件").clicked() {
                                    self.open_file_dialog();
                                }
                            });

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
                                ui.label("老板模式");
                                ui.add_space(20.0);
                                let mut chk = self.config.auto_hide_on_mouse_leave;
                                if ui
                                    .checkbox(&mut chk, "鼠标离开窗口自动隐藏")
                                    .changed()
                                {
                                    self.config.auto_hide_on_mouse_leave = chk;
                                    self.auto_hide_enabled.store(chk, Ordering::SeqCst);
                                }
                            });
                            ui.label(
                                egui::RichText::new(
                                    "开启后鼠标移出阅读界面将触发老板键隐藏；再次按 Alt+Z 唤回",
                                )
                                .size(11.0)
                                .color(Color32::from_rgb(0x7a, 0x6f, 0x5a)),
                            );

                            ui.horizontal(|ui| {
                                ui.label("标题栏");
                                ui.add_space(20.0);
                                ui.checkbox(&mut self.config.show_titlebar, "显示顶部标题栏");
                            });
                            ui.label(
                                egui::RichText::new("隐藏后窗口更简洁，仍可右键打开本菜单、中间拖动窗口")
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
