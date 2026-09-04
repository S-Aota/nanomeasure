//! 測長コマンドのデータモデルとジオメトリ計算。
//!
//! コマンドには「ユーザーが指定した端点」だけを保存し、フィッティング結果と
//! 測定値（長さ）は `compute()` で適用のたびに再計算する。これにより履歴を
//! 別の画像へ適用すると、同じ設定のまま新しい画像に再フィッティングされる。
//!
//! このモジュールは egui に依存しない純粋ロジックのみ。

use std::collections::HashSet;
use std::ops::{Add, Div, Mul, Sub};

use serde::{Deserialize, Serialize};

use crate::frame::{format_length, Scale};
use crate::gray::Gray16;
use crate::measure_fit::{self, FitRegion};

/// 画像 px 座標の点（小数可）。serde では `{"x": .., "y": ..}`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pt2 {
    pub x: f64,
    pub y: f64,
}

impl Pt2 {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    /// 単位ベクトル。零ベクトルなら x 軸方向を返す。
    pub fn normalize(self) -> Self {
        let len = self.length();
        if len > 0.0 {
            self / len
        } else {
            Self::new(1.0, 0.0)
        }
    }

    /// 反時計回りに 90° 回転（y 下向きの画像座標では「左」を向く）。
    pub fn perp(self) -> Self {
        Self::new(-self.y, self.x)
    }
}

impl Add for Pt2 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Pt2 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f64> for Pt2 {
    type Output = Self;
    fn mul(self, s: f64) -> Self {
        Self::new(self.x * s, self.y * s)
    }
}

impl Div<f64> for Pt2 {
    type Output = Self;
    fn div(self, s: f64) -> Self {
        Self::new(self.x / s, self.y / s)
    }
}

/// 端点の自動フィッティングの方式。1=Off / 2=Gaussian / 3=DerivativeGaussian。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// クリック位置そのまま。
    #[default]
    Off,
    /// ガウシアン分布でフィッティング（明暗の境界線の検出）。
    Gaussian,
    /// プロファイルの微分（輝度のステップ）にガウシアンをフィッティング。
    DerivativeGaussian,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AngleMode {
    /// 0/90/180/270° の 4 方向にスナップ。
    #[default]
    FourDir,
    /// 任意の角度。
    Free,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewMeasureMode {
    /// 新しい二点間測長のたびにチェックを "new group" へ戻す。
    #[default]
    NewGroup,
    /// 前回測定したグループが有効なまま。
    Keep,
}

/// 1 端点ぶんのフィッティング設定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FitSettings {
    pub mode: FitMode,
    /// 検出領域の横長さ（平均方向、ノイズ軽減）。1..=20。
    pub width_px: u32,
    /// 検出領域の縦長さ（フィッティング方向）。1..=50。
    pub length_px: u32,
}

impl Default for FitSettings {
    fn default() -> Self {
        Self {
            mode: FitMode::Off,
            width_px: 11,
            length_px: 31,
        }
    }
}

/// 測定結果をまとめるグループ。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasureGroup {
    pub id: u64,
    pub name: String,
}

/// 測長モードの設定。デフォルトはすべて左側の選択。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasurePreferences {
    pub angle: AngleMode,
    pub snap: bool,
    pub new_measure: NewMeasureMode,
}

impl Default for MeasurePreferences {
    fn default() -> Self {
        Self {
            angle: AngleMode::FourDir,
            snap: false,
            new_measure: NewMeasureMode::NewGroup,
        }
    }
}

/// 測長コマンドに保存されるツール 1 つ。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeasureTool {
    /// 二点間測長（赤矢印）。p1/p2 はユーザー指定点（フィッティング前）。
    /// フィッティング設定は測長ごとに保持する（作成時にパネルの
    /// 設定をコピーしたもの。`default` は旧形式のデータを読むため）。
    Distance {
        id: u64,
        p1: Pt2,
        p2: Pt2,
        group: u64,
        #[serde(default)]
        fit1: FitSettings,
        #[serde(default)]
        fit2: FitSettings,
    },
    /// 境界線（線分、紫）。測定値は長さ。結果リストに出る。
    /// フィッティング設定は測長ごとに保持する。
    Boundary {
        id: u64,
        p1: Pt2,
        p2: Pt2,
        group: u64,
        #[serde(default)]
        fit: FitSettings,
    },
    /// オフセット線。境界線を法線方向へずらした補助線（無限直線、紫）。
    /// `distance` は符号付き px。法線 = 境界線方向を +90° 回転した側が正。
    Offset {
        id: u64,
        source: u64,
        distance: f64,
    },
}

