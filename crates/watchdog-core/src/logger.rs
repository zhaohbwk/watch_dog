//! 带时间戳的文件日志，超过大小阈值后简单轮转（改名 .1）。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use time::macros::format_description;
use time::OffsetDateTime;

/// 单个日志文件超过该大小（字节）后轮转（看门狗日志与程序输出日志共用）
pub(crate) const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;

pub struct Logger {
    path: PathBuf,
    also_stderr: bool,
    lock: Mutex<()>,
}

impl Logger {
    /// 创建日志器；`also_stderr = true` 时同步输出到控制台（前台调试模式用）。
    pub fn new(path: PathBuf, also_stderr: bool) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)
                    .map_err(|e| format!("创建日志目录失败 {}：{e}", dir.display()))?;
            }
        }
        Ok(Logger {
            path,
            also_stderr,
            lock: Mutex::new(()),
        })
    }

    pub fn info(&self, msg: &str) {
        self.write("INFO", msg);
    }

    pub fn warn(&self, msg: &str) {
        self.write("WARN", msg);
    }

    pub fn error(&self, msg: &str) {
        self.write("ERROR", msg);
    }

    fn write(&self, level: &str, msg: &str) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.rotate_if_needed();
        let line = format!("{} [{level:<5}] {msg}", now_string());
        if self.also_stderr {
            eprintln!("{line}");
        }
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                // 写入失败不中断主流程（避免日志问题拖垮看门狗）
                let _ = writeln!(f, "{line}");
            }
            Err(e) => {
                if self.also_stderr {
                    eprintln!("日志写入失败 {}：{e}", self.path.display());
                }
            }
        }
    }

    fn rotate_if_needed(&self) {
        if let Ok(meta) = fs::metadata(&self.path) {
            if meta.len() > MAX_LOG_SIZE {
                let backup = self.path.with_extension("log.1");
                let _ = fs::remove_file(&backup);
                let _ = fs::rename(&self.path, &backup);
            }
        }
    }
}

/// 当前本地时间字符串（供看门狗日志与程序输出日志的分隔行共用）
pub(crate) fn now_string() -> String {
    let fmt = format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
    );
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    now.format(fmt).unwrap_or_default()
}
