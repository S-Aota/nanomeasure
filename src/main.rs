// リリースビルドでは Windows でコンソールウィンドウを出さない。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 翻訳ファイルを読み込む。ロケール名は locales/ 以下のファイル名。
// 言語の切り替えは保存済み設定を読める TemApp::new で行う。
rust_i18n::i18n!("locales", fallback = "ja");

mod app;
mod assoc;
mod command;
mod dialogs;
mod document;
mod export;
mod fonts;
mod frame;
mod gray;
mod measure;
mod measure_fit;
mod measure_mode;
mod metadata;
mod settings;
mod view;

fn main() -> eframe::Result<()> {
    // 引数で渡されたファイルは起動時に開く（画像、または .nmjson コマンド履歴。
    // エクスプローラのダブルクリック／「プログラムから開く」用）。
    let startup_files: Vec<std::path::PathBuf> =
        std::env::args_os().skip(1).map(Into::into).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true)
            // タイトルは言語設定を反映して TemApp::new で差し替える。
            .with_title("NanoMeasure"),
        ..Default::default()
    };
    eframe::run_native(
        "nanomeasure",
        options,
        Box::new(move |cc| Ok(Box::new(app::TemApp::new(cc, startup_files)))),
    )
}
