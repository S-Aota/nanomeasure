//! アノテーション付き画像の書き出し。
//!
//! 画面上の測長オーバーレイと同じ内容（フィッティング領域の枠・線・矢印・
//! 測定値ラベル）を画像へ直接描画する。egui の描画を再利用せず、imageproc で
//! 同じ図形を描き直す。画像の外へはみ出た部分は描画時に切り落とされる
//! （imageproc が範囲外の画素を無視する）。
//!
//! 出力は 16bit グレースケール（`render`）と RGB 8bit（`render_rgb`）の
//! 2 通り。描画コードは `AnnotationPixel` で共通化している。

use std::path::Path;
use std::sync::OnceLock;

use ab_glyph::{FontArc, FontVec, PxScale};
use egui::Color32;
use image::{ImageBuffer, Luma, Pixel, Rgb};
use imageproc::definitions::Clamp;
use imageproc::drawing::{draw_polygon_mut, draw_text_mut, text_size};
use imageproc::point::Point;

use crate::frame::Scale;
use crate::gray::Gray16;
use crate::measure::{ComputedMeasure, Pt2, ToolKind, format_measurement};
use crate::measure_mode::{COLOR_DISTANCE, COLOR_GUIDE, region_color};

/// 出力先テンプレートの既定値。`{dir}` / `{filename}` は保存時に画像パスから解決。
pub const DEFAULT_EXPORT_PATH: &str = "{dir}/{filename}_result.jpg";

/// 対応している出力形式の拡張子。拡張子の大文字小文字は無視する。
pub const SUPPORTED_EXTENSIONS: &[&str] = &["tif", "tiff", "png", "jpg", "jpeg"];

/// 出力形式として対応している拡張子か。
pub fn validate_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.iter().any(|s| e.eq_ignore_ascii_case(s)))
}

/// 出力画素の型。グレースケール（16bit）と RGB（8bit）で同じ描画コードを
/// 使うための共通化。`from_screen` が画面表示の色から出力画素を作る。
trait AnnotationPixel: Pixel + 'static
where
    Self::Subpixel: 'static + Into<f32> + Clamp<f32>,
{
    fn from_screen(color: Color32) -> Self;
}

impl AnnotationPixel for Luma<u16> {
    /// 色は輝度へ落とす（グレースケール出力では色を保持できない）。
    fn from_screen(color: Color32) -> Self {
        let lum = (color.r() as u32 * 299 + color.g() as u32 * 587 + color.b() as u32 * 114)
            / 1000;
        Luma([(lum * 257) as u16])
    }
}

impl AnnotationPixel for Rgb<u8> {
    /// 画面表示の色をそのまま使う。
    fn from_screen(color: Color32) -> Self {
        Rgb([color.r(), color.g(), color.b()])
    }
}

