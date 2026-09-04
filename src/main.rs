// リリースビルドでは Windows でコンソールウィンドウを出さない。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod command;
mod dialogs;
mod document;
mod fonts;
mod frame;
mod gray;
mod measure;
mod measure_fit;
mod measure_mode;
mod metadata;
mod view;

fn main() -> eframe::Result<()> {
    // 引数で渡されたファイルは起動時に開く（エクスプローラの「プログラムから開く」用）。
    let startup_files: Vec<std::path::PathBuf> =
        std::env::args_os().skip(1).map(Into::into).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true)
            .with_title("tem_measure - TEM 画像解析"),
        ..Default::default()
    };
    eframe::run_native(
        "tem_measure",
        options,
        Box::new(move |cc| Ok(Box::new(app::TemApp::new(cc, startup_files)))),
    )
}
