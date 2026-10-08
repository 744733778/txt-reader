import io
p = r'C:\Users\Administrator\Desktop\test\txt-reader\src\app.rs'
s = io.open(p, encoding='utf-8').read()
log = []
def rep(old, new, tag):
    global s
    n = s.count(old)
    if n != 1:
        raise SystemExit('%s count=%d' % (tag, n))
    s = s.replace(old, new, 1)
    log.append(tag)

rep("use std::sync::{Arc, Mutex};", "use std::sync::Arc;", 'import')

old = """// ---------------------------------------------------------------------------
// UI 线程共享句柄：老板键线程隐藏/显示窗口后，同步 egui 视图状态并唤醒帧循环，
// 防止 egui-winit 内部可见性状态与原生窗口不一致（表现为隐藏后又被重新显示）。
// ---------------------------------------------------------------------------
static UI_CTX: Mutex<Option<egui::Context>> = Mutex::new(None);
/// 老板键显示窗口后，在 UI 线程强制聚焦一次（防 egui focused 状态陈旧导致需先单击）
static JUST_SHOWN: AtomicBool = AtomicBool::new(false);

/// 老板键隐藏窗口后调用：同步 egui 视图为隐藏并唤醒一帧
pub fn request_ui_hide() {
    if let Some(ctx) = UI_CTX.lock().unwrap().clone() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint();
    }
}

/// 老板键显示窗口后调用：同步 egui 视图为显示、唤醒一帧并标记需强制聚焦
pub fn request_ui_show() {
    if let Some(ctx) = UI_CTX.lock().unwrap().clone() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.request_repaint();
    }
    JUST_SHOWN.store(true, Ordering::SeqCst);
}

"""
new = ""
rep(old, new, 'remove_ui_ctx')

old = """    const WM_NCLBUTTONDOWN: u32 = 0x00A1;
    const HTCAPTION: isize = 0x0002;"""
new = """    const WM_NCLBUTTONDOWN: u32 = 0x00A1;
    const HTCAPTION: isize = 0x0002;
    const WM_NULL: u32 = 0x0000;"""
rep(old, new, 'wm_null')

old = """        fn TranslateMessage(lp_msg: *const MSG) -> i32;
        fn DispatchMessageW(lp_msg: *const MSG) -> isize;"""
new = """        fn TranslateMessage(lp_msg: *const MSG) -> i32;
        fn DispatchMessageW(lp_msg: *const MSG) -> isize;
        fn PostMessageW(h_wnd: isize, msg: u32, w_param: isize, l_param: isize) -> i32;"""
rep(old, new, 'postmessage')

old = """mod win32 {
    use std::sync::atomic::{AtomicIsize, Ordering};"""
new = """mod win32 {
    use global_hotkey::GlobalHotKeyEvent;
    use std::sync::atomic::{AtomicIsize, Ordering};"""
rep(old, new, 'win32_import')

old = """            if visible {
                ShowWindow(h, SW_HIDE);
                // 同步 egui-winit 可见性状态，防止其内部认为窗口仍可见而重新显示
                super::request_ui_hide();
            } else {
                ShowWindow(h, SW_SHOW);
                SetForegroundWindow(h);
                super::request_ui_show();
            }"""
new = """            if visible {
                ShowWindow(h, SW_HIDE);
            } else {
                ShowWindow(h, SW_SHOW);
                SetForegroundWindow(h);
                // 唤醒 UI 线程消息泵（WM_NULL 会唤醒 winit 的 GetMessage），
                // 使其恢复渲染与悬停聚焦，ESC/右键等立即可用
                PostMessageW(h, WM_NULL, 0, 0);
            }"""
rep(old, new, 'toggle_pure')

old = """    pub fn pump_hotkey_messages() {
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
            }
        }
    }"""
new = """    pub fn pump_hotkey_messages() {
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
    }"""
rep(old, new, 'pump_merged')

old = """    /// 无边框模式拖动窗口：先聚焦，再向系统发送 WM_NCLBUTTONDOWN+HTCAPTION，
    /// 让操作系统原生接管窗口移动（自定义标题栏的标准做法，可靠且无需手动算位置）。
    pub fn start_window_drag() {
        focus_window();
        let Some(h) = find_window() else {
            crate::log::app_log("start_window_drag: find_window returned None");
            return;
        };
        crate::log::app_log(&format!("start_window_drag: hwnd={:x}", h));
        unsafe {
            ReleaseCapture();
            SendMessageW(h, WM_NCLBUTTONDOWN, HTCAPTION, 0);
        }
        // 原生拖动结束后，ReleaseCapture 可能使窗口丢失焦点，
        // 这里重新聚焦，避免下一次拖动前还要先单击一次窗口。
        focus_window();
    }"""
new = """    /// 无边框窗口原生拖动：ReleaseCapture + PostMessageW(WM_NCLBUTTONDOWN, HTCAPTION)。
    /// 与 winit drag_window 同款，但绕过 egui-winit 的 has_focus() 门禁直接发原生消息。
    /// PostMessage 异步投递，DefWindowProc 处理时以真实按键状态进入系统移动模态循环，
    /// 窗口随鼠标平滑移动（无闪烁虚影）；与 SendMessage 的同步路径不同。
    pub fn begin_native_drag() {
        focus_window();
        let Some(h) = find_window() else {
            crate::log::app_log("begin_native_drag: find_window returned None");
            return;
        };
        crate::log::app_log(&format!("begin_native_drag: hwnd={:x}", h));
        unsafe {
            ReleaseCapture();
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            let lparam = (((pt.y as i32) & 0xFFFF) << 16) | ((pt.x as i32) & 0xFFFF);
            PostMessageW(h, WM_NCLBUTTONDOWN, HTCAPTION, lparam as isize);
        }
        // 原生拖动结束后，ReleaseCapture 可能使窗口丢失焦点，
        // 这里重新聚焦，避免下一次拖动前还要先单击一次窗口。
        focus_window();
    }"""
