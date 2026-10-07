use egui::{FontData, FontDefinitions, FontFamily};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 常见中文字体的文件名关键词（不区分大小写）
const CHINESE_FONTS: &[(&str, &str)] = &[
    ("simsun", "宋体 (SimSun)"),
    ("simhei", "黑体 (SimHei)"),
    ("msyh", "微软雅黑 (Microsoft YaHei)"),
    ("kaiti", "楷体 (KaiTi)"),
    ("simkai", "楷体 (KaiTi)"),
    ("fangsong", "仿宋 (FangSong)"),
    ("simfang", "仿宋 (FangSong)"),
    ("deng", "等线 (DengXian)"),
    ("lisu", "隶书 (LiSu)"),
    ("youyuan", "幼圆 (YouYuan)"),
    ("notosanscjk", "Noto Sans CJK"),
    ("notoserifcjk", "Noto Serif CJK"),
    ("noto sans cjk", "Noto Sans CJK"),
    ("noto serif cjk", "Noto Serif CJK"),
    ("wqy", "文泉驿 (WenQuanYi)"),
];

/// 系统字体目录
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Ok(root) = std::env::var("WINDIR") {
            dirs.push(PathBuf::from(root).join("Fonts"));
        }
        dirs.push(PathBuf::from("C:\\Windows\\Fonts"));
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = home_dir() {
            dirs.push(home.join("Library").join("Fonts"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = home_dir() {
            dirs.push(home.join(".fonts"));
            dirs.push(home.join(".local").join("share").join("fonts"));
        }
    }
    dirs
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// 递归扫描字体文件
fn scan_fonts(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_fonts(&path, out);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext = ext.to_lowercase();
                if ext == "ttf" || ext == "ttc" || ext == "otf" {
                    out.push(path);
                }
            }
        }
    }
}

/// 匹配中文字体名
fn match_font_name(filename: &str) -> Option<&'static str> {
    let fname = filename.to_lowercase();
    for (keyword, display) in CHINESE_FONTS {
        if fname.contains(keyword) {
            return Some(display);
        }
    }
    None
}

/// 收集系统中可用的中文字体（显示名 -> 文件路径）
pub fn available_chinese_fonts() -> BTreeMap<String, PathBuf> {
    let mut map: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in font_dirs() {
        let mut files = Vec::new();
        scan_fonts(&dir, &mut files);
        for f in files {
            if let Some(name) = f.file_name().and_then(|n| n.to_str()) {
                if let Some(display) = match_font_name(name) {
                    // 已存在则跳过（保留第一个找到的）
                    map.entry(display.to_string()).or_insert(f.clone());
                }
            }
        }
    }
    map
}

/// 将指定字体加载到 egui 的字体定义中。
/// 返回加载的字体族名列表，第一个作为默认中文字体。
/// 同时把中文字体加入默认 Proportional / Monospace 族作为后备，
/// 这样所有未显式指定字体的 UI 文字（按钮、菜单等）也能正确渲染中文。
/// `unify_baseline=true`（Windows 7）：把偏好的中文字体插入默认族最前，
/// 让全部字符（含数字/字母）统一用中文字体渲染——GDI 渲染下中英混排
/// 基线错位明显，统一字体可消除"数字、字母和文字不在一条线上"。
pub fn install_fonts(
    fonts: &mut FontDefinitions,
    available: &BTreeMap<String, PathBuf>,
    unify_baseline: bool,
) -> Vec<String> {
    // 统一基线时优先的 UI 字体（按偏好顺序）
    const UI_PREFERRED: &[&str] = &[
        "微软雅黑 (Microsoft YaHei)",
        "宋体 (SimSun)",
        "黑体 (SimHei)",
        "Noto Sans CJK",
    ];
    let mut loaded = Vec::new();
    // 统一基线时挑一个 UI 字体插到默认族最前；其余仍追加在末尾
    let mut ui_primary: Option<String> = None;
    if unify_baseline {
        ui_primary = UI_PREFERRED
            .iter()
            .find_map(|name| available.get_key_value(*name).map(|(k, _)| k.clone()));
    }
    for (display, path) in available {
        if let Ok(bytes) = std::fs::read(path) {
            let family_name = display.clone();
            fonts.font_data.insert(
                family_name.clone(),
                FontData::from_owned(bytes).into(),
            );
            fonts
                .families
                .entry(FontFamily::Name(family_name.clone().into()))
                .or_default()
                .push(family_name.clone());
            // 作为默认族的后备字体（追加在末尾，拉丁字符仍用 egui 默认字体，
            // 中文字符自动回退到中文字体）
            if Some(family_name.clone()) == ui_primary {
                // Win7 统一基线：插入最前，覆盖全部字符（拉丁也用中文字体渲染）
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, family_name.clone());
                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .insert(0, family_name.clone());
            } else {
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .push(family_name.clone());
                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .push(family_name.clone());
            }
            loaded.push(family_name);
        }
    }
    loaded
}
