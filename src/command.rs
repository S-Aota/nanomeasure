//! 画像に対する処理（コマンド）の定義と、その履歴の JSON 表現。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::frame::{Frame, LengthUnit, Scale};
use crate::gray::Gray16;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    /// 画像の読み込み。これ自体もコマンド履歴の 1 要素として扱う。
    InsertImage { path: PathBuf },
    /// 画素と実寸法の対応。`x_pixels` 画素が `x_length` `unit` に相当する。
    SetScale {
        x_pixels: f64,
        x_length: f64,
        y_pixels: f64,
        y_length: f64,
        unit: LengthUnit,
    },
    /// 画像サイズ固定・等倍のまま中心まわりに回転（時計回り）。
    Rotate { angle_deg: f32 },
    /// in_min..in_max の輝度を 0..65535 へ線形に引き伸ばす。
    Levels { in_min: u16, in_max: u16 },
}

impl Command {
    /// メタデータから読んだ画素サイズを、そのままスケール設定コマンドにする。
    pub fn scale_from(scale: Scale) -> Self {
        let unit = LengthUnit::best_for(scale.nm_per_px_x);
        Self::SetScale {
            x_pixels: 1.0,
            x_length: scale.nm_per_px_x / unit.nm(),
            y_pixels: 1.0,
            y_length: scale.nm_per_px_y / unit.nm(),
            unit,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::InsertImage { path } => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                format!("画像挿入: {name}")
            }
            Self::SetScale { .. } => match self.scale() {
                Some(scale) => format!("スケール設定: {}", scale.describe()),
                None => "スケール設定: (値が不正)".to_owned(),
            },
            Self::Rotate { angle_deg } => format!("回転: {angle_deg:.2}°"),
            Self::Levels { in_min, in_max } => format!("レベル補正: {in_min} → {in_max}"),
        }
    }

    /// スケール設定コマンドなら、その内容を nm/px に直したもの。
    /// 画素数が 0 や負のときなど、値として成り立たない場合は `None`。
    pub fn scale(&self) -> Option<Scale> {
        let Self::SetScale {
            x_pixels,
            x_length,
            y_pixels,
            y_length,
            unit,
        } = self
        else {
            return None;
        };
        let x = x_length * unit.nm() / x_pixels;
        let y = y_length * unit.nm() / y_pixels;
        (x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0).then(|| Scale::new(x, y))
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

/// スケールをコマンドとして持つようになった版が 2。
pub const HISTORY_FORMAT_VERSION: u32 = 2;

/// `.json` に書き出すコマンド履歴。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryFile {
    pub app: String,
    pub version: u32,
    pub commands: Vec<CommandItem>,
}

impl HistoryFile {
    pub fn new(commands: Vec<CommandItem>) -> Self {
        Self {
            app: "tem_measure".to_owned(),
            version: HISTORY_FORMAT_VERSION,
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
        if file.version > HISTORY_FORMAT_VERSION {
            return Err(format!(
                "この履歴はより新しい形式です (version {})",
                file.version
            ));
        }
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
    fn scale_command_converts_pixel_length_pairs() {
        // 512 px = 47.9 nm というスケールバー読み取り相当の入力。
        let cmd = Command::SetScale {
            x_pixels: 512.0,
            x_length: 47.9,
            y_pixels: 512.0,
            y_length: 47.9,
            unit: LengthUnit::Nanometer,
        };
        let scale = cmd.scale().expect("正の値なので換算できること");
        assert!((scale.nm_per_px_x - 47.9 / 512.0).abs() < 1e-12);
    }

    #[test]
    fn scale_command_rejects_zero_pixels() {
        let cmd = Command::SetScale {
            x_pixels: 0.0,
            x_length: 1.0,
            y_pixels: 1.0,
            y_length: 1.0,
            unit: LengthUnit::Nanometer,
        };
        assert!(cmd.scale().is_none());
    }

    /// メタデータ由来のスケールが、そのまま読める単位のコマンドになること。
    #[test]
    fn scale_from_metadata_round_trips() {
        let scale = Scale::new(0.093517, 0.093517);
        let cmd = Command::scale_from(scale);
        let back = cmd.scale().expect("換算できること");
        assert!((back.nm_per_px_x - scale.nm_per_px_x).abs() < 1e-12);
        assert_eq!(cmd.label(), "スケール設定: 1 px = 0.09352 nm");
    }
}
