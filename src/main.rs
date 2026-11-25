//! 简易桌面图片查看器：打开图片、按 1:1 或缩放显示、前后切换同目录图片。

// 以 GUI 子系统编译：双击 exe 时 Windows 不再分配控制台窗口。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

// perf 必须最先声明：里面的宏以文本作用域方式对其后所有模块可见。
#[macro_use]
mod perf;

mod annotation;
mod console;
mod editor;
mod gallery;
mod geometry;
mod icons;
mod loader;
mod ocr;
mod ocr_cache;
mod ocr_worker;
mod settings;
mod viewer;

#[cfg(test)]
mod ocr_e2e_test;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // 计时起点：必须早于其它初始化，才能量出真实的启动耗时。
    perf::init();

    // 必须在任何日志输出之前接管 stderr，否则 GUI 模式下的诊断信息会丢失。
    console::redirect_stderr_to_log();
    perf_log!("进程启动");

    // 任务栏与标题栏图标，与 build.rs 嵌入 exe 的资源保持一致。
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_256.png"))
        .expect("icon_256.png should be a valid png");
    perf_log!("图标加载完成");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // app_id 决定存档目录，固定后设置与窗口状态才能跨版本恢复。
            .with_app_id("photo-viewer")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([360.0, 260.0])
            .with_icon(icon),
        ..Default::default()
    };
    let initial = std::env::args().nth(1).map(PathBuf::from);
    perf_log!("窗口选项就绪");

    eframe::run_native(
        "Photo Viewer",
        options,
        Box::new(move |cc| {
            // 窗口此时已经创建，uptime 即为「进程启动 → 窗口就绪」的耗时。
            perf_log!("窗口创建完成");
            let mut app = viewer::PhotoViewer::new();
            perf_log!("应用实例创建完成");
            app.load_settings(cc.storage);
            if let Some(path) = initial {
                // 只登记路径：目录扫描与解码都在后台，窗口先画出来。
                app.open(&path);
                perf_log!("初始图片已登记，后台加载中");
            }
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
}
