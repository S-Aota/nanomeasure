//! TIFF のタグから画素の実寸法を読み取る。
//!
//! 対応しているのは次の 2 つ。読めなければ `None` を返し、スケール設定
//! コマンドは自動追加されない（誤った値を勝手に入れないため）。
//!
//! - FEI / Thermo Fisher の私的タグ（34682 FEI_HELIOS / 34680 FEI_SFEG /
//!   34683 FEI_TITAN）。中身は INI 形式のテキストで、`[Scan]` セクションの
//!   `PixelWidth` / `PixelHeight` に **メートル単位**の画素サイズが入る。
//! - ImageJ が書く `ImageDescription`（`unit=nm` など）と `XResolution`。
//!
//! 素の `XResolution` / `ResolutionUnit` だけの TIFF は見ない。多くの書き出し
//! ソフトが意味のない 72 dpi を既定で埋めるため、それを寸法として採用すると
//! 誤ったスケールが入ってしまう。
//!
//! x と y の画素サイズは 1 つのスケールへまとめる。差が平均の 1% 以内なら
//! 読取誤差とみなして平均を採用し、超えるなら矩形画素の可能性があるため
//! 読み取り失敗にする（正しくない値を 1 つに潰さない）。

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use tiff::decoder::Decoder;
use tiff::tags::Tag;

use crate::frame::Scale;

/// FEI / Thermo Fisher が装置情報を入れる私的タグ。
const FEI_TAGS: [u16; 3] = [34682, 34680, 34683];

/// TIFF から画素の実寸法を読む。TIFF でない、またはスケールが無ければ `None`。
pub fn read_tiff_scale(path: &Path) -> Option<Scale> {
    let is_tiff = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
    if !is_tiff {
        return None;
    }

    let file = File::open(path).ok()?;
    let mut decoder = Decoder::new(BufReader::new(file)).ok()?;

    for tag in FEI_TAGS {
        if let Some(text) = read_text_tag(&mut decoder, Tag::Unknown(tag))
            && let Some(scale) = parse_fei_metadata(&text)
        {
            return Some(scale);
        }
    }

    let description = read_text_tag(&mut decoder, Tag::ImageDescription)?;
    let x_res = decoder.get_tag_f64(Tag::XResolution).ok()?;
    let y_res = decoder.get_tag_f64(Tag::YResolution).unwrap_or(x_res);
    parse_imagej_metadata(&description, x_res, y_res)
}

/// ASCII タグとして読み、駄目なら生バイト列として読む。
/// FEI の私的タグは装置によって BYTE 型で書かれていることがある。
fn read_text_tag<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
) -> Option<String> {
    if let Ok(text) = decoder.get_tag_ascii_string(tag) {
        return Some(text);
    }
    let bytes = decoder.get_tag_u8_vec(tag).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// FEI の INI テキストから `[Scan]` の PixelWidth / PixelHeight（メートル）を読む。
fn parse_fei_metadata(text: &str) -> Option<Scale> {
    let width_m = ini_value(text, "Scan", "PixelWidth")?.parse::<f64>().ok()?;
    let height_m = ini_value(text, "Scan", "PixelHeight")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(width_m);
    as_single_scale(width_m * 1e9, height_m * 1e9)
}

/// ImageJ の `ImageDescription` の `unit=` と、解像度タグ（単位あたりの画素数）を組む。
fn parse_imagej_metadata(description: &str, x_res: f64, y_res: f64) -> Option<Scale> {
    if !description.contains("ImageJ") {
        return None;
    }
    let unit = description.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "unit").then(|| value.trim().to_owned())
    })?;
    let unit_nm = match unit.as_str() {
        "pm" => 1e-3,
        "A" | "Å" | "angstrom" => 0.1,
        "nm" => 1.0,
        "um" | "µm" | "micron" => 1e3,
        "mm" => 1e6,
        _ => return None,
    };
    as_single_scale(unit_nm / x_res, unit_nm / y_res)
}

/// x と y（nm/px）を 1 つのスケールへまとめる。どちらかが正でないか、
/// 差が平均の 1% を超える場合は `None`。
fn as_single_scale(x_nm: f64, y_nm: f64) -> Option<Scale> {
    let mean = (x_nm + y_nm) / 2.0;
    let usable = x_nm.is_finite()
        && y_nm.is_finite()
        && x_nm > 0.0
        && y_nm > 0.0
        && (x_nm - y_nm).abs() / mean <= 0.01;
    usable.then_some(Scale::new(mean))
}

