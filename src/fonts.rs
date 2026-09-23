//! CJK 表示用のフォント設定。
//!
//! egui の既定フォントには CJK が入っていないため、OS のシステムフォントから
//! 最初に見つかったものを優先フォントとして差し込み、さらに CJK_FALLBACKS に
//! ある簡体・繁体・韓国語フォントを末尾へ足してグリフを補う。
//! どれも見つからない場合は既定のまま（英数字のみ）。

use std::sync::Arc;

use egui::{Context, FontData, FontDefinitions, FontFamily};

/// 優先フォント（(ファイルパス, フォントコレクション内の face 番号)）。
/// 先頭から探して最初に見つかった 1 つを Proportional の先頭に置く。
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

/// 簡体・繁体・ハングルのフォールバック。優先フォントにないグリフを
/// 補うため、見つかったものはすべて Proportional / Monospace の末尾に足す。
/// Windows 7 以降に標準搭載のフォントのみ（他の OS ではパスが無く飛ばされる）。
const CJK_FALLBACKS: &[(&str, u32)] = &[
    ("C:/Windows/Fonts/msyh.ttc", 0),   // Microsoft YaHei（簡体）
    ("C:/Windows/Fonts/msjh.ttc", 0),   // Microsoft JhengHei（繁体）
    ("C:/Windows/Fonts/malgun.ttf", 0), // Malgun Gothic（ハングル）
];

pub fn install_japanese_font(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let mut found = false;

    // 優先フォント: 最初に見つかった 1 つだけ。
    if let Some((bytes, index)) = CANDIDATES
        .iter()
        .find_map(|(path, index)| std::fs::read(path).ok().map(|bytes| (bytes, *index)))
    {
        let mut data = FontData::from_owned(bytes);
        data.index = index;

        fonts.font_data.insert("cjk".to_owned(), Arc::new(data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk".to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("cjk".to_owned());
        found = true;
    }

    // フォールバック: 見つかったものはすべて末尾へ足す。
    for (i, (path, index)) in CJK_FALLBACKS.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut data = FontData::from_owned(bytes);
        data.index = *index;
        let name = format!("cjk_fallback_{i}");

        fonts.font_data.insert(name.clone(), Arc::new(data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push(name.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(name);
        found = true;
    }

    if !found {
        eprintln!("{}", rust_i18n::t!("fonts.not_found"));
        return;
    }
    ctx.set_fonts(fonts);
}

/// 書き出し画像のラベル描画用に、最初に見つかったフォントのバイト列と
/// face 番号。egui 用の `install_japanese_font` と同じ候補を試す。
pub fn first_available_font() -> Option<(Vec<u8>, u32)> {
    CANDIDATES
        .iter()
        .find_map(|(path, index)| std::fs::read(path).ok().map(|bytes| (bytes, *index)))
}