/// アノテーション付き 16bit グレースケール画像を作る。
///
/// `overlays` は画面上に重ねて表示しているのと同じ測長オーバーレイ
/// （それぞれの計算結果と、そのときのスケール）。
/// `annotation_scale` は解像度による自動調整に掛ける係数（既定 1.0）。
pub fn render(
    img: &Gray16,
    overlays: &[(ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
) -> ImageBuffer<Luma<u16>, Vec<u16>> {
    let mut out = img.to_luma16_buffer();
    draw_overlays(&mut out, img, overlays, annotation_scale);
    out
}

/// アノテーション付き RGB 8bit 画像を作る（アノテーションの色を残す）。
/// 下地は 16bit グレーの上位 8bit をそのまま使う。
pub fn render_rgb(
    img: &Gray16,
    overlays: &[(ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let mut out = ImageBuffer::from_fn(img.width, img.height, |x, y| {
        let v = (img.at(x, y) >> 8) as u8;
        Rgb([v, v, v])
    });
    draw_overlays(&mut out, img, overlays, annotation_scale);
    out
}

/// 書き出した 16bit グレースケール画像を保存する。
/// jpg / jpeg は 8bit に落とす（JPEG は 16bit 非対応）。
pub fn save(buf: &ImageBuffer<Luma<u16>, Vec<u16>>, path: &Path) -> Result<(), String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let result = if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        image::DynamicImage::ImageLuma16(buf.clone()).to_rgb8().save(path)
    } else {
        buf.save(path)
    };
    result.map_err(|e| format!("{} に保存できません: {e}", path.to_string_lossy()))
}

/// 書き出した RGB 8bit 画像を保存する。
pub fn save_rgb(buf: &ImageBuffer<Rgb<u8>, Vec<u8>>, path: &Path) -> Result<(), String> {
    buf.save(path)
        .map_err(|e| format!("{} に保存できません: {e}", path.to_string_lossy()))
}

/// 測長オーバーレイをまとめて描く（`render` / `render_rgb` 共通部分）。
/// 各描画関数の where 節（`AnnotationPixel` の制約は伝播しないため明示）。
fn draw_overlays<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    src: &Gray16,
    overlays: &[(ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let f = base_scale(src) * annotation_scale.clamp(0.25, 8.0);
    let font = font();
    for (computed, scale) in overlays {
        draw_computed(img, computed, *scale, f, font.as_ref());
    }
}

/// アノテーションの基準倍率。画面では線幅 2px・文字 12px が
/// 画像 1px = 画面 1px のときの見た目なので、出力では画像の解像度に
/// 応じて自動で大きくし、どの解像度でも見た目の大きさが揃うようにする。
fn base_scale(img: &Gray16) -> f32 {
    (img.width.max(img.height) as f32 / 1024.0).clamp(1.0, 6.0)
}

/// ラベル描画用フォント。OS のシステムフォントから最初に見つかったもの。
/// 見つからなければラベルなしで描く。
fn font() -> &'static Option<FontArc> {
    static FONT: OnceLock<Option<FontArc>> = OnceLock::new();
    FONT.get_or_init(|| {
        crate::fonts::first_available_font().and_then(|(bytes, index)| {
            FontVec::try_from_vec_and_index(bytes, index)
                .ok()
                .map(FontArc::new)
        })
    })
}

/// 測長オーバーレイ 1 コマンド分を描く（measure_mode::draw_computed と同じ内容）。
fn draw_computed<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    computed: &ComputedMeasure,
    scale: Option<Scale>,
    f: f32,
    font: Option<&FontArc>,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    for t in &computed.tools {
        // フィッティング領域の枠（点線）。
        for region in &t.fit_regions {
            let mut poly = region.corners().to_vec();
            poly.push(poly[0]);
            draw_dashed_polyline(img, &poly, f, P::from_screen(region_color(region.mode)));
        }
        match t.kind {
            ToolKind::Distance => {
                draw_line_and_arrows(img, t.p1, t.p2, f, P::from_screen(COLOR_DISTANCE));
                if let Some(len) = t.length_px {
                    let text = format_measurement(len, scale);
                    draw_value_label(img, font, t.p1, t.p2, text, P::from_screen(COLOR_DISTANCE), f);
                }
            }
            ToolKind::Boundary => {
                // 長さは結果リストに出るので、画像中には描かない（画面と同じ）。
                let guide = P::from_screen(COLOR_GUIDE);
                draw_thick_line(img, t.p1, t.p2, (2.0 * f).max(1.0), guide);
            }
            ToolKind::Offset => {
                let guide = P::from_screen(COLOR_GUIDE);
                if let Some(src) = t.source.and_then(|s| computed.by_id(s)) {
                    draw_offset_link(img, (src.p1, src.p2), (t.p1, t.p2), f, guide);
                }
                draw_thick_line(img, t.p1, t.p2, (2.0 * f).max(1.0), guide);
                if let Some(d) = t.distance_px {
                    let sign = if d >= 0.0 { "+" } else { "-" };
                    let text = format!("{sign}{}", format_measurement(d.abs(), scale));
                    draw_value_label(img, font, t.p1, t.p2, text, guide, f);
                }
            }
        }
    }
}

/// 線と両端の矢印頭。
fn draw_line_and_arrows<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    a: Pt2,
    b: Pt2,
    f: f32,
    color: P,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    draw_thick_line(img, a, b, (2.0 * f).max(1.0), color);
    draw_arrow_head(img, a, (a - b).normalize(), f, color);
    draw_arrow_head(img, b, (b - a).normalize(), f, color);
}

/// 線の先端から ±25° に開いた 2 本の短線で矢印頭を描く（画面と同じ形）。
/// `dir` は tip から線の内側を向く単位ベクトル。
fn draw_arrow_head<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    tip: Pt2,
    dir: Pt2,
    f: f32,
    color: P,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let rotate = |v: Pt2, deg: f64| {
        let (sin, cos) = deg.to_radians().sin_cos();
        Pt2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
    };
    let len = 9.0 * f as f64;
    let width = (2.0 * f).max(1.0);
    // 画面版は tip - rotate(dir, ±25°) * 9（引き算で向きを折り返す）。
    draw_thick_line(img, tip, tip - rotate(dir, 25.0) * len, width, color);
    draw_thick_line(img, tip, tip - rotate(dir, -25.0) * len, width, color);
}