/// INI 形式のテキストから `[section]` 内の `key` の値を取り出す（大文字小文字は無視）。
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim().trim_end_matches('\0');
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name.trim().eq_ignore_ascii_case(section);
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case(key)
        {
            return Some(v.trim().to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEI_SAMPLE: &str = "[Beam]\r\n\
        Beam=EBeam\r\n\
        Scan=EScan\r\n\
        [Scan]\r\n\
        InternalScan=true\r\n\
        PixelWidth=9.3517e-011\r\n\
        PixelHeight=9.3517e-011\r\n";

    #[test]
    fn reads_fei_pixel_size_in_meters() {
        let scale = parse_fei_metadata(FEI_SAMPLE).expect("FEI メタデータを読めること");
        assert!((scale.nm_per_px - 0.093517).abs() < 1e-9);
    }

    /// x と y の差が平均の 1% 以内なら、読取誤差として平均を採用すること。
    #[test]
    fn averages_small_anisotropy() {
        let text = "[Scan]\r\nPixelWidth=9.35e-011\r\nPixelHeight=9.42e-011\r\n";
        let scale = parse_fei_metadata(text).expect("1% 以内の差は読めること");
        assert!((scale.nm_per_px - 0.09385).abs() < 1e-5);
        let text = "[Scan]\r\nPixelWidth=9.4e-011\r\nPixelHeight=9.45e-011\r\n";
        // 平均 (0.09425) に対する差 0.00025 / 0.09425 ≈ 0.3% → 読める。
        assert!(parse_fei_metadata(text).is_some());
    }

    /// 差が 1% を超えると矩形画素の可能性があるため、潰さず読み取り失敗にすること。
    #[test]
    fn rejects_large_anisotropy() {
        let text = "[Scan]\r\nPixelWidth=9.5e-011\r\nPixelHeight=9.8e-011\r\n";
        // 平均 (0.0965) に対する差 0.0015 / 0.0965 ≈ 1.6% → 読まない。
        assert!(parse_fei_metadata(text).is_none());
    }

    #[test]
    fn ini_lookup_is_section_scoped() {
        // [Beam] にも Scan= があるが、欲しいのは [Scan] セクションの方。
        assert_eq!(
            ini_value(FEI_SAMPLE, "Beam", "Scan").as_deref(),
            Some("EScan")
        );
        assert_eq!(
            ini_value(FEI_SAMPLE, "Scan", "PixelWidth").as_deref(),
            Some("9.3517e-011")
        );
        assert_eq!(ini_value(FEI_SAMPLE, "Scan", "Beam"), None);
    }

    #[test]
    fn rejects_metadata_without_pixel_size() {
        assert!(parse_fei_metadata("[Beam]\r\nHV=200000\r\n").is_none());
        assert!(parse_fei_metadata("[Scan]\r\nPixelWidth=0\r\n").is_none());
        assert!(parse_fei_metadata("[Scan]\r\nPixelWidth=abc\r\n").is_none());
    }

    #[test]
    fn reads_imagej_unit_and_resolution() {
        let desc = "ImageJ=1.54f\nunit=nm\nspacing=1.0\n";
        // 1 nm あたり 4 画素 → 1 px = 0.25 nm
        let scale = parse_imagej_metadata(desc, 4.0, 4.0).expect("ImageJ メタデータを読めること");
        assert!((scale.nm_per_px - 0.25).abs() < 1e-12);
    }

    #[test]
    fn ignores_resolution_without_imagej_unit() {
        assert!(parse_imagej_metadata("ImageJ=1.54f\n", 4.0, 4.0).is_none());
        assert!(parse_imagej_metadata("unit=nm\n", 4.0, 4.0).is_none());
    }

    fn testdata(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name)
    }

    /// `examples/gen_testdata.rs` が書いた FEI タグを、実ファイルから読み戻せること。
    #[test]
    fn reads_scale_from_testdata_tiff() {
        let scale = read_tiff_scale(&testdata("nanoparticles_16bit.tif"))
            .expect("FEI タグを持つ TIFF からスケールを読めること");
        assert!((scale.nm_per_px - 0.093517).abs() < 1e-6);
    }

    /// x と y の差が 1% を超えるファイル（testdata の carbon_support は
    /// 0.5 / 0.48 nm/px）は、1 つの値へ潰さず読み取り失敗にすること。
    #[test]
    fn rejects_anisotropic_testdata_tiff() {
        assert!(read_tiff_scale(&testdata("carbon_support_8bit.tif")).is_none());
    }

    /// メタデータの無いファイルでは、勝手にスケールを作らないこと。
    #[test]
    fn returns_none_without_metadata() {
        assert!(read_tiff_scale(&testdata("low_contrast_12bit_in_16bit.png")).is_none());
        assert!(read_tiff_scale(&testdata("color_sample.png")).is_none());
    }
}
