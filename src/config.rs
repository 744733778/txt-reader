use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 每个文件的阅读进度记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressRecord {
    pub name: String,
    pub offset: usize,
    pub total_length: usize,
    pub font_size: f32,
    pub line_height: f32,
    pub fg: String,
    pub bg: String,
    pub font: String,
    pub auto_ms: u64,
    pub saved_at: u64,
}

/// 全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 窗口位置与大小
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    pub window_width: f32,
    pub window_height: f32,
    pub maximized: bool,

    /// 上次打开的文件路径
    pub last_file: Option<PathBuf>,

    /// 每个文件的阅读进度（key 为文件绝对路径）
    pub progress: HashMap<String, ProgressRecord>,

    /// 启动时自动打开上次文件
    pub auto_open_last: bool,

    /// 最小窗口尺寸
    pub min_width: f32,
    pub min_height: f32,

    /// 始终置顶
    pub always_on_top: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            window_x: None,
            window_y: None,
            window_width: 820.0,
            window_height: 640.0,
            maximized: false,
            last_file: None,
            progress: HashMap::new(),
            auto_open_last: true,
            min_width: 180.0,
            min_height: 120.0,
            always_on_top: false,
        }
    }
}

impl Config {
    /// 获取配置文件路径
    pub fn path() -> Option<PathBuf> {
        if let Some(proj) = directories::ProjectDirs::from("com", "txtreader", "TXT阅读器") {
            Some(proj.config_dir().join("config.json"))
        } else {
            None
        }
    }

    /// 从磁盘加载，失败则返回默认值
    pub fn load() -> Self {
        if let Some(p) = Self::path() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(cfg) = serde_json::from_str(&s) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    /// 保存到磁盘
    pub fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(&p, s);
            }
        }
    }
}
