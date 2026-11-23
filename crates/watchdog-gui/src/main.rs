//! 应用看门狗图形配置工具入口。
//!
//! 编辑 watchdog.toml、管理 Windows 服务、查看运行日志。
//! 服务端（watchdog.exe）保持无界面运行，本程序是独立的配置界面。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod log_view;
mod service_ctrl;

use app::WatchdogApp;

/// 品牌图标原始 RGBA 像素（256×256，egui `IconData` 直接消费）。
/// 由 assets/logo/png/watchdog-icon-256.png 预导出（installer/tools/gen_brand_assets.py），
/// 避免为解码 PNG 在运行时引入 image 依赖。
static APP_ICON_RGBA: &[u8] = include_bytes!("../assets/icon.rgba");

/// 品牌图标：窗口标题栏与任务栏显示（品牌定稿见 assets/logo/README.md）。
fn app_icon() -> egui::IconData {
    egui::IconData {
        width: 256,
        height: 256,
        rgba: APP_ICON_RGBA.to_vec(),
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([960.0, 640.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        &format!(
            "应用看门狗 - 配置工具 v{}",
            watchdog_core::BUILD_VERSION
        ),
        options,
        Box::new(|cc| Ok(Box::new(WatchdogApp::new(cc)))),
    )
}

/// 加载系统中文字体，保证界面中文正常显示（egui 默认字体不含 CJK 字形）。
///
/// 按顺序尝试各平台常见中文字体，首个加载成功的作为回退字体追加到字体族末尾
/// （拉丁字符仍由 egui 默认字体渲染，中文走回退字体）。
pub fn setup_cjk_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        // Windows（Server 2022）
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Linux（常见发行版）
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ];

    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "cjk".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    // 全部失败时界面中文会显示为方框，但不影响功能
}
