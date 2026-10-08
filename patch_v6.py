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

# 1) extern GetAsyncKeyState
rep("        fn SetWindowPos(h_wnd: isize, h_wnd_insert_after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;",
    "        fn SetWindowPos(h_wnd: isize, h_wnd_insert_after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;\n        fn GetAsyncKeyState(v_key: i32) -> i16;", 'extern_key')

rep("    const SWP_NOACTIVATE: u32 = 0x0010;",
    "    const SWP_NOACTIVATE: u32 = 0x0010;\n    const VK_LBUTTON: i32 = 0x01;", 'vk')

# 2) focus_window: AttachThreadInput fallback
old = """    /// 确保窗口获得焦点并置顶（首次点击可能只聚焦窗口，导致第一次拖动被吞）
    pub fn focus_window() {
        if let Some(h) = find_window() {
            unsafe {
                SetForegroundWindow(h);
                BringWindowToTop(h);
            }
        }
    }"""
new = """    /// 确保窗口获得焦点并置顶。SetForegroundWindow 可能被前台锁拒绝，
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
        unsafe { (GetAsyncKeyState(VK_LBUTTON) & 0x8000) != 0 }
    }

    /// 窗口是否已是前台窗口（真实焦点，不依赖 egui 的 focused 状态）
    pub fn is_foreground() -> bool {
        let Some(h) = find_window() else { return false };
        unsafe { GetForegroundWindow() == h }
    }"""
rep(old, new, 'focus+helpers')

# 3) move_window_to -> move_window_to_xy (物理像素)
old = """    /// 拖动期间每帧同步移动窗口：SetWindowPos 立即落位（无 egui OuterPosition
    /// 命令队列的异步延迟），拖动更跟手、无累积漂移。
    pub fn move_window_to(pos: egui::Pos2) {
        let Some(h) = find_window() else { return };
        unsafe {
            SetWindowPos(
                h,
                0,
                pos.x.round() as i32,
                pos.y.round() as i32,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }"""
new = """    /// 拖动期间同步移动窗口：SetWindowPos 立即落位（无异步队列延迟），
    /// 参数为物理像素屏幕坐标。失败时记日志（供诊断）。
    pub fn move_window_to_xy(x: i32, y: i32) {
        let Some(h) = find_window() else { return };
        unsafe {
            let ret = SetWindowPos(h, 0, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            if ret == 0 {
                crate::log::app_log(&format!("move_window_to_xy: SetWindowPos failed ({},{})", x, y));
            }
        }
    }"""
rep(old, new, 'move_xy')

# 4) stub 同步
old = """    pub fn is_visible() -> bool {
        true
    }
    pub fn move_window_to(_pos: egui::Pos2) {}
    pub fn focus_window() {}"""
new = """    pub fn is_visible() -> bool {
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
    pub fn focus_window() {}"""
rep(old, new, 'stub')

# 5) toggle show 诊断行
old = """                PostMessageW(
                    h,
                    WM_MOUSEMOVE,
                    0,
                    ((((pt.y as i32) & 0xFFFF) << 16) | ((pt.x as i32) & 0xFFFF)) as isize,
                );
            }"""
new = """                PostMessageW(
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
            }"""
rep(old, new, 'show_diag')

# 6) 悬停聚焦：裸 Win32 前台检查（egui focused 会陈旧为 true 导致永不聚焦）
old = """        // 悬停聚焦：窗口未聚焦但光标已移入可见窗口时，自动抢焦点。
        // 根治“窗口失焦后第一次交互（拖动/缩放/右键菜单）被吞”的问题——
        // 让窗口在用户按下前已聚焦，egui 才能收到第一次鼠标事件。
        if !ctx.input(|i| i.viewport().focused.unwrap_or(false)) && win32::cursor_over_visible_window() {
            crate::log::app_log("hover-focus: cursor in window & unfocused -> focus_window()");
            win32::focus_window();
        }"""
new = """        // 悬停聚焦：用裸 Win32 判断“窗口不是前台”+“光标在窗口内”再抢焦点。
        // 不依赖 egui 的 focused（该状态会陈旧为 true 导致永不聚焦——表现为
        // ESC/右键每次都要先单击）。窗口隐藏时 cursor_over_visible_window 返回 false。
        if win32::cursor_over_visible_window() && !win32::is_foreground() {
            crate::log::app_log("hover-focus: cursor in window & not foreground -> focus_window()");
            win32::focus_window();
        }"""
rep(old, new, 'hover_raw')

# 7) 拖动：延续用裸 Win32 物理按键状态
old = """                    if drag_resp.drag_started() {
                        crate::log::app_log("drag started (manual)");
                        // 纯手动拖动：原生拖动（StartDrag / PostMessage WM_NCLBUTTONDOWN
                        // HTCAPTION）经多轮实测均被 winit 机制破坏（has_focus 门禁 /
                        // 模态循环被 winit 内部消息提前取消并抢走鼠标捕获），导致窗口不动、
                        // egui 拖动中断（表现为"单击拾取单击放下"）。不再尝试原生，
                        // 直接 SetWindowPos 每帧同步落位，跟手且无异步队列延迟。
                        if let (Some(pos), Some(outer)) = (
                            ctx.input(|i| i.pointer.interact_pos()),
                            ctx.input(|i| i.viewport().outer_rect),
                        ) {
                            self.drag_offset = pos - outer.min;
                        }
                        self.drag_active = true;
                    }
                    if self.drag_active {
                        // 以物理按键状态延续拖动（不依赖 egui 的 dragged()——
                        // 窗口移动时它可能提前结束导致"不跟随"），窗口隐藏时立即终止。
                        if ctx.input(|i| i.pointer.primary_down()) && win32::is_visible() {
                            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                                win32::move_window_to(pos - self.drag_offset);
                            }
                        } else {
                            self.drag_active = false;
                            crate::log::app_log("drag ended");
                            win32::focus_window();
                        }
                    }"""
new = """                    if drag_resp.drag_started() {
                        crate::log::app_log("drag started (manual)");
                        // 偏移量用裸 Win32 坐标（物理像素，与 SetWindowPos 同坐标系），
                        // 不依赖 egui 的 interact_pos / outer_rect（逻辑坐标，DPI 缩放时错位）。
                        if let (Some((cx, cy)), Some((wx, wy))) =
                            (win32::cursor_pos(), win32::window_pos())
                        {
                            self.drag_offset = Vec2::new(cx as f32 - wx as f32, cy as f32 - wy as f32);
                        }
                        self.drag_active = true;
                    }
                    if self.drag_active {
                        // 延续用裸 Win32 物理按键状态：egui 的 primary_down 在窗口移动时
                        // 会异常丢失（表现为 start 后同帧 ended、窗口不动），
                        // GetAsyncKeyState 反映真实按键，拖动不再中断。
                        if win32::left_button_down() && win32::is_visible() {
                            if let Some((cx, cy)) = win32::cursor_pos() {
                                win32::move_window_to_xy(
                                    (cx as f32 - self.drag_offset.x).round() as i32,
                                    (cy as f32 - self.drag_offset.y).round() as i32,
                                );
                            }
                            // 保证拖动期间每帧都执行 update（否则窗口移动后无事件驱动重绘）
                            ctx.request_repaint();
                        } else {
                            self.drag_active = false;
                            crate::log::app_log("drag ended");
                            win32::focus_window();
                        }
                    }"""
rep(old, new, 'drag_raw')

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('ALL OK:', ', '.join(log))
