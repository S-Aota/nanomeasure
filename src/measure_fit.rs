//! 端点位置の自動フィッティング。
//!
//! 端点を中心とする傾き付き長方形領域から画素をサンプリングし（元画像と
//! 同じ解像度、はみ出しは最外周の画素値で padding）、平均方向に平均した
//! 1 次元プロファイルへガウシアン（またはその微分）を当てはめる。
//! 依存クレートを増やさないため、Levenberg-Marquardt は自前の小さな実装。

use crate::gray::Gray16;
use crate::measure::{FitMode, Pt2};

/// フィッティング領域。中心 `center` に、`fit_axis`（縦 = フィッティング
/// 方向）とそれに直交する `avg_axis`（横 = 平均方向）を持つ。
/// 1 画素間隔でサンプリングし、中心行を含むよう長さは奇数が望ましい。
/// `mode` はオーバーレイ描画で枠の色を変えるためのもの。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitRegion {
    pub center: Pt2,
    pub fit_axis: Pt2,
    pub avg_axis: Pt2,
    pub fit_len: u32,
    pub avg_len: u32,
    pub mode: FitMode,
}

impl FitRegion {
    /// `dir` をフィッティング方向、その垂直を平均方向とする領域。
    pub fn new(center: Pt2, dir: Pt2, fit_len: u32, avg_len: u32, mode: FitMode) -> Self {
        Self {
            center,
            fit_axis: dir.normalize(),
            avg_axis: dir.perp().normalize(),
            fit_len,
            avg_len,
            mode,
        }
    }

    /// 描画用の 4 頂点（fit_axis が上、avg_axis が右になる順）。
    pub fn corners(&self) -> [Pt2; 4] {
        let f = self.fit_axis;
        let a = self.avg_axis;
        let hl = (self.fit_len as f64 - 1.0) * 0.5;
        let hw = (self.avg_len as f64 - 1.0) * 0.5;
        [
            self.center - f * hl - a * hw,
            self.center - f * hl + a * hw,
            self.center + f * hl + a * hw,
            self.center + f * hl - a * hw,
        ]
    }

    /// サンプリング位置（fit 方向 k 番目、avg 方向 m 番目の点）。
    pub fn sample_point(&self, k: u32, m: u32) -> Pt2 {
        let fk = k as f64 - (self.fit_len as f64 - 1.0) * 0.5;
        let fm = m as f64 - (self.avg_len as f64 - 1.0) * 0.5;
        self.center + self.fit_axis * fk + self.avg_axis * fm
    }
}

/// 平均方向に平均した 1 次元プロファイル（長さ fit_len）。
pub fn extract_profile(img: &Gray16, region: &FitRegion) -> Vec<f64> {
    (0..region.fit_len)
        .map(|k| {
            let sum: f64 = (0..region.avg_len)
                .map(|m| {
                    let p = region.sample_point(k, m);
                    img.sample_bilinear_clamp(p.x, p.y)
                })
                .sum();
            sum / region.avg_len as f64
        })
        .collect()
}

/// ガウシアンフィットの結果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaussFit {
    /// ピーク中心（サブピクセル）。
    pub mu: f64,
    pub sigma: f64,
    pub amplitude: f64,
    pub baseline: f64,
}

/// プロファイルの中心差分（境界は片側差分）。
fn derivative(profile: &[f64]) -> Vec<f64> {
    let n = profile.len();
    if n < 2 {
        return vec![0.0; n];
    }
    (0..n)
        .map(|i| {
            if i == 0 {
                profile[1] - profile[0]
            } else if i == n - 1 {
                profile[n - 1] - profile[n - 2]
            } else {
                (profile[i + 1] - profile[i - 1]) * 0.5
            }
        })
        .collect()
}

