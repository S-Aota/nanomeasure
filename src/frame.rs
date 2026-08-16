//! パイプラインを流れる値と、その寸法スケール。
//!
//! コマンドは「画像 + スケール」の組を受け取って同じ組を返す。スケールを
//! 画像と一緒に持ち回すことで、スケール設定コマンドの位置に応じて
//! 以降の処理・表示に効くようになる。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::gray::Gray16;

#[derive(Clone)]
pub struct Frame {
    pub image: Arc<Gray16>,
    pub scale: Scale,
}

impl Frame {
    pub fn new(image: Arc<Gray16>) -> Self {
        Self {
            image,
            scale: Scale::default(),
        }
    }

    /// 画像はそのままに、スケールだけ差し替えた組を返す。
    pub fn with_scale(&self, scale: Scale) -> Self {
        Self {
            image: self.image.clone(),
            scale,
        }
    }
}

/// 1 画素あたりの実寸法。x と y を別々に持つ（矩形画素の装置があるため）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub nm_per_px_x: f64,
    pub nm_per_px_y: f64,
}

impl Default for Scale {
    /// スケール未設定のときは 1 px = 1 nm とみなす。
    fn default() -> Self {
        Self {
            nm_per_px_x: 1.0,
            nm_per_px_y: 1.0,
        }
    }
}

impl Scale {
    pub fn new(nm_per_px_x: f64, nm_per_px_y: f64) -> Self {
        Self {
            nm_per_px_x,
            nm_per_px_y,
        }
    }

    /// 1 画素の大きさを表示するための単位と、その単位での値。
    pub fn pixel_size(&self) -> (LengthUnit, f64, f64) {
        let unit = LengthUnit::best_for(self.nm_per_px_x.max(self.nm_per_px_y));
        (
            unit,
            self.nm_per_px_x / unit.nm(),
            self.nm_per_px_y / unit.nm(),
        )
    }

    /// 画像全体の実寸法。1 画素とは桁が違うので、単位は別に選ぶ。
    pub fn extent(&self, width: u32, height: u32) -> (LengthUnit, f64, f64) {
        let w = width as f64 * self.nm_per_px_x;
        let h = height as f64 * self.nm_per_px_y;
        let unit = LengthUnit::best_for(w.max(h));
        (unit, w / unit.nm(), h / unit.nm())
    }

    pub fn describe(&self) -> String {
        let (unit, x, y) = self.pixel_size();
        if (self.nm_per_px_x - self.nm_per_px_y).abs() < f64::EPSILON {
            format!("1 px = {} {}", format_length(x), unit.label())
        } else {
            format!(
                "1 px = {} × {} {}",
                format_length(x),
                format_length(y),
                unit.label()
            )
        }
    }
}

/// 末尾の余計な 0 を落として長さを文字列化する。
pub fn format_length(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LengthUnit {
    #[serde(rename = "pm")]
    Picometer,
    #[serde(rename = "A")]
    Angstrom,
    #[default]
    #[serde(rename = "nm")]
    Nanometer,
    #[serde(rename = "um")]
    Micrometer,
    #[serde(rename = "mm")]
    Millimeter,
}

impl LengthUnit {
    pub const ALL: [Self; 5] = [
        Self::Picometer,
        Self::Angstrom,
        Self::Nanometer,
        Self::Micrometer,
        Self::Millimeter,
    ];

    /// この単位 1 つが何 nm か。
    pub fn nm(self) -> f64 {
        match self {
            Self::Picometer => 1e-3,
            Self::Angstrom => 0.1,
            Self::Nanometer => 1.0,
            Self::Micrometer => 1e3,
            Self::Millimeter => 1e6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Picometer => "pm",
            Self::Angstrom => "Å",
            Self::Nanometer => "nm",
            Self::Micrometer => "µm",
            Self::Millimeter => "mm",
        }
    }

    /// 桁が読みやすくなる単位を選ぶ。TEM では nm が慣習なので広めに nm を使い、
    /// Å は自動選択しない（スケール設定ダイアログで手動選択はできる）。
    pub fn best_for(nm: f64) -> Self {
        let nm = nm.abs();
        if nm == 0.0 {
            Self::Nanometer
        } else if nm < 1e-3 {
            Self::Picometer
        } else if nm < 1000.0 {
            Self::Nanometer
        } else if nm < 1e6 {
            Self::Micrometer
        } else {
            Self::Millimeter
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TEM で使う 0.01〜100 nm/px あたりは nm のまま出したい。
    #[test]
    fn best_unit_prefers_nanometers() {
        assert_eq!(LengthUnit::best_for(0.0935), LengthUnit::Nanometer);
        assert_eq!(LengthUnit::best_for(0.027), LengthUnit::Nanometer);
        assert_eq!(LengthUnit::best_for(0.5), LengthUnit::Nanometer);
        assert_eq!(LengthUnit::best_for(2500.0), LengthUnit::Micrometer);
        assert_eq!(LengthUnit::best_for(1e-5), LengthUnit::Picometer);
    }

    #[test]
    fn describe_collapses_equal_axes() {
        assert_eq!(Scale::new(0.5, 0.5).describe(), "1 px = 0.5 nm");
        assert_eq!(
            Scale::new(0.093517, 0.093517).describe(),
            "1 px = 0.09352 nm"
        );
        assert!(Scale::new(0.5, 0.48).describe().contains('×'));
    }

    /// 1 画素と画像全体は桁が違うので、それぞれに合う単位が選ばれること。
    #[test]
    fn extent_picks_its_own_unit() {
        let scale = Scale::new(0.093517, 0.093517);
        assert_eq!(scale.pixel_size().0, LengthUnit::Nanometer);
        let (unit, w, _) = scale.extent(1024, 1024);
        assert_eq!(unit, LengthUnit::Nanometer);
        assert!((w - 95.761).abs() < 1e-2, "{w}");
    }
}
