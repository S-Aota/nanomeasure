//! アノテーション付き画像の書き出し。
//!
//! 画面上の測長オーバーレイと同じ内容（フィッティング領域の枠・線・矢印・
//! 測定値ラベル）を画像へ直接描画する。egui の描画を再利用せず、imageproc で
//! 同じ図形を描き直す。画像の外へはみ出た部分は描画時に切り落とされる
//! （imageproc が範囲外の画素を無視する）。
//!
//! 出力は 16bit グレースケール（`render`）と RGB 8bit（`render_rgb`）の
//! 2 通り。描画コードは `AnnotationPixel` で共通化している。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ab_glyph::{FontArc, FontVec, PxScale};
use egui::Color32;
use image::{ImageBuffer, Luma, Pixel, Rgb};
use imageproc::definitions::Clamp;
use imageproc::drawing::{draw_polygon_mut, draw_text_mut, text_size};
use imageproc::point::Point;

use rust_i18n::t;

use crate::command::Command;
use crate::document::Document;
use crate::frame::{Frame, Scale};
use crate::gray::Gray16;
use crate::measure::{ComputedMeasure, Pt2, ToolKind, format_angle, format_measurement};
use crate::measure_mode::{COLOR_DISTANCE, COLOR_GUIDE, region_edge_colors};

/// 出力先テンプレートの既定値。`{dir}` / `{filename}` は保存時に画像パスから解決。
pub const DEFAULT_EXPORT_PATH: &str = "{dir}/{filename}_result.jpg";

/// 測定結果 JSON の出力先テンプレートの既定値。
pub const DEFAULT_RESULT_PATH: &str = "{dir}/{filename}_result.json";

/// 測定結果 CSV の出力先テンプレートの既定値。
pub const DEFAULT_CSV_PATH: &str = "{dir}/{filename}_result.csv";

/// 対応している出力形式の拡張子。拡張子の大文字小文字は無視する。
pub const SUPPORTED_EXTENSIONS: &[&str] = &["tif", "tiff", "png", "jpg", "jpeg"];

/// 出力形式として対応している拡張子か。
pub fn validate_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        SUPPORTED_EXTENSIONS
            .iter()
            .any(|s| e.eq_ignore_ascii_case(s))
    })
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
        let lum = (color.r() as u32 * 299 + color.g() as u32 * 587 + color.b() as u32 * 114) / 1000;
        Luma([(lum * 257) as u16])
    }
}