/// `f(x) = A·exp(−(x−μ)²/(2σ²)) + B` をプロファイルへ当てはめる
/// （Levenberg-Marquardt）。失敗（平坦・発散・振幅不足）なら None。
pub fn fit_gaussian(profile: &[f64]) -> Option<GaussFit> {
    let n = profile.len();
    if n < 3 {
        return None;
    }
    let min = profile.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = profile.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if max - min < 1e-6 {
        return None;
    }

    // 初期値: ベースラインは両端平均、ピークはベースラインからの
    // ずれが最大の位置（近傍で重心補正）。
    let b0 = (profile[0] + profile[n - 1]) * 0.5;
    let dev: Vec<f64> = profile.iter().map(|v| v - b0).collect();
    let imax = (0..n)
        .max_by(|&i, &j| dev[i].abs().partial_cmp(&dev[j].abs()).unwrap())
        .unwrap();
    let (lo, hi) = (imax.saturating_sub(1), (imax + 2).min(n));
    let (mut num, mut den) = (0.0, 0.0);
    for (i, &d) in dev.iter().enumerate().take(hi).skip(lo) {
        num += i as f64 * d.abs();
        den += d.abs();
    }
    let mu0 = if den > 0.0 { num / den } else { imax as f64 };

    let mut params = [dev[imax], mu0, 1.5, b0];
    if !lm_fit(profile, &mut params, true) {
        return None;
    }
    let [a, mu, sigma, b] = params;
    // ピークが領域外へ逃げた、幅が非現実的、振幅がノイズ程度 → 失敗。
    if mu < 0.0 || mu > (n - 1) as f64 || !(0.05..=(n as f64)).contains(&sigma) {
        return None;
    }
    if a.abs() < (max - min) * 0.1 {
        return None;
    }
    Some(GaussFit {
        mu,
        sigma,
        amplitude: a,
        baseline: b,
    })
}

/// プロファイルの微分へガウシアンを当てはめ、輝度のステップ位置を求める。
pub fn fit_derivative_gaussian(profile: &[f64]) -> Option<GaussFit> {
    let d = derivative(profile);
    let n = d.len();
    if n < 3 {
        return None;
    }
    let imax = (0..n)
        .max_by(|&i, &j| d[i].abs().partial_cmp(&d[j].abs()).unwrap())
        .unwrap();
    let mut params = [d[imax], imax as f64, 2.0, 0.0];
    if !lm_fit(&d, &mut params, false) {
        return None;
    }
    let [a, mu, sigma, b] = params;
    let range = d.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
        - d.iter().cloned().fold(f64::INFINITY, f64::min);
    if range < 1e-6
        || mu < 0.0
        || mu > (n - 1) as f64
        || !(0.05..=(n as f64)).contains(&sigma)
        || a.abs() < range * 0.2
    {
        return None;
    }
    Some(GaussFit {
        mu,
        sigma,
        amplitude: a,
        baseline: b,
    })
}

