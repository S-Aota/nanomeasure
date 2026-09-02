//! パイプラインを流れる値と、その寸法スケール。
//!
//! コマンドは「画像 + スケール」の組を受け取って同じ組を返す。スケールを
//! 画像と一緒に持ち回すことで、スケール設定コマンドの位置に応じて
//! 以降の処理・表示に効くようになる。スケール未設定（メタデータに
//! 画素サイズが無い等）は `None` で表し、その画像は画素単位でしか
//! 実寸法を出せない。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::gray::Gray16;

#[derive(Clone)]
pub struct Frame {
    pub image: Arc<Gray16>,
    /// このフレームに効いているスケール。未設定なら `None`。
    pub scale: Option<Scale>,
}

impl Frame {
    pub fn new(image: Arc<Gray16>) -> Self {
        Self { image, scale: None }
    }

    /// 画像はそのままに、スケールだけ差し替えた組を返す。
    pub fn with_scale(&self, scale: Scale) -> Self {
        Self {
            image: self.image.clone(),
            scale: Some(scale),
        }
    }
}

/// 1 画素あたりの実寸法と、その表示・入力に使う単位。
/// x と y は同一（正方画素）として 1 つで持つ。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    /// 1 画素の実寸法（nm）。
    pub nm_per_px: f64,
    /// `nm_per_px` を読みやすい形に直したときの単位。ユーザーが
    /// スケール設定ダイアログで選んだ単位がそのまま保持される。
    pub unit: LengthUnit,
}

impl Scale {
    /// nm/px から作る。単位は桁に合うものを自動で選ぶ。
    pub fn new(nm_per_px: f64) -> Self {
        Self {
            nm_per_px,
            unit: LengthUnit::best_for(nm_per_px),
        }
    }

    /// 1 画素の実寸法を、保持している単位で表した値。
    pub fn per_px(&self) -> f64 {
        self.nm_per_px / self.unit.nm()
    }

    /// 画像全体の実寸法。1 画素とは桁が違うので、単位は別に選ぶ。
    pub fn extent(&self, width: u32, height: u32) -> (LengthUnit, f64, f64) {
        let w = width as f64 * self.nm_per_px;
        let h = height as f64 * self.nm_per_px;
        let unit = LengthUnit::best_for(w.max(h));
        (unit, w / unit.nm(), h / unit.nm())
    }

    pub fn describe(&self) -> String {
        format!(
            "1 px = {} {}",
            format_length(self.per_px()),
            self.unit.label()
        )
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
    fn describe_uses_held_unit() {
        assert_eq!(Scale::new(0.5).describe(), "1 px = 0.5 nm");
        assert_eq!(Scale::new(0.093517).describe(), "1 px = 0.09352 nm");
        // 自動選択は nm だが、保持されている単位があればそれが使われる。
        let scale = Scale {
            nm_per_px: 500.0,
            unit: LengthUnit::Micrometer,
        };
        assert_eq!(scale.describe(), "1 px = 0.5 µm");
        assert_eq!(scale.per_px(), 0.5);
    }

    /// 1 画素と画像全体は桁が違うので、それぞれに合う単位が選ばれること。
    #[test]
    fn extent_picks_its_own_unit() {
        let scale = Scale::new(0.093517);
        let (unit, w, _) = scale.extent(1024, 1024);
        assert_eq!(unit, LengthUnit::Nanometer);
        assert!((w - 95.761).abs() < 1e-2, "{w}");
    }
}