impl AnnotationPixel for Luma<u8> {
    /// 色は輝度へ落とす（グレースケール出力では色を保持できない）。
    fn from_screen(color: Color32) -> Self {
        let lum = (color.r() as u32 * 299 + color.g() as u32 * 587 + color.b() as u32 * 114) / 1000;
        Luma([lum as u8])
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
/// `digits` はラベルの小数点以下桁数（表示設定）。
pub fn render(
    img: &Gray16,
    overlays: &[(&ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
    digits: u8,
) -> ImageBuffer<Luma<u16>, Vec<u16>> {
    let mut out = img.to_luma16_buffer();
    draw_overlays(&mut out, img, overlays, annotation_scale, digits);
    out
}

/// アノテーション付き 8bit グレースケール画像を作る（8bit 画像用。
/// 16bit 画像に呼ぶと下位 8bit へ落ちるので呼び分けること）。
pub fn render8(
    img: &Gray16,
    overlays: &[(&ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
    digits: u8,
) -> ImageBuffer<Luma<u8>, Vec<u8>> {
    let mut out = img.to_luma8_buffer();
    draw_overlays(&mut out, img, overlays, annotation_scale, digits);
    out
}

/// アノテーション付き RGB 8bit 画像を作る（アノテーションの色を残す）。
/// 下地は画素値を 8bit へ落とす（16bit は上位 8bit、8bit はそのまま）。
pub fn render_rgb(
    img: &Gray16,
    overlays: &[(&ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
    digits: u8,
) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let mut out = ImageBuffer::from_fn(img.width, img.height, |x, y| {
        let v = if img.depth == 8 {
            img.at(x, y) as u8
        } else {
            (img.at(x, y) >> 8) as u8
        };
        Rgb([v, v, v])
    });
    draw_overlays(&mut out, img, overlays, annotation_scale, digits);
    out
}

/// 書き出した 16bit グレースケール画像を保存する。
/// jpg / jpeg は 8bit に落とす（JPEG は 16bit 非対応）。
pub fn save(buf: ImageBuffer<Luma<u16>, Vec<u16>>, path: &Path) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let result = if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        image::DynamicImage::ImageLuma16(buf).to_rgb8().save(path)
    } else {
        buf.save(path)
    };
    result.map_err(|e| {
        t!(
            "cmd.cannot_save",
            path = path.to_string_lossy(),
            error = format!("{e}")
        )
        .into_owned()
    })
}

/// 書き出した 8bit グレースケール画像を保存する（8bit 画像用）。
pub fn save8(buf: ImageBuffer<Luma<u8>, Vec<u8>>, path: &Path) -> Result<(), String> {
    buf.save(path).map_err(|e| {
        t!(
            "cmd.cannot_save",
            path = path.to_string_lossy(),
            error = format!("{e}")
        )
        .into_owned()
    })
}

/// 書き出した RGB 8bit 画像を保存する。
pub fn save_rgb(buf: &ImageBuffer<Rgb<u8>, Vec<u8>>, path: &Path) -> Result<(), String> {
    buf.save(path).map_err(|e| {
        t!(
            "cmd.cannot_save",
            path = path.to_string_lossy(),
            error = format!("{e}")
        )
        .into_owned()
    })
}

/// 画像出力コマンドを実行する。`index` は画像出力コマンドの位置。
/// アノテーションは、画面上に重ねて表示しているのと同じ測長オーバーレイ
/// （`skip` は測長モードで編集中の未確定コマンド）。出力先テンプレートが
/// 空のときは保存しない（Ok(None)）。戻り値は実際に保存したパス。
pub fn save_image_export(
    doc: &Document,
    index: usize,
    skip: Option<usize>,
    digits: u8,
) -> Result<Option<PathBuf>, String> {
    let Some(Command::ExportImage {
        output,
        annotation_scale,
        color,
    }) = doc.commands.get(index).map(|c| &c.command)
    else {
        return Err(t!("exp.not_image_command").into_owned());
    };
    let template = output.trim();
    if template.is_empty() {
        return Ok(None);
    }
    let img_path = doc
        .image_path_at(index)
        .ok_or_else(|| t!("exp.no_image").into_owned())?;
    let Some(frame) = doc.input_to(index) else {
        return Err(t!("exp.no_result").into_owned());
    };
    let path = resolve_output_path(template, img_path);
    if !validate_extension(&path) {
        return Err(t!("exp.unsupported_ext", path = path.to_string_lossy()).into_owned());
    }
    let measures = doc.measure_overlays(&frame.image, skip);
    let overlays: Vec<(&ComputedMeasure, Option<Scale>)> = measures
        .iter()
        .map(|m| (m.computed.as_ref(), m.scale))
        .collect();
    if *color {
        save_rgb(
            &render_rgb(&frame.image, &overlays, *annotation_scale, digits),
            &path,
        )?;
    } else if frame.image.depth == 8 {
        save8(
            render8(&frame.image, &overlays, *annotation_scale, digits),
            &path,
        )?;
    } else {
        save(
            render(&frame.image, &overlays, *annotation_scale, digits),
            &path,
        )?;
    }
    Ok(Some(path))
}

/// 結果出力コマンド（JSON / CSV 共通）の出力先と入力フレームを解決する。
/// 出力先テンプレートが空のときは保存しない（Ok(None)）。
fn result_target(
    doc: &Document,
    index: usize,
) -> Result<Option<(PathBuf, PathBuf, &Frame)>, String> {
    let Some(item) = doc.commands.get(index) else {
        return Err(t!("exp.not_result_command").into_owned());
    };
    let Command::ExportResult { output, .. } = &item.command else {
        return Err(t!("exp.not_result_command").into_owned());
    };
    let template = output.trim();
    if template.is_empty() {
        return Ok(None);
    }
    let img_path = doc
        .image_path_at(index)
        .ok_or_else(|| t!("exp.no_image").into_owned())?
        .to_path_buf();
    let frame = doc
        .input_to(index)
        .ok_or_else(|| t!("exp.no_result").into_owned())?;
    let path = resolve_output_path(template, &img_path);
    Ok(Some((path, img_path, frame)))
}

/// 結果ファイル（JSON / CSV 共通）のメタデータ。
/// キー名は CSV のメタデータ行と JSON のキーの両方にそのまま使う。
struct ResultMeta {
    filename: String,
    /// 出力日時（ローカル時刻の RFC 3339）。
    exported_at: String,
    /// 1 画素の実寸法（nm）。スケール未設定なら `None`。
    scale_nm_per_px: Option<f64>,
    image_width: u32,
    image_height: u32,
    /// 画素値のビット深度（8 または 16）。
    bit_depth: u8,
    /// 画像全体の実寸法（例: "95.76 x 95.76 nm"）。スケール未設定なら `None`。
    image_extent: Option<String>,
    /// 距離の単位（スケール未設定なら "px"）。
    unit: String,
    /// 測定値の総数（表示番号の付いた二点間測長・角度）。
    measurement_count: usize,
    app: &'static str,
    app_version: &'static str,
}

impl ResultMeta {
    /// CSV 先頭のメタデータ行（キーと値の列）。
    fn csv_rows(&self) -> Vec<(String, String)> {
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        vec![
            ("filename".to_owned(), self.filename.clone()),
            ("exported_at".to_owned(), self.exported_at.clone()),
            (
                "scale_nm_per_px".to_owned(),
                self.scale_nm_per_px.map(|v| v.to_string()).unwrap_or_default(),
            ),
            ("image_width".to_owned(), self.image_width.to_string()),
            ("image_height".to_owned(), self.image_height.to_string()),
            ("bit_depth".to_owned(), self.bit_depth.to_string()),
            ("image_extent".to_owned(), opt(&self.image_extent)),
            ("unit".to_owned(), self.unit.clone()),
            ("measurement_count".to_owned(), self.measurement_count.to_string()),
            ("app".to_owned(), self.app.to_owned()),
            ("app_version".to_owned(), self.app_version.to_owned()),
        ]
    }
}

fn result_meta(img_path: &Path, frame: &Frame, measurement_count: usize) -> ResultMeta {
    let filename = img_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned());
    // スケールはパイプラインを前方へ伝播するので、結果出力コマンドの
    // フレームのスケールは各測長コマンドのものと一致する。
    let unit = frame
        .scale
        .map(|s| s.unit.label().to_owned())
        .unwrap_or_else(|| "px".to_owned());
    let image_extent = frame.scale.map(|s| {
        let (u, w, h) = s.extent(frame.image.width, frame.image.height);
        format!("{} x {} {}", w, h, u.label())
    });
    ResultMeta {
        filename,
        exported_at: chrono::Local::now().to_rfc3339(),
        scale_nm_per_px: frame.scale.map(|s| s.nm_per_px),
        image_width: frame.image.width,
        image_height: frame.image.height,
        bit_depth: frame.image.depth,
        image_extent,
        unit,
        measurement_count,
        app: env!("CARGO_PKG_NAME"),
        app_version: env!("CARGO_PKG_VERSION"),
    }
}

/// 結果ファイルの表部分の 1 行（測定値 1 つ。二点間測長・角度のみ）。
struct ResultRow {
    /// 所属グループの名前。
    group: String,
    /// 結果リストの表示番号（# の後ろに続く番号）。
    number: Option<usize>,
    /// 測定タイプ（グループの種類）。
    kind: ToolKind,
    /// 測定値。距離はスケール換算した長さ、角度は °。
    value: f64,
    /// 値の単位。距離はスケールの単位（未設定なら "px"）、角度は "°"。
    unit: String,
}

/// 表示番号の付いた測定値（二点間測長・角度）を、グループ順 → 番号順に集める。
fn result_rows(doc: &Document, frame: &Frame) -> Vec<ResultRow> {
    let mut rows = Vec::new();
    for overlay in doc.measure_overlays(&frame.image, None) {
        let (data, computed) = (overlay.data, &overlay.computed);
        for g in &data.groups {
            for tid in data.group_tools(g.id) {
                let Some(t) = computed.by_id(tid) else {
                    continue;
                };
                let (value, unit) = match t.kind {
                    ToolKind::Angle => {
                        let Some(deg) = t.angle_deg else { continue };
                        (deg, "°".to_owned())
                    }
                    _ => {
                        let Some(len) = t.length_px else { continue };
                        let value = overlay
                            .scale
                            .map(|s| len * s.per_px())
                            .unwrap_or(len);
                        let unit = overlay
                            .scale
                            .map(|s| s.unit.label().to_owned())
                            .unwrap_or_else(|| "px".to_owned());
                        (value, unit)
                    }
                };
                rows.push(ResultRow {
                    group: g.name.clone(),
                    number: computed.number(tid),
                    kind: g.kind(),
                    value,
                    unit,
                });
            }
        }
    }
    rows
}

/// CSV の 1 フィールドをエスケープする。コンマ・引用符・改行を含むときは
/// 引用符で囲み、中の引用符は 2 重にする（RFC 4180）。
fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// グループの値の統計量。`values` が空のときは `Null` を返す。
/// sigma は標本標準偏差（n-1）。3sigma は sigma の 3 倍。
fn group_stats(values: &[f64]) -> serde_json::Value {
    let n = values.len();
    if n == 0 {
        return serde_json::Value::Null;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let sigma = if n >= 2 {
        let var = values
            .iter()
            .map(|v| (v - mean).powi(2))
            .sum::<f64>()
            / (n - 1) as f64;
        Some(var.sqrt())
    } else {
        None
    };
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) * 0.5
    };
    serde_json::json!({
        "mean": mean,
        "sigma": sigma,
        "3sigma": sigma.map(|s| s * 3.0),
        "median": median,
    })
}

/// 測定結果をまとめて JSON で保存する。`index` は結果出力コマンドの位置。
/// 同じフレームに効いている測長コマンド（画像の `Arc` が一致するもの）を
/// パイプライン順に集め、グループを連結した 1 つの JSON を書き出す。
/// 出力先は `output` テンプレート（`{dir}` / `{filename}` は画像パスから
/// 解決）。空文字列のときは保存しない（Ok(None)）。戻り値は実際に保存したパス。
pub fn save_result_json(doc: &Document, index: usize) -> Result<Option<PathBuf>, String> {
    let Some((path, img_path, frame)) = result_target(doc, index)? else {
        return Ok(None);
    };
    let mut groups: Vec<serde_json::Value> = Vec::new();
    for overlay in doc.measure_overlays(&frame.image, None) {
        let (data, computed) = (overlay.data, &overlay.computed);
        for g in &data.groups {
            // 表示用の丸めはせず、元の精度の数値のまま保存する。
            // 値は種類（type）ごとに変わる: 距離はスケール換算した長さ、
            // 角度はスケール換算しない °。いずれも values に並べる。
            let values: Vec<f64> = data
                .group_tools(g.id)
                .iter()
                .filter_map(|tid| {
                    let t = computed.by_id(*tid)?;
                    match t.kind {
                        ToolKind::Angle => t.angle_deg,
                        _ => t
                            .length_px
                            .map(|l| overlay.scale.map(|s| l * s.per_px()).unwrap_or(l)),
                    }
                })
                .collect();
            let type_name = match g.kind() {
                ToolKind::Distance => "distance",
                ToolKind::Angle => "angle",
                ToolKind::Boundary => "boundary",
                ToolKind::Offset => "offset",
            };
            let stats = group_stats(&values);
            groups.push(serde_json::json!({
                "name": g.name,
                "type": type_name,
                "values": values,
                "stats": stats,
            }));
        }
    }
    // 測定値の総数（CSV の表の行数と同じ）。
    let count = groups
        .iter()
        .map(|g| g["values"].as_array().map_or(0, |v| v.len()))
        .sum();
    let meta = result_meta(&img_path, frame, count);
    let json = serde_json::json!({
        "filename": meta.filename,
        "exported_at": meta.exported_at,
        "scale_nm_per_px": meta.scale_nm_per_px,
        "image_width": meta.image_width,
        "image_height": meta.image_height,
        "bit_depth": meta.bit_depth,
        "image_extent": meta.image_extent,
        "unit": meta.unit,
        "measurement_count": meta.measurement_count,
        "app": meta.app,
        "app_version": meta.app_version,
        "groups": groups,
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| {
        t!(
            "cmd.cannot_save",
            path = path.to_string_lossy(),
            error = format!("{e}")
        )
        .into_owned()
    })?;
    Ok(Some(path))
}

/// 測定結果をまとめて CSV で保存する。`index` は結果出力コマンドの位置。
/// 先頭にメタデータ行（キー, 値）、空行のあとに表
/// （`group,number,type,value,unit` の 1 行 1 測定値）が続く。
/// 値は表示用の丸めをしない。空文字列のときは保存しない（Ok(None)）。
pub fn save_result_csv(doc: &Document, index: usize) -> Result<Option<PathBuf>, String> {
    let Some((path, img_path, frame)) = result_target(doc, index)? else {
        return Ok(None);
    };
    let rows = result_rows(doc, frame);
    let meta = result_meta(&img_path, frame, rows.len());

    let mut out = String::new();
    // Excel で UTF-8 のグループ名を正しく読めるよう BOM を付ける。
    out.push('\u{FEFF}');
    for (key, value) in meta.csv_rows() {
        out.push_str(&csv_escape(&key));
        out.push(',');
        out.push_str(&csv_escape(&value));
        out.push('\n');
    }
    out.push('\n');
    out.push_str("group,number,type,value,unit\n");
    for row in &rows {
        let type_name = match row.kind {
            ToolKind::Distance => "distance",
            ToolKind::Angle => "angle",
            ToolKind::Boundary => "boundary",
            ToolKind::Offset => "offset",
        };
        let fields = [
            csv_escape(&row.group),
            row.number.map(|n| n.to_string()).unwrap_or_default(),
            type_name.to_owned(),
            row.value.to_string(),
            csv_escape(&row.unit),
        ];
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| {
        t!(
            "cmd.cannot_save",
            path = path.to_string_lossy(),
            error = format!("{e}")
        )
        .into_owned()
    })?;
    Ok(Some(path))
}

/// `{dir}` / `{filename}` を画像パスから解決する（画像出力コマンドでも共用）。
pub(crate) fn resolve_output_path(template: &str, img_path: &Path) -> PathBuf {
    let dir = img_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".".to_owned());
    let stem = img_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            img_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".to_owned())
        });
    PathBuf::from(template.replace("{dir}", &dir).replace("{filename}", &stem))
}

/// 測長オーバーレイをまとめて描く（`render` / `render_rgb` 共通部分）。
/// 各描画関数の where 節（`AnnotationPixel` の制約は伝播しないため明示）。
fn draw_overlays<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    src: &Gray16,
    overlays: &[(&ComputedMeasure, Option<Scale>)],
    annotation_scale: f32,
    digits: u8,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let f = base_scale(src) * annotation_scale.clamp(0.25, 8.0);
    let font = font();
    for (computed, scale) in overlays {
        draw_computed(img, computed, *scale, f, font.as_ref(), digits);
    }
}