/// Levenberg-Marquardt による 4 パラメータガウシアンフィット。
/// `baseline` が true のとき B も自由、false（微分モード）なら B は 0 に固定。
fn lm_fit(data: &[f64], params: &mut [f64; 4], baseline: bool) -> bool {
    let n = data.len();
    let mut lambda = 1e-3;
    let x: Vec<f64> = (0..n).map(|i| i as f64).collect();

    for _ in 0..50 {
        // 残差とヤコビアン（数値微分は使わず解析的に計算する）。
        let mut r = vec![0.0; n];
        let mut j = vec![[0.0; 4]; n];
        for (i, &xi) in x.iter().enumerate() {
            let [a, mu, sigma, b] = *params;
            let e = (-((xi - mu).powi(2)) / (2.0 * sigma * sigma)).exp();
            let f = a * e + b;
            r[i] = data[i] - f;
            j[i][0] = e; // dA
            j[i][1] = a * e * (xi - mu) / (sigma * sigma); // dμ
            j[i][2] = a * e * (xi - mu).powi(2) / sigma.powi(3); // dσ
            j[i][3] = if baseline { 1.0 } else { 0.0 }; // dB
        }

        // 正規方程式 (JᵀJ + λ·diag(JᵀJ)) δ = Jᵀr
        let mut jtj = [[0.0; 4]; 4];
        let mut jtr = [0.0; 4];
        for i in 0..n {
            for a in 0..4 {
                jtr[a] += j[i][a] * r[i];
                for b in 0..4 {
                    jtj[a][b] += j[i][a] * j[i][b];
                }
            }
        }
        if !baseline {
            // B を固定するモードでは列 3 が退化する（すべて 0）ので、
            // 単位を入れて δB = 0 のまま解けるようにする。
            jtj[3][3] = 1.0;
        }
        let mut diag = [1e-9; 4];
        for k in 0..4 {
            diag[k] = jtj[k][k].max(1e-9);
        }
        for _ in 0..20 {
            let mut m = jtj;
            for k in 0..4 {
                m[k][k] += lambda * diag[k];
            }
            let Some(delta) = solve4(m, jtr) else {
                return false;
            };
            let mut next = *params;
            for k in 0..4 {
                next[k] += delta[k];
            }
            if !next.iter().all(|v| v.is_finite()) {
                return false;
            }
            let rss = |p: &[f64; 4]| -> f64 {
                let [a, mu, sigma, b] = *p;
                x.iter()
                    .zip(data)
                    .map(|(&xi, &y)| {
                        let f = a * (-((xi - mu).powi(2)) / (2.0 * sigma * sigma)).exp() + b;
                        (y - f).powi(2)
                    })
                    .sum()
            };
            let (old, new) = (rss(params), rss(&next));
            if new < old {
                *params = next;
                lambda *= 0.5;
                // 更新幅が十分小さくなったら収束。
                let scale = params.iter().map(|v| v.abs()).fold(0.0, f64::max);
                let step = delta.iter().map(|v| v.abs()).fold(0.0, f64::max);
                if step < 1e-8 * (scale + 1.0) {
                    return true;
                }
                break;
            }
            lambda *= 10.0;
            // 減衰が強すぎて更新できないなら、これ以上動かないので収束とみなす
            // （解の妥当性は呼び出し側の検証で判断する）。
            if lambda > 1e10 {
                return true;
            }
        }
    }
    true
}

/// 4x4 の連立一次方程式をガウスの消去法（Gauss-Jordan）で解く。
fn solve4(m: [[f64; 4]; 4], v: [f64; 4]) -> Option<[f64; 4]> {
    let mut m = m;
    let mut v = v;
    for col in 0..4 {
        let pivot = (col..4)
            .max_by(|&i, &j| m[i][col].abs().partial_cmp(&m[j][col].abs()).unwrap())
            .unwrap();
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        v.swap(col, pivot);
        // ピボット行を正規化してから、他の行からこの列を消去する。
        let inv = 1.0 / m[col][col];
        for k in col..4 {
            m[col][k] *= inv;
        }
        v[col] *= inv;
        for row in 0..4 {
            if row == col {
                continue;
            }
            let factor = m[row][col];
            if factor == 0.0 {
                continue;
            }
            for k in col..4 {
                m[row][k] -= factor * m[col][k];
            }
            v[row] -= factor * v[col];
        }
    }
    Some(v)
}