impl MeasureTool {
    pub fn id(&self) -> u64 {
        match self {
            Self::Distance { id, .. } | Self::Boundary { id, .. } | Self::Offset { id, .. } => *id,
        }
    }

    /// 測定結果リストに出るツールか。境界線・オフセット線は補助線扱いなので
    /// 出さない（二点間測長のみ）。
    pub fn is_measurement(&self) -> bool {
        matches!(self, Self::Distance { .. })
    }

}

/// 測長コマンドのデータ全体。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasureData {
    pub prefs: MeasurePreferences,
    pub groups: Vec<MeasureGroup>,
    /// 結果リストでチェックされているグループ。None = "new group" 行がチェック。
    pub active_group: Option<u64>,
    /// 二点間測長の端点 1 / 端点 2 のフィッティング設定（別々に持つ）。
    pub dist_fit1: FitSettings,
    pub dist_fit2: FitSettings,
    /// 境界線のフィッティング設定（端点 1 のみ使用）。
    pub boundary_fit: FitSettings,
    pub tools: Vec<MeasureTool>,
}

impl Default for MeasureData {
    fn default() -> Self {
        Self {
            prefs: MeasurePreferences::default(),
            groups: Vec::new(),
            active_group: None,
            dist_fit1: FitSettings::default(),
            dist_fit2: FitSettings::default(),
            boundary_fit: FitSettings::default(),
            tools: Vec::new(),
        }
    }
}

impl MeasureData {
    /// 未使用の最小 ID。
    pub fn next_id(&self) -> u64 {
        self.tools.iter().map(|t| t.id()).max().unwrap_or(0) + 1
    }

    /// `gid` のグループがまだ存在するか。
    pub fn has_group(&self, gid: u64) -> bool {
        self.groups.iter().any(|g| g.id == gid)
    }

    /// 新しい測定の入れ先グループ。None（new group）なら "group N" を
    /// 生成してそちらを返し、生成したかどうかを併せて返す。
    pub fn group_for_new_measurement(&mut self) -> u64 {
        match self.active_group {
            Some(gid) if self.has_group(gid) => gid,
            _ => {
                let id = self.next_group_id();
                let name = next_group_name(&self.groups);
                self.groups.push(MeasureGroup { id, name });
                self.active_group = Some(id);
                id
            }
        }
    }

    /// 「新規測長: グループを追加」なら次回用にチェックを new group へ戻す。
    pub fn apply_new_measure_mode(&mut self) {
        if self.prefs.new_measure == NewMeasureMode::NewGroup {
            self.active_group = None;
        }
    }

    fn next_group_id(&self) -> u64 {
        self.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1
    }

    /// グループ内の、結果リストに出す測定ツール（二点間測長のみ）を
    /// ツール順に並べた ID 列。
    pub fn group_tools(&self, gid: u64) -> Vec<u64> {
        self.tools
            .iter()
            .filter(|t| {
                matches!(t, MeasureTool::Distance { group, .. } if *group == gid)
            })
            .map(|t| t.id())
            .collect()
    }

    /// スナップ対象の無限直線（境界線の延長とオフセット線の計算結果）。
    pub fn snap_lines(&self, img: &Gray16, scale: Option<Scale>) -> Vec<SnapLine> {
        let computed = self.compute(img, scale);
        let mut lines = Vec::new();
        for t in &computed.tools {
            match self.tool_by_id(t.id) {
                Some(MeasureTool::Boundary { .. } | MeasureTool::Offset { .. }) => {
                    lines.push(SnapLine::from_points(t.p1, t.p2));
                }
                _ => {}
            }
        }
        lines
    }

    pub fn tool_by_id(&self, id: u64) -> Option<&MeasureTool> {
        self.tools.iter().find(|t| t.id() == id)
    }

    /// フィッティング結果と測定値を再計算する。オーバーレイ描画と
    /// 結果リストの両方がこれを呼ぶ（毎フレーム再計算・キャッシュなし）。
    pub fn compute(&self, img: &Gray16, scale: Option<Scale>) -> ComputedMeasure {
        self.compute_impl(img, scale, true)
    }

    /// ドラッグ移動中など、プレビュー用にフィッティングを再計算しない版。
    pub fn compute_without_fit(&self, img: &Gray16, scale: Option<Scale>) -> ComputedMeasure {
        self.compute_impl(img, scale, false)
    }

