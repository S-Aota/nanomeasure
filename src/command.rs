//! 画像に対する処理（コマンド）の定義と、その履歴の JSON 表現。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::frame::{Frame, LengthUnit, Scale};
use crate::gray::Gray16;
use crate::measure::MeasureData;

/// コマンドのカテゴリ。処理は `ALL` の並び（入力 → 前処理 → 解析 → 出力）
/// の順に実行される。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandCategory {
    /// 画像を用意する（画像の挿入とスケール設定）。
    Input,
    /// 画像を整える（回転・レベル補正・フィルタなど）。
    Preprocess,
    /// 解析（測長など）。画像は変えない。
    Analysis,
    /// 出力（画像出力など）。画像は変えない。
    Output,
}

impl CommandCategory {
    /// 処理順のカテゴリ列。
    pub const ALL: [Self; 4] = [Self::Input, Self::Preprocess, Self::Analysis, Self::Output];

    /// コマンドリストの見出し。
    pub fn label(self) -> &'static str {
        match self {
            Self::Input => "入力",
            Self::Preprocess => "前処理",
            Self::Analysis => "解析",
            Self::Output => "出力",
        }
    }
}

/// 前処理フィルタの種類とパラメータ。UI の切り替えボタンは [`FilterKind`]
/// を選び、切り替えた種類の既定値で作り直す。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Filter {
    /// ガウシアンぼかし。`sigma` は画素単位の標準偏差。
    GaussianBlur { sigma: f32 },
    /// メディアンフィルタ。一辺 2r+1 の正方形窓の中央値を取る。
    Median { radius: u32 },
    /// アンシャープマスク。`sigma` のぼかしとの差に `amount` を掛けて足し戻す。
    UnsharpMask { sigma: f32, amount: f32 },
}

/// パラメータを除いたフィルタの種類（UI の切り替え用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterKind {
    GaussianBlur,
    Median,
    UnsharpMask,
}

impl Default for Filter {
    fn default() -> Self {
        Self::GaussianBlur { sigma: 1.0 }
    }
}

impl Filter {
    pub fn kind(&self) -> FilterKind {
        match self {
            Self::GaussianBlur { .. } => FilterKind::GaussianBlur,
            Self::Median { .. } => FilterKind::Median,
            Self::UnsharpMask { .. } => FilterKind::UnsharpMask,
        }
    }