/// 端点をフィッティングする。成功したら新しい端点位置（サブピクセル）、
/// 失敗したら None（呼び出し側は元の位置のまま扱う）。
pub fn fit_endpoint(img: &Gray16, region: &FitRegion, mode: FitMode) -> Option<Pt2> {
    let profile = extract_profile(img, region);
    let fit = match mode {
        FitMode::Off => return None,
        FitMode::Gaussian => fit_gaussian(&profile),
        FitMode::DerivativeGaussian => fit_derivative_gaussian(&profile),
    }?;
    let offset = fit.mu - (region.fit_len as f64 - 1.0) * 0.5;
    Some(region.center + region.fit_axis * offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// x = edge で輝度が lo → hi に切り替わるステップエッジ画像。
    fn step_image(w: u32, h: u32, edge: f64, lo: u16, hi: u16) -> Gray16 {
        let mut img = Gray16::black(w, h);
        for y in 0..h {
            for x in 0..w {
                img.data[(y * w + x) as usize] = if (x as f64) < edge { lo } else { hi };
            }
        }
        img
    }

    #[test]
    fn profile_extraction_averages_along_avg_axis() {
        // 垂直エッジ（x = 10）。fit_axis = x 方向、avg_axis = y 方向。
        let img = step_image(40, 40, 10.0, 0, 1000);
        let region = FitRegion::new(Pt2::new(10.0, 20.0), Pt2::new(1.0, 0.0), 21, 11, FitMode::Gaussian);
        let profile = extract_profile(&img, &region);
        assert_eq!(profile.len(), 21);
        assert!(profile[0] < 1e-9);
        assert!((profile[20] - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn derivative_gaussian_finds_step_position() {
        let img = step_image(60, 60, 30.0, 200, 3000);
        let region = FitRegion::new(Pt2::new(30.0, 30.0), Pt2::new(1.0, 0.0), 41, 15, FitMode::DerivativeGaussian);
        let profile = extract_profile(&img, &region);
        let fit = fit_derivative_gaussian(&profile).expect("ステップがあるので成功する");
        // ステップは x=29 と x=30 の間なので、位置は 19.5 が正解。
        assert!((fit.mu - 19.5).abs() < 0.05, "{}", fit.mu);
        let pos = fit_endpoint(&img, &region, FitMode::DerivativeGaussian).unwrap();
        assert!((pos.x - 29.5).abs() < 0.05, "{}", pos.x);
        assert!((pos.y - 30.0).abs() < 1e-9);
    }

    #[test]
    fn gaussian_finds_bright_band_center() {
        // 中央に幅 4 px の明るいバンド（ガウシアン状）を置く。
        let (w, h) = (80, 80);
        let mut img = Gray16::black(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 500.0 * (-((x as f64 - 40.0).powi(2)) / 18.0).exp() + 100.0;
                img.data[(y * w + x) as usize] = v.round() as u16;
            }
        }
        let region = FitRegion::new(Pt2::new(40.0, 40.0), Pt2::new(1.0, 0.0), 31, 9, FitMode::Gaussian);
        let profile = extract_profile(&img, &region);
        let fit = fit_gaussian(&profile).expect("バンドがあるので成功する");
        assert!((fit.mu - 15.0).abs() < 0.1, "中心行 15: {}", fit.mu);
        let pos = fit_endpoint(&img, &region, FitMode::Gaussian).unwrap();
        assert!((pos.x - 40.0).abs() < 0.1, "{}", pos.x);
    }

    #[test]
    fn flat_profile_fails() {
        let mut img = Gray16::black(30, 30);
        for v in img.data.iter_mut() {
            *v = 500;
        }
        let region = FitRegion::new(Pt2::new(15.0, 15.0), Pt2::new(1.0, 0.0), 21, 5, FitMode::Gaussian);
        let profile = extract_profile(&img, &region);
        assert!(fit_gaussian(&profile).is_none());
        assert!(fit_derivative_gaussian(&profile).is_none());
    }

    #[test]
    fn region_corners_form_rotated_rectangle() {
        let region = FitRegion::new(
            Pt2::new(10.0, 10.0),
            Pt2::new(1.0, 1.0),
            11,
            5,
            FitMode::Gaussian,
        );
        let c = region.corners();
        // 中心は 4 頂点の平均。
        let center = c.iter().fold(Pt2::new(0.0, 0.0), |a, &b| a + b) / 4.0;
        assert!((center.x - 10.0).abs() < 1e-9);
        assert!((center.y - 10.0).abs() < 1e-9);
        // fit 方向の長さ（サンプル数-1）は頂点間の距離と一致する（c[0]→c[3] は
        // fit 方向の辺、c[0]→c[2] は対角線）。
        assert!(((c[3] - c[0]).length() - 10.0).abs() < 1e-9);
        assert!(((c[1] - c[0]).length() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_sampling_uses_outermost_pixel() {
        // 全画像が 700 で、その画像の外側へはみ出す領域でも 700 が返る。
        let img = Gray16 {
            width: 20,
            height: 20,
            data: vec![700; 400],
        };
        let region = FitRegion::new(Pt2::new(-5.0, -5.0), Pt2::new(1.0, 0.0), 11, 5, FitMode::Gaussian);
        let profile = extract_profile(&img, &region);
        assert!(profile.iter().all(|&v| (v - 700.0).abs() < 1e-9));
    }
}
