//! 构建脚本：为 watchdog.exe 嵌入品牌图标资源（资源管理器/任务栏显示）。
//!
//! 图标源：仓库 assets/logo/watchdog.ico（品牌定稿见 assets/logo/README.md）。

use std::env;
use std::path::PathBuf;

fn main() {
    // 仅 Windows 目标编译资源（交叉编译其它平台时跳过）
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let ico = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../assets/logo/watchdog.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("图标路径含非法字符"));
    res.compile().expect("嵌入图标资源失败（需要 MSVC 工具链的 rc.exe）");
    println!("cargo:rerun-if-changed=../../assets/logo/watchdog.ico");
    println!("cargo:rerun-if-changed=build.rs");
}
