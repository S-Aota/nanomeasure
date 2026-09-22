//! アプリ全体の設定と、表示用の長さフォーマット。
//!
//! 設定は eframe の persistence でアプリ終了後も保存される
//! （保存先は OS ごとのデータディレクトリ）。`format_length` は
//! あくまで画面表示用の桁数調整で、ファイルへの保存には使わない
//! （測定結果 JSON やコマンド履歴は元精度の f64 のまま保存する）。

use serde::{Deserialize, Serialize};

/// eframe storage 内で設定を保存するキー。
pub const SETTINGS_KEY: &str = "settings";

pub const DEFAULT_LENGTH_DIGITS: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 長さ表示の小数点以下桁数（1〜5）。
    pub length_digits: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            length_digits: DEFAULT_LENGTH_DIGITS,
        }
    }
}

impl Settings {
    /// 保存ファイルはユーザーが手で書き換えられるので、範囲外の値を直す。
    pub fn sanitized(mut self) -> Self {
        self.length_digits = self.length_digits.clamp(1, 5);
        self
    }

    /// eframe storage から読み込む。未保存・読み込み失敗は既定値。
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        storage
            .and_then(|s| eframe::get_value::<Self>(s, SETTINGS_KEY))
            .unwrap_or_default()
            .sanitized()
    }
}

/// 末尾の余計な 0 を落として長さを文字列化する（表示専用）。
pub fn format_length(v: f64, digits: u8) -> String {
    let s = format!("{v:.prec$}", prec = digits as usize);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_zeros() {
        assert_eq!(format_length(0.5, 5), "0.5");
        assert_eq!(format_length(1.0, 5), "1");
        assert_eq!(format_length(0.093517, 5), "0.09352");
        assert_eq!(format_length(0.0, 5), "0");
    }

    #[test]
    fn respects_digits() {
        assert_eq!(format_length(0.093517, 1), "0.1");
        assert_eq!(format_length(0.093517, 2), "0.09");
        assert_eq!(format_length(0.093517, 3), "0.094");
    }

    #[test]
    fn default_and_sanitize() {
        assert_eq!(Settings::default().length_digits, 5);
        let mut s = Settings::default();
        s.length_digits = 7;
        assert_eq!(s.sanitized().length_digits, 5);
        s.length_digits = 0;
        assert_eq!(s.sanitized().length_digits, 1);
    }
}
