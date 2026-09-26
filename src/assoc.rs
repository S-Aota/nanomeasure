//! .nmjson ファイル関連付け用の .reg ファイル生成。
//!
//! アプリ自身はレジストリを書き換えない。ヘルプメニューの「ファイルの
//! 関連付け」ダイアログから .reg ファイルを保存し、ユーザーがエクスプローラで
//! ダブルクリックして登録する方式。登録先は HKEY_CURRENT_USER なので
//! 管理者権限は不要。
//!
//! .reg は regedit が日本語などの非 ASCII パスを正しく読めるよう
//! UTF-16LE + BOM で書き出す。

use std::path::Path;

/// .reg の文字列値として安全な形へエスケープする（`\` → `\\`、`"` → `\"`）。
fn escape_reg_value(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// exe パスを埋め込んだ .reg のテキスト（行末は CRLF、regedit の慣例に合わせる）。
pub fn reg_file_content(exe_path: &Path) -> String {
    let exe = escape_reg_value(&exe_path.to_string_lossy());
    format!(
        "Windows Registry Editor Version 5.00\r\n\
         \r\n\
         [HKEY_CURRENT_USER\\Software\\Classes\\.nmjson]\r\n\
         @=\"nanomeasure.nmjson\"\r\n\
         \r\n\
         [HKEY_CURRENT_USER\\Software\\Classes\\nanomeasure.nmjson]\r\n\
         @=\"NanoMeasure Command History\"\r\n\
         \r\n\
         [HKEY_CURRENT_USER\\Software\\Classes\\nanomeasure.nmjson\\DefaultIcon]\r\n\
         @=\"\\\"{exe}\\\",0\"\r\n\
         \r\n\
         [HKEY_CURRENT_USER\\Software\\Classes\\nanomeasure.nmjson\\shell\\open\\command]\r\n\
         @=\"\\\"{exe}\\\" \\\"%1\\\"\"\r\n",
        exe = exe,
    )
}

/// regedit が読み取れる UTF-16LE + BOM のバイト列（先頭 FF FE + UTF-16LE）。
pub fn reg_file_bytes(exe_path: &Path) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in reg_file_content(exe_path).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// .reg の文字列値内のエスケープ（`\` と `"` の二重化）。
    #[test]
    fn escapes_backslashes_and_quotes() {
        let content = reg_file_content(Path::new(r"C:\My App\nanomeasure.exe"));
        assert!(
            content.contains("@=\"\\\"C:\\\\My App\\\\nanomeasure.exe\\\" \\\"%1\\\"\""),
            "コマンド行がエスケープされる: {content}"
        );
    }

    /// 生成される .reg が .nmjson の関連付け一式を含むこと。
    #[test]
    fn contains_expected_keys() {
        let content = reg_file_content(Path::new(r"C:\app\nanomeasure.exe"));
        assert!(content.contains("Windows Registry Editor Version 5.00"));
        assert!(content.contains(r"[HKEY_CURRENT_USER\Software\Classes\.nmjson]"));
        assert!(content.contains(r#"@="nanomeasure.nmjson""#));
        assert!(content.contains(
            r"[HKEY_CURRENT_USER\Software\Classes\nanomeasure.nmjson\shell\open\command]"
        ));
        assert!(content.contains(r#"\"%1\""#), "%1 がエスケープ付きで入る");
    }

    /// 非 ASCII（日本語）パスはエスケープされずそのまま埋め込まれること
    /// （UTF-16LE で書き出すので regedit がそのまま読める）。
    #[test]
    fn non_ascii_path_kept_verbatim() {
        let content = reg_file_content(Path::new(r"C:\日本語\nanomeasure.exe"));
        assert!(content.contains("日本語"));
    }

    /// バイト列は BOM 付き UTF-16LE で、テキストとラウンドトリップすること。
    #[test]
    fn utf16le_with_bom() {
        let path = Path::new(r"C:\日本語\nanomeasure.exe");
        let bytes = reg_file_bytes(&path);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "先頭は UTF-16LE BOM");
        let text = reg_file_content(&path);
        assert_eq!(
            bytes.len(),
            2 + text.encode_utf16().count() * 2,
            "BOM + UTF-16LE ぶんの長さ"
        );
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let back = String::from_utf16(&units).expect("UTF-16LE として読める");
        assert_eq!(back, text);
    }

    /// 行末は CRLF のみ（単独の LF を含まない）こと。
    #[test]
    fn crlf_line_endings() {
        let content = reg_file_content(Path::new(r"C:\app\nanomeasure.exe"));
        assert!(content.contains("\r\n"));
        let bare_lf = content
            .as_bytes()
            .windows(1)
            .enumerate()
            .any(|(i, b)| {
                b[0] == b'\n' && (i == 0 || content.as_bytes()[i - 1] != b'\r')
            });
        assert!(!bare_lf, "単独 LF なし: {content:?}");
    }
}
