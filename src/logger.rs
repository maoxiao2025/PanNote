// PanNote 应用日志落盘（v2.5.0 可观测性修复）
// 目标：复盘任何事故时当日日志完整可查——不再依赖会丢失的 eprintln。
// 日志位置：~/Library/Logs/PanNote/app-YYYYMMDD.log，按天滚动，保留 14 天。
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const KEEP_DAYS: u64 = 14;

fn log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Logs/PanNote")
}

fn today() -> String {
    chrono::Local::now().format("%Y%m%d").to_string()
}

/// 追加一行日志（同时保留 stderr 输出，兼容终端调试）
/// v2.6.0 修复：测试进程不再写生产日志——
/// 之前冒烟测试的 ConsistencyCheck/StatusMachine 条目会混入 ~/Library/Logs/PanNote/ 干扰真实事故复盘。
/// 注意：集成测试链接生产 lib，cfg(test) 在 lib 编译单元恒为 false、编译期守卫无效，
/// 故用运行时环境变量（测试 setup 设 PANNOTE_LOG_TO_FILE=0），App 正常运行不受影响。
pub fn log(tag: &str, msg: &str) {
    eprintln!("[{}] {}", tag, msg);
    if std::env::var("PANNOTE_LOG_TO_FILE").map(|v| v == "0").unwrap_or(false) {
        return; // 测试环境：只 stderr，不落盘
    }
    let dir = log_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("app-{}.log", today()));
    let ts = chrono::Local::now().format("%H:%M:%S%.3f");
    let line = format!("[{}] [{}] {}\n", ts, tag, msg);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 启动时清理过期日志（保留 KEEP_DAYS 天）
pub fn cleanup_old_logs() {
    let dir = log_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let cutoff = (chrono::Local::now() - chrono::Duration::days(KEEP_DAYS as i64)).date_naive();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(date_part) = name.strip_prefix("app-").and_then(|s| s.strip_suffix(".log")) {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                if d < cutoff {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// 便捷宏：plog!("Recording", "段 {} 转写完成", idx);
#[macro_export]
macro_rules! plog {
    ($tag:expr, $($arg:tt)*) => {
        $crate::logger::log($tag, &format!($($arg)*))
    };
}