    /// 種類を切り替えたときの既定値。
    pub fn default_of(kind: FilterKind) -> Self {
        match kind {
            FilterKind::GaussianBlur => Self::GaussianBlur { sigma: 1.0 },
            FilterKind::Median => Self::Median { radius: 1 },
            FilterKind::UnsharpMask => Self::UnsharpMask {
                sigma: 2.0,
                amount: 1.5,
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::GaussianBlur { sigma } => format!("ガウシアンぼかし: σ = {sigma:.2}"),
            Self::Median { radius } => format!("メディアン: 半径 {radius}"),
            Self::UnsharpMask { sigma, amount } => {
                format!("アンシャープマスク: σ = {sigma:.2}, 強さ = {amount:.2}")
            }
        }
    }

    pub fn apply(&self, img: &Gray16) -> Gray16 {
        match self {
            Self::GaussianBlur { sigma } => img.gaussian_blur(*sigma),
            Self::Median { radius } => img.median_filter(*radius),
            Self::UnsharpMask { sigma, amount } => img.unsharp_mask(*sigma, *amount),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    /// 画像の読み込み。これ自体もコマンド履歴の 1 要素として扱う。
    InsertImage { path: PathBuf },
    /// 画素と実寸法の対応。`pixels` 画素が `length` `unit` に相当する。
    SetScale {
        pixels: f64,
        length: f64,
        unit: LengthUnit,
    },
    /// 画像サイズ固定・等倍のまま中心まわりに回転（時計回り）。
    Rotate { angle_deg: f32 },
    /// in_min..in_max の輝度を 0..65535 へ線形に引き伸ばす。
    Levels { in_min: u16, in_max: u16 },
    /// 前処理フィルタ（ぼかし・メディアン・アンシャープマスク）。
    Filter { filter: Filter },
    /// 測長。画像は変えず、ツール・グループ・フィッティング設定を保持する。
    /// フィッティングと測定値は適用のたびに再計算される。
    Measure { data: MeasureData },
    /// アノテーション付き画像の書き出し。画像は変えず、出力先テンプレートと
    /// アノテーション倍率を保持する。ファイル保存は適用のたびにアプリ側で行う。
    ExportImage {
        /// 出力先テンプレート。`{dir}` / `{filename}` は保存時に画像パスから解決。
        output: String,
        /// アノテーションの倍率（解像度による自動調整に掛ける係数）。
        #[serde(default = "default_annotation_scale")]
        annotation_scale: f32,
        /// カラー（RGB 8bit）で保存するか。既定は 16bit グレースケール
        /// （アノテーションの色は輝度へ落ちる）。
        #[serde(default)]
        color: bool,
    },
    /// 測定結果 JSON の書き出し。画像は変えず、出力先テンプレートを保持する。
    /// ファイル保存は適用のたびにアプリ側で行う（画像出力コマンドと同じ）。
    ExportResult {
        /// 出力先テンプレート。`{dir}` / `{filename}` は保存時に画像パスから解決。
        output: String,
    },
}

fn default_annotation_scale() -> f32 {
    1.0
}

impl Command {
    /// メタデータから読んだ画素サイズを、そのままスケール設定コマンドにする。
    pub fn scale_from(scale: Scale) -> Self {
        Self::SetScale {
            pixels: 1.0,
            length: scale.per_px(),
            unit: scale.unit,
        }
    }

    pub fn label(&self, digits: u8) -> String {
        match self {
            Self::InsertImage { path } => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                format!("画像挿入: {name}")
            }
            Self::SetScale { .. } => match self.scale() {
                Some(scale) => format!("スケール設定: {}", scale.describe(digits)),
                None => "スケール設定: (値が不正)".to_owned(),
            },
            Self::Rotate { angle_deg } => format!("回転: {angle_deg:.2}°"),
            Self::Levels { in_min, in_max } => format!("レベル補正: {in_min} → {in_max}"),
            Self::Filter { filter } => format!("フィルタ: {}", filter.label()),
            Self::Measure { data } => {
                let measurements = data.tools.iter().filter(|t| t.is_measurement()).count();
                format!(
                    "測長: グループ {} 件 / 測定 {} 件",
                    data.groups.len(),
                    measurements
                )
            }
            Self::ExportImage { output, color, .. } => {
                let mode = if *color { "（カラー）" } else { "" };
                format!("画像出力: {output}{mode}")
            }
            Self::ExportResult { output } => format!("結果出力: {output}"),
        }
    }

    /// スケール設定コマンドなら、その内容を画素の実寸法に直したもの。
    /// 画素数が 0 や負のときなど、値として成り立たない場合は `None`。
    /// 単位はコマンドのものがそのまま保持され、表示・測長に使われる。
    pub fn scale(&self) -> Option<Scale> {
        let Self::SetScale {
            pixels,
            length,
            unit,
        } = self
        else {
            return None;
        };
        let nm = length * unit.nm() / pixels;
        (nm.is_finite() && nm > 0.0).then_some(Scale {
            nm_per_px: nm,
            unit: *unit,
        })
    }

    /// このコマンドが属するカテゴリ（処理順を決める）。
    pub fn category(&self) -> CommandCategory {
        match self {
            Self::InsertImage { .. } | Self::SetScale { .. } => CommandCategory::Input,
            Self::Rotate { .. } | Self::Levels { .. } | Self::Filter { .. } => {
                CommandCategory::Preprocess
            }
            Self::Measure { .. } => CommandCategory::Analysis,
            Self::ExportImage { .. } | Self::ExportResult { .. } => CommandCategory::Output,
        }
    }

    /// 入力画像を必要とするか（`InsertImage` だけが入力なしで動く）。
    pub fn is_source(&self) -> bool {
        matches!(self, Self::InsertImage { .. })
    }

    /// このコマンドを適用する。`cache` は同じファイルの再デコードを避けるためのもの。
    pub fn apply(
        &self,
        input: Option<&Frame>,
        cache: &mut HashMap<PathBuf, Arc<Gray16>>,
    ) -> Result<Frame, String> {
        match self {
            Self::InsertImage { path } => Ok(Frame::new(load_image(path, cache)?)),
            Self::SetScale { .. } => {
                let frame = require_input(input)?;
                let scale = self
                    .scale()
                    .ok_or_else(|| "画素数と実寸法には正の値を入れてください".to_owned())?;
                Ok(frame.with_scale(scale))
            }
            Self::Rotate { angle_deg } => {
                let frame = require_input(input)?;
                Ok(Frame {
                    image: Arc::new(frame.image.rotate(*angle_deg)),
                    scale: frame.scale,
                })
            }
            Self::Levels { in_min, in_max } => {
                let frame = require_input(input)?;
                Ok(Frame {
                    image: Arc::new(frame.image.apply_levels(*in_min, *in_max)),
                    scale: frame.scale,
                })
            }
            Self::Filter { filter } => {
                let frame = require_input(input)?;
                Ok(Frame {
                    image: Arc::new(filter.apply(&frame.image)),
                    scale: frame.scale,
                })
            }
            // 測長・画像出力・結果出力は画像を変えない素通しコマンド。
            // オーバーレイと測定値はアプリ側で MeasureData::compute により
            // 毎回再計算される。
            Self::Measure { .. } | Self::ExportImage { .. } | Self::ExportResult { .. } => {
                Ok(require_input(input)?.clone())
            }
        }
    }
}

fn require_input(input: Option<&Frame>) -> Result<&Frame, String> {
    input.ok_or_else(|| "入力画像がありません（先に画像を挿入してください）".to_owned())
}

pub fn load_image(
    path: &Path,
    cache: &mut HashMap<PathBuf, Arc<Gray16>>,
) -> Result<Arc<Gray16>, String> {
    if let Some(img) = cache.get(path) {
        return Ok(img.clone());
    }
    let dynamic = image::open(path)
        .map_err(|e| format!("{} を読み込めません: {e}", path.to_string_lossy()))?;
    let img = Arc::new(Gray16::from_dynamic(&dynamic));
    cache.insert(path.to_path_buf(), img.clone());
    Ok(img)
}

/// リスト 1 行分。`enabled` を外すとパイプラインから一時的に除外される。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandItem {
    pub enabled: bool,
    pub command: Command,
}

impl CommandItem {
    pub fn new(command: Command) -> Self {
        Self {
            enabled: true,
            command,
        }
    }
}

/// `.json` に書き出すコマンド履歴。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryFile {
    pub app: String,
    pub commands: Vec<CommandItem>,
}