    fn compute_impl(
        &self,
        img: &Gray16,
        scale: Option<Scale>,
        fit: bool,
    ) -> ComputedMeasure {
        let mut tools = Vec::new();
        for tool in &self.tools {
            if let Some(c) = self.compute_tool(tool, img, scale, fit) {
                tools.push(c);
            }
        }
        ComputedMeasure { tools }
    }

    fn compute_tool(
        &self,
        tool: &MeasureTool,
        img: &Gray16,
        _scale: Option<Scale>,
        fit: bool,
    ) -> Option<ComputedTool> {
        match tool {
            MeasureTool::Distance {
                id,
                p1,
                p2,
                group,
                fit1,
                fit2,
            } => {
                let dir = (*p2 - *p1).normalize();
                let p1_fit = if fit {
                    fit_endpoint(*p1, dir, *fit1, img, true)
                } else {
                    *p1
                };
                let p2_fit = if fit {
                    fit_endpoint(*p2, dir, *fit2, img, true)
                } else {
                    *p2
                };
                // 領域枠は最初に与えた端点位置を中心に固定し、
                // フィッティングで検出位置が動いても追従させない。
                // モード 1（クリック位置そのまま）でも表示し、色で設定がわかる。
                let regions = [
                    FitRegion::new(*p1, dir, fit1.length_px, fit1.width_px, fit1.mode),
                    FitRegion::new(*p2, dir, fit2.length_px, fit2.width_px, fit2.mode),
                ]
                .to_vec();
                Some(ComputedTool {
                    id: *id,
                    kind: ToolKind::Distance,
                    p1: p1_fit,
                    p2: p2_fit,
                    length_px: Some((p2_fit - p1_fit).length()),
                    group: Some(*group),
                    distance_px: None,
                    fit_regions: regions,
                })
            }
            MeasureTool::Boundary {
                id,
                p1,
                p2,
                group,
                fit: boundary_fit,
            } => {
                let dir = (*p2 - *p1).normalize();
                // 領域枠は最初に与えた一点目を中心に固定（表示のみ。
                // モード 2/3 でも同じ位置）。フィッティングで動かない。
                let region = FitRegion::new(
                    *p1,
                    dir.perp(),
                    boundary_fit.length_px,
                    boundary_fit.width_px,
                    boundary_fit.mode,
                );
                if boundary_fit.mode == FitMode::Off || !fit {
                    // モード 1（またはプレビュー）: ユーザー指定の二点間のまま。
                    return Some(ComputedTool {
                        id: *id,
                        kind: ToolKind::Boundary,
                        p1: *p1,
                        p2: *p2,
                        length_px: Some((*p2 - *p1).length()),
                        group: Some(*group),
                        distance_px: None,
                        fit_regions: vec![region],
                    });
                }
                // モード 2/3: 一点目が線分の中点かつフィッティング領域の中心。
                // 平均方向 = 二点目方向。フィッティングで一点目を垂直方向に調整する。
                let center = fit_endpoint(*p1, dir, *boundary_fit, img, false);
                let half = (*p2 - *p1).length();
                Some(ComputedTool {
                    id: *id,
                    kind: ToolKind::Boundary,
                    p1: center - dir * half,
                    p2: center + dir * half,
                    length_px: Some(half * 2.0),
                    group: Some(*group),
                    distance_px: None,
                    fit_regions: vec![region],
                })
            }
            MeasureTool::Offset {
                id,
                source,
                distance,
            } => {
                // 元の境界線を法線方向へずらした、同じ長さの線分。
                let src = self.compute_tool(self.tool_by_id(*source)?, img, _scale, fit)?;
                let n = (src.p2 - src.p1).normalize().perp();
                Some(ComputedTool {
                    id: *id,
                    kind: ToolKind::Offset,
                    p1: src.p1 + n * *distance,
                    p2: src.p2 + n * *distance,
                    length_px: None,
                    group: None,
                    distance_px: Some(*distance),
                    fit_regions: Vec::new(),
                })
            }
        }
    }
}

