//! 极简启动日志：用于排查"双击无反应"的静默启动失败。
//! 日志写到系统临时目录下的 txt-reader.log（Windows 即 %TEMP%\txt-reader.log）。
use std::io::Write;
use std::path::PathBuf;

pub fn log_file() -> PathBuf {
    std::env::temp_dir().join("txt-reader.log")
}

pub fn app_log(msg: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "?".to_string());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file())
    {
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}
