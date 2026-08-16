//! 日本語表示用のフォント設定。
//!
//! egui の既定フォントには CJK が入っていないため、OS のシステムフォントから
//! 最初に見つかったものを差し込む。見つからない場合は既定のまま（英数字のみ）。

use std::sync::Arc;

use egui::{Context, FontData, FontDefinitions, FontFamily};

/// (ファイルパス, フォントコレクション内の face 番号)
const CANDIDATES: &[(&str, u32)] = &[
    // Windows
    ("C:/Windows/Fonts/YuGothM.ttc", 0),
    ("C:/Windows/Fonts/YuGothR.ttc", 0),
    ("C:/Windows/Fonts/meiryo.ttc", 0),
    ("C:/Windows/Fonts/msgothic.ttc", 0),
    // Linux（開発用）
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Medium.ttc", 0),
    ("/usr/share/fonts/truetype/fonts-japanese-gothic.ttf", 0),
    (
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        0,
    ),
    // macOS
    ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
];

pub fn install_japanese_font(ctx: &Context) {
    for (path, index) in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut data = FontData::from_owned(bytes);
        data.index = *index;

        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert("jp".to_owned(), Arc::new(data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "jp".to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("jp".to_owned());
        ctx.set_fonts(fonts);
        return;
    }
    eprintln!("日本語フォントが見つかりませんでした。日本語が表示されない場合があります。");
}