rep(old, new, 'begin_native_drag')

old = """        // 老板键热键注册：GlobalHotKeyManager 必须创建在“运行事件循环的线程”上，
        // WM_HOTKEY 由该线程的消息泵分发到 global_hotkey_proc。若建在主线程，
        // 主窗口隐藏后 eframe/winit 事件循环睡死、不再泵消息，Alt+Z 将不再送达
        // （实测表现：老板键隐藏后无法再次唤回）。
        // 因此改在专用线程创建管理器 + 注册热键 + 独立 GetMessageW 消息泵，与主线程解耦。
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

        // 老板键事件：由专用线程作为通道唯一消费者处理，直接调用 Win32 操作窗口。
        // 原因：eframe 只在 RedrawRequested 时运行帧，隐藏窗口收不到 WM_PAINT，
        // 事件循环会睡死，update() 不执行，通道事件若只靠 update() 轮询将永远无法处理
        // （表现：老板键隐藏窗口后无法唤回）。
        {
            let rx = GlobalHotKeyEvent::receiver().clone();
            std::thread::spawn(move || {
                // 200ms 防抖：Windows 按住热键时 WM_HOTKEY 会重复触发，
                // 一次按键若双发会导致"隐藏后立刻又显示"（表现为老板键失效）。
                let mut last_toggle = std::time::Instant::now() - std::time::Duration::from_millis(500);
                while let Ok(ev) = rx.recv() {
                    if ev.state == global_hotkey::HotKeyState::Pressed {
                        let now = std::time::Instant::now();
                        if now.duration_since(last_toggle) < std::time::Duration::from_millis(200) {
                            crate::log::app_log("boss hotkey: debounced double-fire");
                            continue;
                        }
                        last_toggle = now;
                        crate::log::app_log("boss hotkey Alt+Z triggered");
                        win32::toggle_window();
                    }
                }
            });
        }"""
new = """        // 老板键：GlobalHotKeyManager 必须创建在“运行事件循环的线程”上，WM_HOTKEY 由该
        // 线程的消息泵分发。主窗口隐藏后主线程事件循环睡死，因此热键泵与主线程完全解耦。
        // 泵线程同时作为事件通道唯一消费者（消费→防抖→Win32 隐藏/显示），无跨线程依赖。
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
        }"""
rep(old, new, 'new_pump_only')

old = """            // 共享 egui Context 给老板键线程（同步可见性/唤醒帧循环）
            *UI_CTX.lock().unwrap() = Some(ctx.clone());
            // 强制让窗口获得焦点，避免首次拖动/缩放被“先聚焦窗口”吞掉
            win32::focus_window();"""
new = """            // 强制让窗口获得焦点，避免首次拖动/缩放被“先聚焦窗口”吞掉
            win32::focus_window();"""
rep(old, new, 'rm_first_ctx')

old = """        // 老板键显示窗口后的首帧：强制聚焦（SetForegroundWindow 可能被前台锁吞掉）
        if JUST_SHOWN.swap(false, Ordering::SeqCst) {
            crate::log::app_log("just-shown -> force focus");
            win32::focus_window();
        }"""
new = ""
rep(old, new, 'rm_just_shown')

old = """                    if drag_resp.drag_started() {
                        crate::log::app_log("drag started (native StartDrag)");
                        // 优先原生拖动（winit drag_window：ReleaseCapture+PostMessage WM_NCLBUTTONDOWN
                        // HTCAPTION，OS 接管移动，平滑无闪烁）。之前失败是因为窗口无焦点导致
                        // egui-winit 的 has_focus() 检查静默跳过；现在悬停聚焦已就绪。
                        // 若 150ms 后窗口未随指针移动（原生未生效），自动回退手动 OuterPosition 跟随。
                        self.drag_active = true;
                        self.drag_native_attempt = true;
                        self.drag_start_outer = ctx.input(|i| i.viewport().outer_rect);
                        self.drag_last_check = Instant::now();
                        win32::focus_window();
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }"""
new = """                    if drag_resp.drag_started() {
                        crate::log::app_log("drag started (native drag)");
                        // 优先原生拖动：直接 PostMessageW(WM_NCLBUTTONDOWN, HTCAPTION)，
                        // 绕过 egui-winit 的 has_focus() 门禁（该门禁是之前 StartDrag
                        // 静默失效的根因）。若 150ms 后窗口未随指针移动（原生未生效），
                        // 自动回退手动 OuterPosition 跟随。
                        self.drag_active = true;
                        self.drag_native_attempt = true;
                        self.drag_start_outer = ctx.input(|i| i.viewport().outer_rect);
                        self.drag_last_check = Instant::now();
                        win32::begin_native_drag();
                    }"""
rep(old, new, 'drag_native')

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('ALL OK:', ', '.join(log))