impl HistoryFile {
    pub fn new(commands: Vec<CommandItem>) -> Self {
        Self {
            app: "tem_measure".to_owned(),
            commands,
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json)
            .map_err(|e| format!("{} に保存できません: {e}", path.to_string_lossy()))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{} を読み込めません: {e}", path.to_string_lossy()))?;
        let file: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{} の解析に失敗しました: {e}", path.to_string_lossy()))?;
        Ok(file)
    }

    /// 別画像へ使い回すため、先頭の画像挿入コマンドを取り除いた処理列を返す。
    pub fn processing_only(&self) -> Vec<CommandItem> {
        self.commands
            .iter()
            .filter(|c| !c.command.is_source())
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_command_converts_pixel_length_pair() {
        // 512 px = 47.9 nm というスケールバー読み取り相当の入力。
        let cmd = Command::SetScale {
            pixels: 512.0,
            length: 47.9,
            unit: LengthUnit::Nanometer,
        };
        let scale = cmd.scale().expect("正の値なので換算できること");
        assert!((scale.nm_per_px - 47.9 / 512.0).abs() < 1e-12);
        assert_eq!(scale.unit, LengthUnit::Nanometer);
    }

    #[test]
    fn scale_command_rejects_zero_pixels() {
        let cmd = Command::SetScale {
            pixels: 0.0,
            length: 1.0,
            unit: LengthUnit::Nanometer,
        };
        assert!(cmd.scale().is_none());
    }

    #[test]
    fn commands_map_to_categories() {
        let insert = Command::InsertImage {
            path: "a.tif".into(),
        };
        assert_eq!(insert.category(), CommandCategory::Input);
        let scale = Command::SetScale {
            pixels: 1.0,
            length: 2.0,
            unit: LengthUnit::Nanometer,
        };
        assert_eq!(scale.category(), CommandCategory::Input);
        let rotate = Command::Rotate { angle_deg: 90.0 };
        assert_eq!(rotate.category(), CommandCategory::Preprocess);
        let levels = Command::Levels {
            in_min: 0,
            in_max: 100,
        };
        assert_eq!(levels.category(), CommandCategory::Preprocess);
        let measure = Command::Measure {
            data: MeasureData::default(),
        };
        assert_eq!(measure.category(), CommandCategory::Analysis);
        let export = Command::ExportImage {
            output: "a.png".into(),
            annotation_scale: 1.0,
            color: false,
        };
        assert_eq!(export.category(), CommandCategory::Output);
        let result = Command::ExportResult {
            output: "a.json".into(),
        };
        assert_eq!(result.category(), CommandCategory::Output);
        let filter = Command::Filter {
            filter: Filter::GaussianBlur { sigma: 1.0 },
        };
        assert_eq!(filter.category(), CommandCategory::Preprocess);
    }

    /// フィルタコマンドの JSON ラウンドトリップ（種類のタグが付くこと）。
    #[test]
    fn filter_command_round_trips() {
        for filter in [
            Filter::GaussianBlur { sigma: 1.5 },
            Filter::Median { radius: 2 },
            Filter::UnsharpMask {
                sigma: 2.0,
                amount: 1.5,
            },
        ] {
            let cmd = Command::Filter { filter };
            let json = serde_json::to_string(&cmd).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cmd, "JSON: {json}");
        }
    }