/// オフセット線と元の境界線の関係を示す矢印（元の中点 → オフセット線の中点）。
fn draw_offset_link<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    src: (Pt2, Pt2),
    off: (Pt2, Pt2),
    f: f32,
    color: P,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let sm = (src.0 + src.1) * 0.5;
    let om = (off.0 + off.1) * 0.5;
    // 距離 0 で線が重なっているときは描かない。
    if (om - sm).length() < 2.0 * f as f64 {
        return;
    }
    draw_thick_line(img, sm, om, (1.0 * f).max(1.0), color);
    draw_arrow_head(img, om, (om - sm).normalize(), f, color);
}

/// 線の中点から少し浮かせて測定値を描く（画面の draw_value_label と同じ位置）。
fn draw_value_label<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    font: Option<&FontArc>,
    a: Pt2,
    b: Pt2,
    text: String,
    color: P,
    f: f32,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let mid = (a + b) * 0.5;
    let d = b - a;
    let n = if d.length() > 1.0 {
        d.perp() / d.length()
    } else {
        Pt2::new(0.0, 1.0)
    };
    let anchor = mid + n * 8.0 * f as f64;
    let Some(font) = font else {
        return;
    };
    let px = PxScale::from(12.0 * f);
    let (w, h) = text_size(px, font, &text);
    // draw_text_mut の y はテキスト上端。下端を anchor に合わせる。
    let x = (anchor.x - w as f64 * 0.5).round() as i32;
    let y = (anchor.y - h as f64).round() as i32;
    draw_text_mut(img, color, x, y, px, font, &text);
}

/// 中点を中心にした太さ `width` の線分（4 角形を塗る）。
fn draw_thick_line<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    a: Pt2,
    b: Pt2,
    width: f32,
    color: P,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let ab = b - a;
    let len = ab.length();
    if len < 1e-6 {
        return;
    }
    let n = ab.perp() / len;
    let hw = width * 0.5;
    fill_polygon(
        img,
        &[
            a + n * hw as f64,
            a - n * hw as f64,
            b - n * hw as f64,
            b + n * hw as f64,
        ],
        color,
    );
}

/// 折れ線を点線で描く（ダッシュと隙間を交互に）。フィッティング領域の枠に使う。
fn draw_dashed_polyline<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    pts: &[Pt2],
    f: f32,
    color: P,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let dash = 6.0 * f;
    let gap = 5.0 * f;
    let width = (1.5 * f).max(1.0);
    for pair in pts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let ab = b - a;
        let len = ab.length();
        if len < 1e-6 {
            continue;
        }
        let dir = ab / len;
        let mut t = 0.0f64;
        let mut on = true;
        while t < len {
            let seg = (if on { dash } else { gap } as f64).min(len - t);
            if on {
                draw_thick_line(img, a + dir * t, a + dir * (t + seg), width, color);
            }
            t += seg;
            on = !on;
        }
    }
}

