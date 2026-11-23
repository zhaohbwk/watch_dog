//! 日志尾部查看：后台线程读取日志文件末尾内容，支持自动/手动刷新。
//!
//! 读盘与解码都在后台线程执行（结果经通道回传，UI 每帧 poll 非阻塞应用），
//! 避免日志文件较大或磁盘繁忙时阻塞界面。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// 最多读取的字节数（约等于末尾数百行日志）
const TAIL_BYTES: u64 = 256 * 1024;

/// 后台读取结果：（发起读取时的日志路径，内容或错误）
type ReadResult = (PathBuf, Result<String, String>);

pub struct LogView {
    pub auto_refresh: bool,
    pub content: String,
    pub error: Option<String>,
    last_refresh: Instant,
    /// 最近一次后台读取的结果接收端（新请求会整体替换，未完成的旧结果被丢弃）
    rx: Option<Receiver<ReadResult>>,
}

impl Default for LogView {
    fn default() -> Self {
        LogView {
            auto_refresh: true,
            content: "（暂无日志）".to_owned(),
            error: None,
            last_refresh: Instant::now() - Duration::from_secs(60),
            rx: None,
        }
    }
}

impl LogView {
    /// 请求后台重新加载（立即发起，不等自动刷新周期）
    pub fn load(&mut self, path: &Path) {
        self.last_refresh = Instant::now();
        let path = path.to_path_buf();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let result = read_tail(&path, TAIL_BYTES).map_err(|e| format!("读取日志失败：{e}"));
            let _ = tx.send((path, result));
        });
    }

    /// 按需发起后台刷新（`auto_refresh` 时每 2 秒一次，避免频繁读盘）
    pub fn refresh(&mut self, path: &Path) {
        if !self.auto_refresh {
            return;
        }
        if self.last_refresh.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.load(path);
    }

    /// 应用后台读取完成的结果（每帧调用，非阻塞）。
    /// 读取时的路径与当前查看目标不符时忽略——目标已切换，旧内容不应显示。
    pub fn poll(&mut self, current: &Path) {
        let mut latest = None;
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                latest = Some(msg);
            }
        }
        if let Some((path, result)) = latest {
            if path == current {
                match result {
                    Ok(text) => {
                        self.content = text;
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, path: &Path) {
        ui.horizontal(|ui| {
            ui.label(format!("日志文件: {}", path.display()));
            ui.checkbox(&mut self.auto_refresh, "自动刷新");
            if ui.button("立即刷新").clicked() {
                self.load(path);
            }
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::YELLOW, err);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&self.content).monospace().size(11.0))
                        .wrap(),
                );
            });
    }
}

/// 读取文件末尾 `tail_bytes` 字节并解码为文本。
///
/// 编码策略：整体为合法 UTF-8 时按 UTF-8 显示（看门狗自身日志即 UTF-8）；
/// 否则回退 GBK 解码——子进程输出多为控制台代码页编码（中文系统为 GBK），
/// 直接 lossy 转换会整段乱码。
fn read_tail(path: &Path, tail_bytes: u64) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let mut file = std::fs::File::open(path)?;
    let start = meta.len().saturating_sub(tail_bytes);
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(decode_bytes(&bytes))
}

/// UTF-8 优先、GBK 回退的字节解码
fn decode_bytes(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => text,
        Err(e) => {
            let raw = e.into_bytes();
            let (decoded, _, _) = encoding_rs::GBK.decode(&raw);
            decoded.into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode_bytes;

    #[test]
    fn decode_prefers_utf8_and_falls_back_to_gbk() {
        // 合法 UTF-8 → 原样显示（看门狗日志、UTF-8 输出的程序）
        assert_eq!(decode_bytes("中文 abc".as_bytes()), "中文 abc");

        // GBK 编码的“中文”（D6 D0 CE C4，非法 UTF-8）→ 按 GBK 解码
        assert_eq!(decode_bytes(&[0xD6, 0xD0, 0xCE, 0xC4]), "中文");

        // UTF-8 与 GBK 混合：分隔行（UTF-8）+ 子进程 GBK 输出
        let mut mixed = format!("===== 看门狗启动 =====\r\n").into_bytes();
        mixed.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4]);
        assert!(decode_bytes(&mixed).contains("中文"));
    }
}