/// アノテーションの基準倍率。画面では線幅 2px・文字 12px が
/// 画像 1px = 画面 1px のときの見た目なので、出力では画像の解像度に
/// 応じて自動で大きくし、どの解像度でも見た目の大きさが揃うようにする。
fn base_scale(img: &Gray16) -> f32 {
    (img.width.max(img.height) as f32 / 1024.0).clamp(0.5, 6.0)
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
    digits: u8,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    for t in computed.tools.iter() {
        // フィッティング領域の枠（点線）。符号固定モードは辺ごとに明暗を
        // 付けるので、画面と同じ `region_edge_colors` で 1 辺ずつ描く。
        for region in &t.fit_regions {
            let poly = region.corners();
            let colors = region_edge_colors(region.mode, region.sign);
            for i in 0..4 {
                draw_dashed_polyline(
                    img,
                    &[poly[i], poly[(i + 1) % 4]],
                    f,
                    P::from_screen(colors[i]),
                );
            }
        }
        match t.kind {
            ToolKind::Distance => {
                draw_line_and_arrows(img, t.p1, t.p2, f, P::from_screen(COLOR_DISTANCE));
                if let Some(len) = t.length_px {
                    // 番号は結果リストと同じ通し番号（補助線には振らない）。
                    let text = match computed.number(t.id) {
                        Some(n) => format!("#{n} {}", format_measurement(len, scale, digits)),
                        None => format_measurement(len, scale, digits),
                    };
                    draw_value_label(
                        img,
                        font,
                        t.p1,
                        t.p2,
                        text,
                        P::from_screen(COLOR_DISTANCE),
                        f,
                    );
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
                    // 補助線は結果リストに出ないので番号は付けない。
                    let sign = if d >= 0.0 { "+" } else { "-" };
                    let text = format!("{sign}{}", format_measurement(d.abs(), scale, digits));
                    draw_value_label(img, font, t.p1, t.p2, text, guide, f);
                }
            }
            ToolKind::Angle => {
                // 頂点 p2 を共有する 2 線分。なす角を二等分線方向に表示する
                // （画面版と同じ）。
                let Some(p3) = t.p3 else { continue };
                let color = P::from_screen(COLOR_DISTANCE);
                draw_thick_line(img, t.p1, t.p2, (2.0 * f).max(1.0), color);
                draw_thick_line(img, t.p2, p3, (2.0 * f).max(1.0), color);
                if let Some(deg) = t.angle_deg {
                    let text = match computed.number(t.id) {
                        Some(n) => format!("#{n} {}", format_angle(deg, digits)),
                        None => format_angle(deg, digits),
                    };
                    draw_angle_label(img, font, (t.p1, t.p2, p3), text, color, f);
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

/// 線の中点から少し浮かせて測定値を描く（画面の draw_value_label より少し離す）。
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
    let anchor = mid + n * 13.0 * f as f64;
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

/// 角度のラベル。頂点から二等分線方向へ少し浮かせて描く（画面版と同じ位置）。
/// `pts` は (1 点目, 頂点, 3 点目)。
fn draw_angle_label<P>(
    img: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    font: Option<&FontArc>,
    pts: (Pt2, Pt2, Pt2),
    text: String,
    color: P,
    f: f32,
) where
    P: AnnotationPixel,
    P::Subpixel: Into<f32> + Clamp<f32>,
{
    let (a, v, b) = pts;
    let (u1, u2) = (a - v, b - v);
    let bisect = {
        let d = u1 / u1.length() + u2 / u2.length();
        // 180° で二等分方向が打ち消し合うときは上方向にする。
        if d.length() > 1e-6 {
            d / d.length()
        } else {
            Pt2::new(0.0, -1.0)
        }
    };
    let anchor = v + bisect * 16.0 * f as f64;
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
fn fill_polygon<P>(img: &mut ImageBuffer<P, Vec<P::Subpixel>>, pts: &[Pt2], color: P)
where
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
    use crate::command::{Command, ResultFormat};
    use crate::document::{Document, SourceCache};
    use crate::frame::LengthUnit;
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

    /// 二点間測長の線が画像へ描かれること（ラベルと重ならない線上の点が
    /// アノテーション色になる）。
    #[test]
    fn distance_line_is_drawn() {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement(ToolKind::Distance);
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
        let out = render(&img, &[(&computed, None)], 1.0, 5);
        let line = gray_of_line(COLOR_DISTANCE);
        assert_eq!(out.get_pixel(60, 100).0[0], line, "ラベル外の線上の点");
        assert_eq!(out.get_pixel(100, 0).0[0], 0, "線から離れた画素は元のまま");
    }

    /// 矢印頭が画面と同じ向きに開くこと（羽は線の内側へ折り返す）。
    #[test]
    fn arrow_head_matches_screen_direction() {
        // base_scale は長辺 1024px で 1.0 になるので、f=1.0 の幾何で検証できる。
        let img = Gray16::black(1024, 1024);
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement(ToolKind::Distance);
        data.tools.push(MeasureTool::Distance {
            id: 1,
            p1: Pt2::new(100.0, 200.0),
            p2: Pt2::new(300.0, 200.0),
            group: g,
            fit1: Default::default(),
            fit2: Default::default(),
        });
        let computed = data.compute(&img, None);
        let out = render(&img, &[(&computed, None)], 1.0, 5);
        let line = gray_of_line(COLOR_DISTANCE);
        // 端点 2 の矢印: tip=(300,200)、dir=(1,0)（線の内側向き）。
        // tip - rotate(dir, ±25°)*9 で羽は左側・上下に開く:
        // (291.9, 196.2) / (291.9, 203.8)。
        assert_eq!(out.get_pixel(292, 196).0[0], line, "上側の羽");
        assert_eq!(out.get_pixel(292, 204).0[0], line, "下側の羽");
        // 鏡像（線の外側へ開く）だと羽は (308.2, 196.2) / (308.2, 203.8) に来る。
        // この位置はフィッティング領域の枠と重なるので 0 とは限らないが、
        // 少なくとも測長線の色ではないこと（正しい実装なら点線枠か背景）。
        assert_ne!(
            out.get_pixel(308, 196).0[0],
            line,
            "鏡像位置には羽を描かない"
        );
        assert_ne!(
            out.get_pixel(308, 204).0[0],
            line,
            "鏡像位置には羽を描かない"
        );
    }

    /// RGB 出力ではアノテーションの色がそのまま残ること。
    #[test]
    fn rgb_keeps_annotation_colors() {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement(ToolKind::Distance);
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
        let out = render_rgb(&img, &[(&computed, None)], 1.0, 5);
        assert_eq!(
            out.get_pixel(60, 100).0,
            [235, 70, 70],
            "ラベル外の線上の点が赤"
        );
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

    /// 8bit 画像はグレースケール出力でも 8bit のまま保存されること。
    #[test]
    fn save8_keeps_8bit_output() {
        let img = Gray16 {
            width: 2,
            height: 2,
            depth: 8,
            data: vec![0, 100, 200, 255],
        };
        let out = render8(&img, &[], 1.0, 5);
        let dir = std::env::temp_dir().join(format!("nanomeasure_8bit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.png");
        save8(out, &path).expect("保存できる");
        let read = image::open(&path).expect("保存したファイルが読める");
        assert!(
            matches!(read, image::DynamicImage::ImageLuma8(_)),
            "8bit のまま保存される"
        );
        assert_eq!(read.to_luma8().get_pixel(0, 0).0[0], 0);
        assert_eq!(read.to_luma8().get_pixel(1, 1).0[0], 255);
    }

    /// 実画像への描画と tif / png / jpg の保存をまとめて確かめる。
    #[test]
    fn end_to_end_save_smoke() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/nanoparticles_16bit.tif");
        let mut cache = std::collections::HashMap::new();
        let img = crate::command::load_image(&src, &mut cache).expect("testdata が読める");

        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement(ToolKind::Distance);
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
        let out = render(&img, &[(&computed, None)], 1.5, 5);

        let dir = std::env::temp_dir().join(format!("nanomeasure_smoke_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, tolerance) in [("out.tif", 32i32), ("out.png", 32), ("out.jpg", 5000)] {
            let path = dir.join(name);
            save(out.clone(), &path).expect("保存できる");
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
        let rgb = render_rgb(&img, &[(&computed, None)], 1.5, 5);
        let rgb_path = dir.join("out_rgb.png");
        save_rgb(&rgb, &rgb_path).expect("RGB 保存できる");
        let read = image::open(&rgb_path).expect("RGB が読める");
        assert_eq!(
            read.to_rgb8().get_pixel(55, 25).0,
            [235, 70, 70],
            "線の中点が赤"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 3 本の測長を持つグループを作る（save_result_json のテスト用）。
    fn data_with_measurements() -> MeasureData {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement(ToolKind::Distance);
        let fit = crate::measure::FitSettings::default();
        for (id, x1, x2) in [(1, 10.0, 50.0), (2, 20.0, 60.0), (3, 30.0, 70.0)] {
            data.tools.push(MeasureTool::Distance {
                id,
                p1: Pt2::new(x1, 50.0),
                p2: Pt2::new(x2, 50.0),
                group: g,
                fit1: fit,
                fit2: fit,
            });
        }
        data
    }

    /// 結果出力コマンドが測長結果を JSON で保存すること。
    #[test]
    fn save_result_json_writes_filename_first() {
        let dir = std::env::temp_dir().join(format!("nanomeasure_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("sample.tif");
        image::GrayImage::from_raw(4, 4, vec![0u8; 16])
            .unwrap()
            .save(&img_path)
            .unwrap();

        let mut doc = Document::new("sample.tif");
        doc.push_command(Command::InsertImage {
            path: img_path.clone(),
        });
        doc.push_command(Command::Measure {
            data: data_with_measurements(),
        });
        doc.push_command(Command::ExportResult {
            output: "{dir}/{filename}_result.json".to_owned(),
            format: ResultFormat::Json,
        });
        doc.recompute(&mut SourceCache::new());

        let path = save_result_json(&doc, 2).unwrap().expect("保存される");
        assert_eq!(path, dir.join("sample_result.json"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("{\n  \"filename\": \"sample.tif\""),
            "先頭にファイル名: {text}"
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["unit"], "px", "スケール未設定は px");
        assert_eq!(v["scale_nm_per_px"], serde_json::Value::Null);
        assert_eq!(v["image_width"], 4);
        assert_eq!(v["image_height"], 4);
        assert_eq!(v["bit_depth"], 8);
        assert_eq!(v["measurement_count"], 3);
        assert_eq!(v["app"], env!("CARGO_PKG_NAME"));
        assert_eq!(v["app_version"], env!("CARGO_PKG_VERSION"));
        assert!(v["exported_at"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(v["groups"][0]["type"], "distance");
        assert_eq!(v["groups"][0]["values"].as_array().unwrap().len(), 3);
        // 全測定値が同値なら mean = median = 値、sigma = 0。
        let stats = &v["groups"][0]["stats"];
        assert!((stats["mean"].as_f64().unwrap() - 40.0).abs() < 1e-9);
        assert!((stats["sigma"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        assert!((stats["3sigma"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        assert!((stats["median"].as_f64().unwrap() - 40.0).abs() < 1e-9);
        // 表示用の丸めはせず、元の精度の数値で保存される。
        let first = v["groups"][0]["values"][0]
            .as_f64()
            .expect("数値で保存される");
        assert!(
            first.is_finite() && first > 0.0,
            "測定値が数値として保存されている: {first}"
        );

        // 出力先が空なら保存しない。
        doc.commands.get_mut(2).expect("結果出力コマンド").command = Command::ExportResult {
            output: String::new(),
            format: ResultFormat::Json,
        };
        assert!(save_result_json(&doc, 2).unwrap().is_none());

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&img_path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    /// 角度グループの JSON 出力（type: angle / values は空 / angles に角度値）。
    #[test]
    fn save_result_json_outputs_angle_group() {
        let dir =
            std::env::temp_dir().join(format!("nanomeasure_test_angle_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("sample.tif");
        image::GrayImage::from_raw(4, 4, vec![0u8; 16])
            .unwrap()
            .save(&img_path)
            .unwrap();

        let mut data = data_with_measurements();
        // 距離グループがアクティブなので、角度は別グループが作られる。
        let g = data.group_for_new_measurement(ToolKind::Angle);
        data.tools.push(MeasureTool::Angle {
            id: 10,
            p1: Pt2::new(10.0, 0.0),
            p2: Pt2::new(0.0, 0.0),
            p3: Pt2::new(0.0, 10.0),
            group: g,
        });

        let mut doc = Document::new("sample.tif");
        doc.push_command(Command::InsertImage {
            path: img_path.clone(),
        });
        doc.push_command(Command::Measure { data });
        doc.push_command(Command::ExportResult {
            output: "{dir}/{filename}_result.json".to_owned(),
            format: ResultFormat::Json,
        });
        doc.recompute(&mut SourceCache::new());

        let path = save_result_json(&doc, 2).unwrap().expect("保存される");
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        // グループ順: 距離（既存）→ 角度（新規作成）。
        assert_eq!(v["groups"][0]["type"], "distance");
        assert_eq!(v["groups"][1]["type"], "angle");
        // 角度も values に格納（type で判別）。angles キーは無い。
        let values = v["groups"][1]["values"].as_array().unwrap();
        assert_eq!(values.len(), 1);
        assert!((values[0].as_f64().unwrap() - 90.0).abs() < 1e-9);
        assert!(v["groups"][1].get("angles").is_none(), "angles キーは廃止");
        assert_eq!(v["measurement_count"], 4, "距離 3 + 角度 1");

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&img_path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    /// グループ統計量の計算（値のばらつきがある場合）。
    #[test]
    fn group_stats_computes_statistics() {
        // mean = 4, 標本分散 = ((3-4)^2+(4-4)^2+(5-4)^2)/2 = 1, sigma = 1。
        let stats = group_stats(&[3.0, 4.0, 5.0]);
        assert!((stats["mean"].as_f64().unwrap() - 4.0).abs() < 1e-9);
        assert!((stats["sigma"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert!((stats["3sigma"].as_f64().unwrap() - 3.0).abs() < 1e-9);
        assert!((stats["median"].as_f64().unwrap() - 4.0).abs() < 1e-9);
        // 偶数個の中央値は中央 2 つの平均。
        let stats = group_stats(&[1.0, 2.0, 3.0, 4.0]);
        assert!((stats["median"].as_f64().unwrap() - 2.5).abs() < 1e-9);
        // 1 個では標本標準偏差が出ない（null）。
        let stats = group_stats(&[7.0]);
        assert_eq!(stats["mean"].as_f64().unwrap(), 7.0);
        assert!(stats["sigma"].is_null(), "n=1 の sigma は null");
        assert!(stats["3sigma"].is_null(), "n=1 の 3sigma は null");
        assert_eq!(stats["median"].as_f64().unwrap(), 7.0);
        // 空なら stats 自体が null。
        assert!(group_stats(&[]).is_null());
    }

    /// 結果出力コマンドが測定結果を CSV で保存すること
    /// （メタデータ行 → 空行 → group,number,type,value,unit の表）。
    #[test]
    fn save_result_csv_writes_metadata_and_rows() {
        let dir =
            std::env::temp_dir().join(format!("nanomeasure_test_csv_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("sample.tif");
        image::GrayImage::from_raw(4, 4, vec![0u8; 16])
            .unwrap()
            .save(&img_path)
            .unwrap();

        let mut doc = Document::new("sample.tif");
        doc.push_command(Command::InsertImage {
            path: img_path.clone(),
        });
        // スケール設定: 100 px = 10 nm → 1 px = 0.1 nm。
        doc.push_command(Command::SetScale {
            pixels: 100.0,
            length: 10.0,
            unit: LengthUnit::Nanometer,
        });
        doc.push_command(Command::Measure {
            data: data_with_measurements(),
        });
        doc.push_command(Command::ExportResult {
            output: "{dir}/{filename}_result.csv".to_owned(),
            format: ResultFormat::Csv,
        });
        doc.recompute(&mut SourceCache::new());

        let path = save_result_csv(&doc, 3).unwrap().expect("保存される");
        assert_eq!(path, dir.join("sample_result.csv"));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.strip_prefix('\u{FEFF}').unwrap_or(l))
            .collect();
        // 先頭はメタデータ行（BOM 付き）。
        assert_eq!(lines[0], "filename,sample.tif");
        let meta: Vec<(&str, &str)> = lines[..11]
            .iter()
            .map(|l| l.split_once(',').expect("キー,値 の並び: {l}"))
            .collect();
        assert_eq!(meta[0], ("filename", "sample.tif"));
        assert!(meta[1].0 == "exported_at" && !meta[1].1.is_empty(), "出力日時");
        assert_eq!(meta[2], ("scale_nm_per_px", "0.1"));
        assert_eq!(meta[3], ("image_width", "4"));
        assert_eq!(meta[4], ("image_height", "4"));
        assert_eq!(meta[5], ("bit_depth", "8"));
        assert!(meta[6].0 == "image_extent" && meta[6].1.ends_with("nm"));
        assert_eq!(meta[7], ("unit", "nm"));
        assert_eq!(meta[8], ("measurement_count", "3"));
        assert_eq!(meta[9], ("app", env!("CARGO_PKG_NAME")));
        assert_eq!(meta[10], ("app_version", env!("CARGO_PKG_VERSION")));
        // 空行のあとに表のヘッダと 1 行 1 測定値。
        let header = lines.iter().position(|l| l.is_empty()).expect("空行") + 1;
        assert_eq!(lines[header], "group,number,type,value,unit");
        assert_eq!(lines.len(), header + 1 + 3, "3 測定値ぶんの行");
        let row1: Vec<&str> = lines[header + 1].split(',').collect();
        assert_eq!(row1.len(), 5, "5 列: {row1:?}");
        assert!(!row1[0].is_empty(), "グループ名が入る: {row1:?}");
        assert_eq!(row1[1], "1", "# の後ろの番号");
        assert_eq!(row1[2], "distance");
        let value: f64 = row1[3].parse().expect("数値");
        assert!((value - 4.0).abs() < 1e-9, "40 px * 0.1 nm/px = 4.0: {value}");
        assert_eq!(row1[4], "nm");
        let row3: Vec<&str> = lines[header + 3].split(',').collect();
        assert_eq!(row3[1], "3", "通し番号が続く");
        assert!((row3[3].parse::<f64>().unwrap() - 4.0).abs() < 1e-9);

        // 出力先が空なら保存しない。
        doc.commands.get_mut(3).expect("結果出力コマンド").command = Command::ExportResult {
            output: String::new(),
            format: ResultFormat::Csv,
        };
        assert!(save_result_csv(&doc, 3).unwrap().is_none());

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&img_path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