/// 端点のフィッティング。`dir` はツールの線方向。失敗時は元の点。
///
/// - 二点間測長（`fit_along_dir` = true）: フィット方向 = 線方向、
///   平均方向 = 垂直（仕様どおり横 = 平均 = 線に垂直）。
/// - 境界線モード 2/3（`fit_along_dir` = false）: 平均方向 = 線方向
///   （仕様どおり二点目方向が平均を取る方向）、フィット方向 = その垂直。
fn fit_endpoint(
    center: Pt2,
    dir: Pt2,
    settings: FitSettings,
    img: &Gray16,
    fit_along_dir: bool,
) -> Pt2 {
    if settings.mode == FitMode::Off {
        return center;
    }
    let fit_axis = if fit_along_dir { dir } else { dir.perp() };
    let region = FitRegion::new(
        center,
        fit_axis,
        settings.length_px,
        settings.width_px,
        settings.mode,
    );
    measure_fit::fit_endpoint(img, &region, settings.mode).unwrap_or(center)
}

/// ツールの種類（計算結果側。描画の色・形の切り替えに使う）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Distance,
    Boundary,
    Offset,
}

/// フィッティング適用後の表示ジオメトリ。値は保存されず毎回再計算される。
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedTool {
    pub id: u64,
    pub kind: ToolKind,
    pub p1: Pt2,
    pub p2: Pt2,
    /// Distance / Boundary のみ。フィッティング後の長さ（px）。
    pub length_px: Option<f64>,
    pub group: Option<u64>,
    /// オフセット線の符号付きオフセット距離（px）。
    pub distance_px: Option<f64>,
    /// フィッティング領域の枠（オーバーレイ描画用）。Off なら空。
    pub fit_regions: Vec<FitRegion>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputedMeasure {
    pub tools: Vec<ComputedTool>,
}

impl ComputedMeasure {
    pub fn by_id(&self, id: u64) -> Option<&ComputedTool> {
        self.tools.iter().find(|t| t.id == id)
    }
}

/// スナップに使う無限直線（単位方向つき）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapLine {
    pub origin: Pt2,
    pub dir: Pt2,
}

impl SnapLine {
    pub fn from_points(p1: Pt2, p2: Pt2) -> Self {
        Self {
            origin: p1,
            dir: (p2 - p1).normalize(),
        }
    }

    /// 点から直線までの距離。
    pub fn distance_to(&self, p: Pt2) -> f64 {
        let v = p - self.origin;
        (self.dir.x * v.y - self.dir.y * v.x).abs()
    }

    /// 直線上の最近点（射影）。
    pub fn project(&self, p: Pt2) -> Pt2 {
        let v = p - self.origin;
        let t = self.dir.x * v.x + self.dir.y * v.y;
        self.origin + self.dir * t
    }
}

