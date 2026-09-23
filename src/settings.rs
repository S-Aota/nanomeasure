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

/// UI 言語。保存値は rust-i18n のロケール名（locales/ 以下のファイル名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Ja,
    En,
    Kr,
    Cn,
    Tw,
}

impl Language {
    /// 設定ダイアログで選べる言語の一覧。
    pub const ALL: [Language; 5] = [
        Language::Ja,
        Language::En,
        Language::Kr,
        Language::Cn,
        Language::Tw,
    ];

    /// rust-i18n のロケール名。起動時に `set_locale` へ渡す。
    pub fn code(self) -> &'static str {
        match self {
            Language::Ja => "ja",
            Language::En => "en",
            Language::Kr => "kr",
            Language::Cn => "cn",
            Language::Tw => "tw",
        }
    }

    /// 選択肢の表示名。各言語自身の表記のまま（翻訳しない）。
    pub fn label(self) -> &'static str {
        match self {
            Language::Ja => "日本語",
            Language::En => "English",
            Language::Kr => "한국어",
            Language::Cn => "简体中文",
            Language::Tw => "繁體中文",
        }
    }

    pub fn from_code(code: &str) -> Option<Language> {
        match code {
            "ja" => Some(Language::Ja),
            "en" => Some(Language::En),
            "kr" => Some(Language::Kr),
            "cn" => Some(Language::Cn),
            "tw" => Some(Language::Tw),
            _ => None,
        }
    }
}

impl Default for Language {
    fn default() -> Self {
        Language::Ja
    }
}

/// 保存ファイルを手で書き換えられても既定値に戻せるよう、未知の値は Ja にする。
impl<'de> Deserialize<'de> for Language {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let code = String::deserialize(deserializer)?;
        Ok(Self::from_code(&code).unwrap_or_default())
    }
}

/// 文字列として保存する。derive のままだと eframe の保存形式 (RON) で
/// 識別子（`ja`）として書かれ、読み戻しに失敗して設定ごと既定値に戻るため。
impl Serialize for Language {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.code())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 長さ表示の小数点以下桁数（1〜5）。
    pub length_digits: u8,
    /// UI 言語。起動時に `rust_i18n::set_locale` へ反映される。
    pub language: Language,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            length_digits: DEFAULT_LENGTH_DIGITS,
            language: Language::default(),
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

    #[test]
    fn language_default_and_roundtrip() {
        assert_eq!(Settings::default().language, Language::Ja);
        let json = serde_json::to_string(&Settings::default()).unwrap();
        let s: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s.language, Language::Ja);
    }

    #[test]
    fn unknown_language_falls_back_to_ja() {
        // 手で書き換えられた保存ファイルでも、他の設定は壊さず読み込む。
        let s: Settings =
            serde_json::from_str(r#"{"language": "fr", "length_digits": 3}"#).unwrap();
        assert_eq!(s.language, Language::Ja);
        assert_eq!(s.length_digits, 3);
        // 言語フィールドが無い（旧バージョンの）保存データも読める。
        let s: Settings = serde_json::from_str(r#"{"length_digits": 2}"#).unwrap();
        assert_eq!(s.language, Language::Ja);
    }

    /// eframe の persistence は RON。unit バリアントが識別子として書かれると
    /// 読み戻しに失敗して設定が全て既定値へ戻るため、文字列で往復できることを確かめる。
    #[test]
    fn language_ron_roundtrip() {
        let s = Settings::default();
        let ron_text = ron::to_string(&s).unwrap();
        assert!(ron_text.contains("language:\"ja\""), "got: {ron_text}");
        let back: Settings = ron::from_str(&ron_text).unwrap();
        assert_eq!(back.language, Language::Ja);
        assert_eq!(back.length_digits, s.length_digits);
    }
}