/// 多角形を塗る（頂点は i32 に丸める。範囲外の画素は無視される）。
fn fill_polygon<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    pts: &[Pt2],
    color: P,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let poly: Vec<Point<i32>> = pts
        .iter()
        .map(|p| Point::new(p.x.round() as i32, p.y.round() as i32))
        .collect();
    draw_polygon_mut(img, &poly, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::{MeasureData, MeasureTool};

    fn computed_with_tool(tool: MeasureTool, data: &MeasureData) -> ComputedMeasure {
        let img = Gray16::black(200, 200);
        let mut data = data.clone();
        data.tools.push(tool);
        data.compute(&img, None)
    }

    /// gray() と同じ変換を値で返す（アサート用）。
    fn gray_of_line(color: egui::Color32) -> u16 {
        Luma::<u16>::from_screen(color).0[0]
    }

    /// 二点間測長の線が画像へ描かれること（線の中点がアノテーション色になる）。
    #[test]
    fn distance_line_is_drawn() {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        let tool = MeasureTool::Distance {
            id: 1,
            p1: Pt2::new(10.0, 100.0),
            p2: Pt2::new(190.0, 100.0),
            group: g,
            fit1: Default::default(),
            fit2: Default::default(),
        };
        let computed = computed_with_tool(tool, &data);
        let img = Gray16::black(200, 200);
        let out = render(&img, &[(computed, None)], 1.0);
        let line = gray_of_line(COLOR_DISTANCE);
        assert_eq!(out.get_pixel(100, 100).0[0], line, "線の中点");
        assert_eq!(out.get_pixel(100, 0).0[0], 0, "線から離れた画素は元のまま");
    }

    /// 矢印頭が画面と同じ向きに開くこと（羽は線の内側へ折り返す）。
    #[test]
    fn arrow_head_matches_screen_direction() {
        let img = Gray16::black(400, 400);
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        data.tools.push(MeasureTool::Distance {
            id: 1,
            p1: Pt2::new(100.0, 200.0),
            p2: Pt2::new(300.0, 200.0),
            group: g,
            fit1: Default::default(),
            fit2: Default::default(),
        });
        let computed = data.compute(&img, None);
        let out = render(&img, &[(computed, None)], 1.0);
        let line = gray_of_line(COLOR_DISTANCE);
        // 端点 2 の矢印: tip=(300,200)、dir=(1,0)（線の内側向き）。
        // tip - rotate(dir, ±25°)*9 で羽は左側・上下に開く:
        // (291.9, 196.2) / (291.9, 203.8)。
        assert_eq!(out.get_pixel(292, 196).0[0], line, "上側の羽");
        assert_eq!(out.get_pixel(292, 204).0[0], line, "下側の羽");
        // 鏡像（線の外側へ開く）だと羽は (308.2, 196.2) / (308.2, 203.8) に来る。
        // この位置はフィッティング領域の枠と重なるので 0 とは限らないが、
        // 少なくとも測長線の色ではないこと（正しい実装なら点線枠か背景）。
        assert_ne!(out.get_pixel(308, 196).0[0], line, "鏡像位置には羽を描かない");
        assert_ne!(out.get_pixel(308, 204).0[0], line, "鏡像位置には羽を描かない");
    }

    /// RGB 出力ではアノテーションの色がそのまま残ること。
    #[test]
    fn rgb_keeps_annotation_colors() {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        let tool = MeasureTool::Distance {
            id: 1,
            p1: Pt2::new(10.0, 100.0),
            p2: Pt2::new(190.0, 100.0),
            group: g,
            fit1: Default::default(),
            fit2: Default::default(),
        };
        let computed = computed_with_tool(tool, &data);
        let img = Gray16::black(200, 200);
        let out = render_rgb(&img, &[(computed, None)], 1.0);
        assert_eq!(out.get_pixel(100, 100).0, [235, 70, 70], "線の中点が赤");
        assert_eq!(out.get_pixel(100, 0).0, [0, 0, 0], "線から離れた画素は黒");
    }

    /// 対応拡張子の判定（テンプレートのままでも拡張子が読めること）。
    #[test]
    fn extension_validation() {
        assert!(validate_extension(Path::new("{dir}/{filename}_result.jpg")));
        assert!(validate_extension(Path::new("a.tiff")));
        assert!(validate_extension(Path::new("a.png")));
        assert!(!validate_extension(Path::new("a.bmp")));
        assert!(!validate_extension(Path::new("noext")));
    }

    /// 実画像への描画と tif / png / jpg の保存をまとめて確かめる。
    #[test]
    fn end_to_end_save_smoke() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/nanoparticles_16bit.tif");
        let mut cache = std::collections::HashMap::new();
        let img = crate::command::load_image(&src, &mut cache).expect("testdata が読める");

        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        data.tools.push(MeasureTool::Distance {
            id: 1,
            p1: Pt2::new(10.0, 20.0),
            p2: Pt2::new(100.0, 30.0),
            group: g,
            fit1: Default::default(),
            fit2: Default::default(),
        });
        data.tools.push(MeasureTool::Boundary {
            id: 2,
            p1: Pt2::new(30.0, 10.0),
            p2: Pt2::new(40.0, 90.0),
            group: g,
            fit: Default::default(),
        });
        data.tools.push(MeasureTool::Offset {
            id: 3,
            source: 2,
            distance: 12.0,
        });
        let computed = data.compute(&img, None);
        let out = render(&img, &[(computed.clone(), None)], 1.5);

        let dir = std::env::temp_dir().join(format!("tem_measure_smoke_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, tolerance) in [("out.tif", 32i32), ("out.png", 32), ("out.jpg", 5000)] {
            let path = dir.join(name);
            save(&out, &path).expect("保存できる");
            let read = image::open(&path).expect("保存したファイルが読める");
            assert_eq!((read.width(), read.height()), (img.width, img.height));
            // 測長線の中点はアノテーション色になっている（jpg は非可逆なので許容を広く）。
            let gray = gray_of_line(COLOR_DISTANCE);
            let mid = read.to_luma16();
            let px = mid.get_pixel(55, 25).0[0];
            assert!(
                (px as i32 - gray as i32).abs() <= tolerance,
                "{name}: 線の中点がアノテーション色 ({px} ≈ {gray})"
            );
        }
        // RGB 版: png に保存して赤チャンネルが残ること。
        let rgb = render_rgb(&img, &[(computed, None)], 1.5);
        let rgb_path = dir.join("out_rgb.png");
        save_rgb(&rgb, &rgb_path).expect("RGB 保存できる");
        let read = image::open(&rgb_path).expect("RGB が読める");
        assert_eq!(read.to_rgb8().get_pixel(55, 25).0, [235, 70, 70], "線の中点が赤");
        std::fs::remove_dir_all(&dir).ok();
    }
}