/// 二点間測長の端点 2 を置くときのスナップ計算。
///
/// `p1` / `p1_line` は端点 1 とそのスナップ先（既に配置済み）。
/// `cursor` は端点 2 のユーザー入力位置。`lines` はスナップ候補の直線。
/// しきい値は画像 px。
pub fn snap_distance(
    p1: Pt2,
    p1_line: Option<&SnapLine>,
    cursor: Pt2,
    lines: &[SnapLine],
    threshold: f64,
) -> (Pt2, Pt2) {
    // カーソルに最も近い直線を選ぶ。
    let nearest = lines
        .iter()
        .filter(|l| l.distance_to(cursor) <= threshold)
        .min_by(|a, b| {
            a.distance_to(cursor)
                .partial_cmp(&b.distance_to(cursor))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    match (p1_line, nearest) {
        // 両方スナップ（同一の直線でも可）: 各端点をそれぞれの直線上の
        // ユーザー入力位置に最も近い点へ固定する。
        (Some(l1), Some(l2)) => (l1.project(p1), l2.project(cursor)),
        // 片方のみ（端点 2 がスナップ）: 測長線が補助線と直交するように、
        // 端点 2 は端点 1 から直線へ下ろした垂線の足にする。
        (None, Some(l2)) => (p1, l2.project(p1)),
        // 片方のみ（端点 1 がスナップ中）: 端点 1 は直線上に固定しつつ、
        // 線方向には端点 2 に合わせて垂線の足へ追従する。
        (Some(l1), None) => (l1.project(cursor), cursor),
        (None, None) => (p1, cursor),
    }
}

/// 角度 4 方向スナップ。`p1` を固定し、カーソル方向を最も近い
/// 0/90/180/270° に丸めて、その直線へ射影した点を返す。
pub fn snap_angle_four(p1: Pt2, cursor: Pt2) -> Pt2 {
    let v = cursor - p1;
    if v.length() < 1e-9 {
        return cursor;
    }
    let angle = (v.y.atan2(v.x) / std::f64::consts::FRAC_PI_2).round() * std::f64::consts::FRAC_PI_2;
    let dir = Pt2::new(angle.cos(), angle.sin());
    let t = dir.x * v.x + dir.y * v.y;
    p1 + dir * t
}

/// 既存の "group N" と重ならない最小の N で "group N" を作る。
pub fn next_group_name(groups: &[MeasureGroup]) -> String {
    let taken: HashSet<u64> = groups
        .iter()
        .filter_map(|g| g.name.strip_prefix("group ")?.trim().parse().ok())
        .collect();
    let n = (1u64..).find(|n| !taken.contains(n)).unwrap_or(1);
    format!("group {n}")
}

/// 測定値の表示。スケールがあれば実寸、なければ px。
pub fn format_measurement(px: f64, scale: Option<Scale>) -> String {
    match scale {
        Some(scale) => format!(
            "{} {}",
            format_length(px * scale.per_px()),
            scale.unit.label()
        ),
        None => format!("{} px", format_length(px)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Pt2 {
        Pt2::new(x, y)
    }

    #[test]
    fn snap_line_distance_and_projection() {
        // 水平線 y = 10。
        let line = SnapLine::from_points(pt(0.0, 10.0), pt(100.0, 10.0));
        assert!((line.distance_to(pt(50.0, 14.0)) - 4.0).abs() < 1e-9);
        let p = line.project(pt(50.0, 14.0));
        assert!((p.x - 50.0).abs() < 1e-9);
        assert!((p.y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn snap_none_when_far_away() {
        let lines = [SnapLine::from_points(pt(0.0, 0.0), pt(100.0, 0.0))];
        let (p1, p2) = snap_distance(pt(0.0, 50.0), None, pt(100.0, 50.0), &lines, 10.0);
        assert_eq!(p1, pt(0.0, 50.0));
        assert_eq!(p2, pt(100.0, 50.0));
    }

    #[test]
    fn snap_both_endpoints_even_same_line() {
        // 同一の水平線に両端点がスナップ: それぞれ射影位置に固定。
        let line = SnapLine::from_points(pt(0.0, 0.0), pt(100.0, 0.0));
        let (p1, p2) = snap_distance(
            pt(10.0, 2.0),
            Some(&line),
            pt(80.0, 3.0),
            &[line],
            10.0,
        );
        assert_eq!(p1, pt(10.0, 0.0));
        assert_eq!(p2, pt(80.0, 0.0));
    }

    #[test]
    fn snap_only_p2_uses_perpendicular_foot() {
        // 端点 2 だけが水平線にスナップ → 端点 1 から垂線の足。
        let line = SnapLine::from_points(pt(0.0, 0.0), pt(100.0, 0.0));
        let (p1, p2) = snap_distance(pt(40.0, 60.0), None, pt(80.0, 2.0), &[line], 10.0);
        assert_eq!(p1, pt(40.0, 60.0));
        assert_eq!(p2, pt(40.0, 0.0), "垂線の足。カーソルの x は無視される");
    }

    #[test]
    fn snap_only_p1_follows_cursor_foot() {
        // 端点 1 がスナップ中でカーソルは線から離れた → 端点 1 が垂線の足へ追従。
        let line = SnapLine::from_points(pt(0.0, 0.0), pt(100.0, 0.0));
        let (p1, p2) = snap_distance(
            pt(10.0, 0.0),
            Some(&line),
            pt(70.0, 40.0),
            &[line],
            10.0,
        );
        assert_eq!(p1, pt(70.0, 0.0));
        assert_eq!(p2, pt(70.0, 40.0));
    }

    #[test]
    fn angle_four_snaps_each_quadrant() {
        let origin = pt(50.0, 50.0);
        let close = |a: Pt2, b: Pt2| (a - b).length() < 1e-9;
        // 右・下・左・上の各方向（少しずらした入力）。
        assert!(close(snap_angle_four(origin, pt(120.0, 58.0)), pt(120.0, 50.0)));
        assert!(close(snap_angle_four(origin, pt(44.0, 130.0)), pt(50.0, 130.0)));
        assert!(close(snap_angle_four(origin, pt(-20.0, 46.0)), pt(-20.0, 50.0)));
        assert!(close(snap_angle_four(origin, pt(56.0, -30.0)), pt(50.0, -30.0)));
    }

    #[test]
    fn next_group_name_skips_taken() {
        let groups = |names: &[&str]| {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| MeasureGroup {
                    id: i as u64,
                    name: n.to_string(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(next_group_name(&[]), "group 1");
        assert_eq!(next_group_name(&groups(&["group 1"])), "group 2");
        assert_eq!(
            next_group_name(&groups(&["group 1", "group 2", "好きな名前"])),
            "group 3"
        );
    }

    #[test]
    fn new_group_auto_generates_and_apply_mode_resets() {
        let mut data = MeasureData::default();
        let g1 = data.group_for_new_measurement();
        assert_eq!(data.groups.len(), 1);
        assert_eq!(data.groups[0].name, "group 1");
        // NewGroup モード（デフォルト）: 測定後は new group に戻る。
        data.apply_new_measure_mode();
        assert_eq!(data.active_group, None);
        let g2 = data.group_for_new_measurement();
        assert_ne!(g1, g2);
        // Keep モードではそのまま。
        data.prefs.new_measure = NewMeasureMode::Keep;
        data.apply_new_measure_mode();
        assert_eq!(data.active_group, Some(g2));
    }

    #[test]
    fn json_round_trip() {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        data.tools.push(MeasureTool::Distance {
            id: 1,
            p1: pt(12.5, 34.25),
            p2: pt(12.5, 200.0),
            group: g,
            fit1: FitSettings::default(),
            fit2: FitSettings::default(),
        });
        data.tools.push(MeasureTool::Offset {
            id: 2,
            source: 1,
            distance: -7.5,
        });
        let json = serde_json::to_string(&data).unwrap();
        let back: MeasureData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
        // kind タグが snake_case で出ること。
        assert!(json.contains("\"kind\":\"distance\""));
        assert!(json.contains("\"kind\":\"offset\""));
    }

    #[test]
    fn compute_distance_without_fit_is_straight_line() {
        let img = Gray16::black(100, 100);
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        data.tools.push(MeasureTool::Distance {
            id: 1,
            p1: pt(10.0, 10.0),
            p2: pt(40.0, 50.0),
            group: g,
            fit1: FitSettings::default(),
            fit2: FitSettings::default(),
        });
        let c = data.compute(&img, None);
        let t = c.by_id(1).unwrap();
        assert_eq!(t.p1, pt(10.0, 10.0));
        assert_eq!(t.p2, pt(40.0, 50.0));
        assert!((t.length_px.unwrap() - 50.0).abs() < 1e-9);
        // 領域枠はモード 1（Off）でも表示する（色で設定がわかるように）。
        assert_eq!(t.fit_regions.len(), 2, "端点 1/2 の領域");
    }

    #[test]
    fn boundary_mode1_is_user_segment() {
        let img = Gray16::black(100, 100);
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        data.tools.push(MeasureTool::Boundary {
            id: 1,
            p1: pt(0.0, 0.0),
            p2: pt(30.0, 40.0),
            group: g,
            fit: FitSettings::default(),
        });
        let c = data.compute(&img, None);
        let t = c.by_id(1).unwrap();
        assert_eq!(t.p1, pt(0.0, 0.0));
        assert_eq!(t.p2, pt(30.0, 40.0));
        assert!((t.length_px.unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn boundary_fit_centers_on_edge_and_doubles_length() {
        // y = 30 で輝度が 0 → 3000 に変わる水平エッジ。
        let (w, h) = (200, 100);
        let mut img = Gray16::black(w, h);
        for y in 0..h {
            for x in 0..w {
                img.data[(y * w + x) as usize] = if y >= 30 { 3000 } else { 0 };
            }
        }
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        // 線方向は水平（二点目が右）。一点目はエッジから 1 px ずらして置く。
        let fit = FitSettings {
            mode: FitMode::DerivativeGaussian,
            width_px: 11,
            length_px: 41,
        };
        data.tools.push(MeasureTool::Boundary {
            id: 1,
            p1: pt(50.0, 31.0),
            p2: pt(100.0, 31.0),
            group: g,
            fit,
        });
        let c = data.compute(&img, None);
        let t = c.by_id(1).unwrap();
        // 中心がエッジ（y ≈ 29.5）へ吸着し、線分は p1 を中心に 2 倍の長さ。
        assert!((t.p1.y - 29.5).abs() < 0.5, "{}", t.p1.y);
        assert!((t.p2.y - 29.5).abs() < 0.5, "{}", t.p2.y);
        assert!((t.p1.x - 0.0).abs() < 1e-9, "{}", t.p1.x);
        assert!((t.p2.x - 100.0).abs() < 1e-9, "{}", t.p2.x);
        assert!((t.length_px.unwrap() - 100.0).abs() < 1e-9);
    }
}
