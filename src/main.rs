// Windows 下不弹出控制台窗口；其他平台忽略此属性
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod config;
mod fonts;
mod log;
mod pagination;
mod toc;

use app::ReaderApp;
use config::Config;
use eframe::egui;

fn main() -> eframe::Result<()> {
    // 记录静默启动失败/崩溃（GUI 子系统无控制台，肉眼看不到错误）
    std::panic::set_hook(Box::new(|info| {
        log::app_log(&format!("PANIC: {}", info));
    }));
    log::app_log(&format!(
        "app start (config={})",
        Config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "?/path".into())
    ));

    let config = Config::load();
    log::app_log(&format!(
        "config loaded: {}x{} pos={:?} titlebar={} borderless",
        config.window_width,
        config.window_height,
        (config.window_x, config.window_y),
        config.show_titlebar,
    ));

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([config.window_width, config.window_height])
        .with_min_inner_size([config.min_width, config.min_height])
        .with_decorations(false) // 无边框：去掉原生标题栏（最小化/最大化/关闭按钮）
        .with_title("TXT 阅读器");

    if config.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    if let (Some(x), Some(y)) = (config.window_x, config.window_y) {
        viewport = viewport.with_position([x as f32, y as f32]);
    }
    if config.maximized {
        viewport = viewport.with_maximized(true);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    log::app_log("before run_native");
    let result = eframe::run_native(
        "TXT 阅读器",
        native_options,
        Box::new(|cc| {
            use egui::{Color32, Visuals};
            let mut visuals = Visuals::light();
            visuals.panel_fill = Color32::from_rgb(0xf3, 0xec, 0xdd);
            visuals.window_fill = Color32::from_rgb(0xfd, 0xfa, 0xf1);
            visuals.extreme_bg_color = Color32::from_rgb(0xe8, 0xdf, 0xc8);
            visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(0xe8, 0xdf, 0xc8);
            visuals.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0x33, 0x2d, 0x24);
            visuals.widgets.inactive.bg_fill = Color32::from_rgb(0xe8, 0xdf, 0xc8);
            visuals.widgets.hovered.bg_fill = Color32::from_rgb(0xd9, 0xcd, 0xb0);
            visuals.widgets.active.bg_fill = Color32::from_rgb(0x3e, 0x6b, 0x57);
            visuals.selection.bg_fill = Color32::from_rgb(0x3e, 0x6b, 0x57);
            visuals.selection.stroke.color = Color32::from_rgb(0xfd, 0xfa, 0xf1);
            visuals.hyperlink_color = Color32::from_rgb(0x3e, 0x6b, 0x57);
            cc.egui_ctx.set_visuals(visuals);

            log::app_log("app creation callback invoked");
            Ok(Box::new(ReaderApp::new(cc, config)))
        }),
    );

    match &result {
        Ok(()) => log::app_log("run_native returned Ok (窗口正常关闭)"),
        Err(e) => log::app_log(&format!("run_native ERROR: {:?}", e)),
    }
    result
}