    /// メタデータ由来のスケールが、そのまま読める単位のコマンドになること。
    #[test]
    fn scale_from_metadata_round_trips() {
        let scale = Scale::new(0.093517);
        let cmd = Command::scale_from(scale);
        let back = cmd.scale().expect("換算できること");
        assert!((back.nm_per_px - scale.nm_per_px).abs() < 1e-12);
        assert_eq!(cmd.label(5), "スケール設定: 1 px = 0.09352 nm");
    }

    /// 測長コマンドの無い古い履歴はそのまま読めること。version キーは
    /// リリース前に廃止したので、あっても未知のキーとして無視される。
    #[test]
    fn history_without_measure_commands_still_loads() {
        let json = r#"{
            "app": "tem_measure",
            "version": 3,
            "commands": [
                {"enabled": true, "command": {"type": "InsertImage", "path": "a.tif"}},
                {"enabled": true, "command": {"type": "SetScale", "pixels": 1.0, "length": 0.093517, "unit": "nm"}}
            ]
        }"#;
        let file: HistoryFile = serde_json::from_str(json).expect("version キー付きでも読める");
        assert_eq!(file.commands.len(), 2);
        assert!(file.processing_only().len() == 1, "画像挿入だけが除かれる");
    }

    /// 測長コマンドを含む履歴の JSON ラウンドトリップ。
    #[test]
    fn measure_command_round_trips() {
        let mut data = MeasureData::default();
        data.group_for_new_measurement();
        let cmd = Command::Measure { data: data.clone() };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmd);
        let Command::Measure { data: d } = back else {
            panic!("Measure に戻る");
        };
        assert_eq!(d, data);
    }

    /// 画像出力コマンドの JSON ラウンドトリップ。annotation_scale / color が
    /// 無い JSON は既定値（1.0 / グレー）で読めること。
    #[test]
    fn export_command_round_trips() {
        let cmd = Command::ExportImage {
            output: "{dir}/{filename}_result.jpg".to_owned(),
            annotation_scale: 2.0,
            color: true,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmd);
        let missing: Command =
            serde_json::from_str(r#"{"type":"ExportImage","output":"a.png"}"#).unwrap();
        assert_eq!(
            missing,
            Command::ExportImage {
                output: "a.png".to_owned(),
                annotation_scale: 1.0,
                color: false,
            }
        );
    }

    /// 結果出力コマンドの JSON ラウンドトリップ。
    #[test]
    fn export_result_command_round_trips() {
        let cmd = Command::ExportResult {
            output: "{dir}/{filename}_result.json".to_owned(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmd);
    }
}
