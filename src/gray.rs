//! 16bit グレースケール画像と、その上で動く画素処理。
//!
//! アプリ内の画像は常にこの型で持ち回す。8bit 画像は読み込み時に 16bit へ
//! 拡張し、8bit へ落とすのは画面表示用テクスチャを作るときだけ。

use egui::ColorImage;

#[derive(Clone)]
pub struct Gray16 {
    pub width: u32,
    pub height: u32,
    /// 左上から行優先。長さは width * height。
    pub data: Vec<u16>,
}

impl Gray16 {
    pub fn black(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width as usize) * (height as usize)],
        }
    }

    #[inline]
    pub fn at(&self, x: u32, y: u32) -> u16 {
        self.data[(y as usize) * (self.width as usize) + (x as usize)]
    }

    pub fn from_dynamic(img: &image::DynamicImage) -> Self {
        // カラー画像はここでグレースケールへ落とす（輝度加重平均）。
        let buf = img.to_luma16();
        Self {
            width: buf.width(),
            height: buf.height(),
            data: buf.into_raw(),
        }
    }

    pub fn to_luma16_buffer(&self) -> image::ImageBuffer<image::Luma<u16>, Vec<u16>> {
        image::ImageBuffer::from_raw(self.width, self.height, self.data.clone())
            .expect("data length always matches width * height")
    }

    pub fn min_max(&self) -> (u16, u16) {
        let mut lo = u16::MAX;
        let mut hi = u16::MIN;
        for &v in &self.data {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if self.data.is_empty() {
            (0, u16::MAX)
        } else {
            (lo, hi)
        }
    }

    /// `bins` 個のビンに均等分割した輝度ヒストグラム。
    pub fn histogram(&self, bins: usize) -> Vec<u32> {
        let mut h = vec![0u32; bins];
        let scale = bins as f32 / 65536.0;
        for &v in &self.data {
            let b = ((v as f32 * scale) as usize).min(bins - 1);
            h[b] += 1;
        }
        h
    }

    /// 累積ヒストグラムから下側/上側 `frac` を切り捨てた輝度を返す（オートレベル用）。
    pub fn percentiles(&self, frac: f64) -> (u16, u16) {
        let hist = self.histogram(65536);
        let total: u64 = self.data.len() as u64;
        if total == 0 {
            return (0, u16::MAX);
        }
        let cut = (total as f64 * frac) as u64;
        let mut acc = 0u64;
        let mut lo = 0u16;
        for (i, &c) in hist.iter().enumerate() {
            acc += c as u64;
            if acc > cut {
                lo = i as u16;
                break;
            }
        }
        acc = 0;
        let mut hi = u16::MAX;
        for (i, &c) in hist.iter().enumerate().rev() {
            acc += c as u64;
            if acc > cut {
                hi = i as u16;
                break;
            }
        }
        if hi <= lo { (0, u16::MAX) } else { (lo, hi) }
    }

    /// 画像サイズを保ったまま中心まわりに回転する（時計回り、等倍）。
    /// はみ出た部分は捨てられ、埋まらない部分は黒（0）になる。
    pub fn rotate(&self, angle_deg: f32) -> Self {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return self.clone();
        }
        let theta = angle_deg.to_radians();
        let (sin, cos) = theta.sin_cos();
        let cx = (w as f32 - 1.0) * 0.5;
        let cy = (h as f32 - 1.0) * 0.5;

        let mut out = Self::black(w, h);
        for y in 0..h {
            let dy = y as f32 - cy;
            for x in 0..w {
                let dx = x as f32 - cx;
                // 出力→入力の逆写像なので -theta の回転行列を掛ける。
                let sx = cx + cos * dx + sin * dy;
                let sy = cy - sin * dx + cos * dy;
                out.data[(y as usize) * (w as usize) + x as usize] = self.sample_bilinear(sx, sy);
            }
        }
        out
    }

    /// 範囲外を 0 として扱う双一次補間サンプリング。
    #[inline]
    fn sample_bilinear(&self, sx: f32, sy: f32) -> u16 {
        let (w, h) = (self.width as i64, self.height as i64);
        let x0 = sx.floor() as i64;
        let y0 = sy.floor() as i64;
        if x0 < -1 || y0 < -1 || x0 >= w || y0 >= h {
            return 0;
        }
        let fx = sx - x0 as f32;
        let fy = sy - y0 as f32;
        let get = |x: i64, y: i64| -> f32 {
            if x < 0 || y < 0 || x >= w || y >= h {
                0.0
            } else {
                self.data[(y as usize) * (self.width as usize) + x as usize] as f32
            }
        };
        let top = get(x0, y0) * (1.0 - fx) + get(x0 + 1, y0) * fx;
        let bot = get(x0, y0 + 1) * (1.0 - fx) + get(x0 + 1, y0 + 1) * fx;
        (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 65535.0) as u16
    }

    /// 範囲外を最外周の画素値で padding する双一次補間サンプリング。
    /// 測長のフィッティング領域は画像からはみ出すことがあるため、
    /// 0 埋めの `sample_bilinear` とは別に用意した。
    pub fn sample_bilinear_clamp(&self, x: f64, y: f64) -> f64 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let (w, h) = (self.width as f64, self.height as f64);
        let cx = x.clamp(0.0, w - 1.0);
        let cy = y.clamp(0.0, h - 1.0);
        let x0 = cx.floor();
        let y0 = cy.floor();
        let x1 = (x0 + 1.0).min(w - 1.0);
        let y1 = (y0 + 1.0).min(h - 1.0);
        let fx = cx - x0;
        let fy = cy - y0;
        let get = |xi: f64, yi: f64| {
            self.data[(yi as usize) * (self.width as usize) + xi as usize] as f64
        };
        let top = get(x0, y0) * (1.0 - fx) + get(x1, y0) * fx;
        let bot = get(x0, y1) * (1.0 - fx) + get(x1, y1) * fx;
        top * (1.0 - fy) + bot * fy
    }

    /// ガウシアンぼかし（分離型 2 パス）。`sigma` は画素単位の標準偏差。
    /// カーネル半径は 3σ とし、画像の端は最外周の画素値で埋める。
    pub fn gaussian_blur(&self, sigma: f32) -> Self {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 {
            return self.clone();
        }
        let sigma = sigma.max(0.1);
        let radius = ((sigma * 3.0).ceil() as usize).max(1);
        let kernel: Vec<f32> = (0..=2 * radius)
            .map(|i| {
                let d = i as f32 - radius as f32;
                (-(d * d) / (2.0 * sigma * sigma)).exp()
            })
            .collect();
        let sum: f32 = kernel.iter().sum();
        let kernel: Vec<f32> = kernel.iter().map(|k| k / sum).collect();

        // 横 → 縦の順に畳み込む。端はクランプ（0 埋めだと縁が暗くなるため）。
        let mut tmp = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (i, &k) in kernel.iter().enumerate() {
                    let sx = (x + i).saturating_sub(radius).min(w - 1);
                    acc += self.data[y * w + sx] as f32 * k;
                }
                tmp[y * w + x] = acc;
            }
        }
        let mut out = Self::black(self.width, self.height);
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (i, &k) in kernel.iter().enumerate() {
                    let sy = (y + i).saturating_sub(radius).min(h - 1);
                    acc += tmp[sy * w + x] * k;
                }
                out.data[y * w + x] = acc.round().clamp(0.0, 65535.0) as u16;
            }
        }
        out
    }

    /// メディアンフィルタ。一辺 2r+1 の正方形窓の中央値を取る。
    /// 画像の端は最外周の画素値で埋める。窓が大きいと重いので、
    /// 呼び出し側で `radius` を小さく制限すること。
    pub fn median_filter(&self, radius: u32) -> Self {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 || radius == 0 {
            return self.clone();
        }
        let r = radius as usize;
        let mut out = Self::black(self.width, self.height);
        let mut window = Vec::with_capacity((2 * r + 1) * (2 * r + 1));
        for y in 0..h {
            let y0 = y.saturating_sub(r);
            let y1 = (y + r).min(h - 1);
            for x in 0..w {
                let x0 = x.saturating_sub(r);
                let x1 = (x + r).min(w - 1);
                window.clear();
                for j in y0..=y1 {
                    for i in x0..=x1 {
                        window.push(self.data[j * w + i]);
                    }
                }
                let mid = window.len() / 2;
                window.select_nth_unstable(mid);
                out.data[y * w + x] = window[mid];
            }
        }
        out
    }

    /// アンシャープマスク。`sigma` でぼかしたものとの差に `amount` を掛けて
    /// 元画像へ足し戻す（amount = 0 なら元のまま、1 が標準的な強さ）。
    pub fn unsharp_mask(&self, sigma: f32, amount: f32) -> Self {
        let blurred = self.gaussian_blur(sigma);
        let mut out = Self::black(self.width, self.height);
        for (o, (&v, &b)) in out
            .data
            .iter_mut()
            .zip(self.data.iter().zip(blurred.data.iter()))
        {
            let s = v as f32 + amount * (v as f32 - b as f32);
            *o = s.round().clamp(0.0, 65535.0) as u16;
        }
        out
    }

    /// `in_min`..`in_max` の輝度を 0..65535 へ線形に引き伸ばす。
    pub fn apply_levels(&self, in_min: u16, in_max: u16) -> Self {
        let lo = in_min.min(in_max);
        let hi = in_min.max(in_max);
        let mut lut = vec![0u16; 65536];
        if hi == lo {
            // 幅ゼロは二値化として扱う。
            for (v, out) in lut.iter_mut().enumerate() {
                *out = if v as u16 >= hi { u16::MAX } else { 0 };
            }
        } else {
            let span = (hi - lo) as f32;
            for (v, out) in lut.iter_mut().enumerate() {
                let t = ((v as f32 - lo as f32) / span).clamp(0.0, 1.0);
                *out = (t * 65535.0).round() as u16;
            }
        }
        Self {
            width: self.width,
            height: self.height,
            data: self.data.iter().map(|&v| lut[v as usize]).collect(),
        }
    }

    /// factor x factor のブロック平均で縮小する（表示用ミップの生成）。
    pub fn downsample_box(&self, factor: u32) -> Self {
        if factor <= 1 {
            return self.clone();
        }
        let nw = (self.width / factor).max(1);
        let nh = (self.height / factor).max(1);
        let mut out = Self::black(nw, nh);
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = 0u64;
                let mut n = 0u64;
                for j in 0..factor {
                    let sy = y * factor + j;
                    if sy >= self.height {
                        break;
                    }
                    for i in 0..factor {
                        let sx = x * factor + i;
                        if sx >= self.width {
                            break;
                        }
                        sum += self.at(sx, sy) as u64;
                        n += 1;
                    }
                }
                out.data[(y as usize) * (nw as usize) + x as usize] =
                    if n == 0 { 0 } else { (sum / n) as u16 };
            }
        }
        out
    }

    /// 表示レンジ `lo`..`hi` を 0..255 に写して egui のテクスチャ元画像を作る。
    pub fn to_color_image(&self, lo: u16, hi: u16) -> ColorImage {
        let (lo, hi) = if hi > lo { (lo, hi) } else { (0, u16::MAX) };
        let span = (hi - lo) as f32;
        let bytes: Vec<u8> = self
            .data
            .iter()
            .map(|&v| {
                let t = ((v as f32 - lo as f32) / span).clamp(0.0, 1.0);
                (t * 255.0).round() as u8
            })
            .collect();
        ColorImage::from_gray([self.width as usize, self.height as usize], &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_dot(w: u32, h: u32, x: u32, y: u32) -> Gray16 {
        let mut img = Gray16::black(w, h);
        img.data[(y as usize) * (w as usize) + x as usize] = u16::MAX;
        img
    }

    /// 中心の右にある点は、90° 回転で中心の下へ移る（= 時計回り）。
    #[test]
    fn rotate_90_is_clockwise() {
        let img = with_dot(9, 9, 6, 4);
        let out = img.rotate(90.0);
        assert_eq!(out.width, 9);
        assert_eq!(out.height, 9);
        assert_eq!(out.at(4, 6), u16::MAX);
        assert_eq!(out.at(6, 4), 0);
    }

    /// 画像サイズは変わらず、はみ出た画素は黒で埋まる。
    #[test]
    fn rotate_keeps_size_and_blackens_corners() {
        let img = Gray16 {
            width: 16,
            height: 16,
            data: vec![u16::MAX; 256],
        };
        let out = img.rotate(45.0);
        assert_eq!((out.width, out.height), (16, 16));
        assert_eq!(out.at(0, 0), 0);
        assert_eq!(out.at(8, 8), u16::MAX);
    }

    #[test]
    fn levels_stretch_is_linear_and_clamped() {
        let img = Gray16 {
            width: 5,
            height: 1,
            data: vec![0, 1000, 2000, 3000, 4000],
        };
        let out = img.apply_levels(1000, 3000);
        assert_eq!(out.data[0], 0, "下限より暗い画素は 0");
        assert_eq!(out.data[1], 0);
        assert_eq!(out.data[2], 32768, "中点は中間輝度");
        assert_eq!(out.data[3], u16::MAX);
        assert_eq!(out.data[4], u16::MAX, "上限より明るい画素は最大値");
    }

    #[test]
    fn downsample_box_averages() {
        let img = Gray16 {
            width: 2,
            height: 2,
            data: vec![0, 100, 200, 300],
        };
        let out = img.downsample_box(2);
        assert_eq!((out.width, out.height), (1, 1));
        assert_eq!(out.data[0], 150);
    }

    #[test]
    fn histogram_counts_all_pixels() {
        let img = Gray16 {
            width: 4,
            height: 1,
            data: vec![0, 0, u16::MAX, 32768],
        };
        let hist = img.histogram(4);
        assert_eq!(hist.iter().sum::<u32>(), 4);
        assert_eq!(hist[0], 2);
        assert_eq!(hist[3], 1);
    }

    #[test]
    fn clamp_sampling_pads_with_outermost_pixels() {
        let img = Gray16 {
            width: 4,
            height: 4,
            data: vec![100; 16],
        };
        assert_eq!(img.sample_bilinear_clamp(1.5, 1.5), 100.0);
        // 画像の外側は最外周の画素値で埋まる（0 ではない）。
        assert_eq!(img.sample_bilinear_clamp(-10.0, -10.0), 100.0);
        assert_eq!(img.sample_bilinear_clamp(100.0, 100.0), 100.0);
    }

    #[test]
    fn clamp_sampling_interpolates() {
        let mut img = Gray16::black(4, 4);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = (i as u16) * 100;
        }
        // (0.5, 0) は値 0 と 100 の中間。
        assert_eq!(img.sample_bilinear_clamp(0.5, 0.0), 50.0);
    }

    /// 一様画像はぼかしても変わらない（端のクランプと重みの正規化の確認）。
    #[test]
    fn gaussian_blur_keeps_uniform_image() {
        let img = Gray16 {
            width: 8,
            height: 8,
            data: vec![1000; 64],
        };
        let out = img.gaussian_blur(2.0);
        assert_eq!(out.data, vec![1000; 64]);
    }

    /// インパルスは周囲へ対称に広がる。
    #[test]
    fn gaussian_blur_spreads_impulse() {
        let img = with_dot(9, 9, 4, 4);
        let out = img.gaussian_blur(1.0);
        assert!(out.at(4, 4) > out.at(5, 4), "中心が最も明るい");
        assert!(out.at(5, 4) > 0, "隣へ漏れる");
        assert!(out.at(4, 4) < u16::MAX, "総和は保たれる");
        // 対称性: 左右の隣は同じ値。
        assert_eq!(out.at(3, 4), out.at(5, 4));
    }

    /// 平坦な画像のスパイク（ごま塩ノイズ相当）は中央値で消える。
    #[test]
    fn median_filter_removes_spikes() {
        let mut img = Gray16::black(7, 7);
        for v in img.data.iter_mut() {
            *v = 1000;
        }
        img.data[3 * 7 + 3] = u16::MAX; // 中心だけスパイク
        img.data[0] = 0; // 隅のスパイク（端のクランプ確認）
        let out = img.median_filter(1);
        assert_eq!(out.at(3, 3), 1000, "スパイクが除去される");
        assert_eq!(out.at(0, 0), 1000);
        assert_eq!(out.data, vec![1000; 49], "全域が平坦へ戻る");
    }

    /// アンシャープマスクは段差のコントラストを強調する。
    #[test]
    fn unsharp_mask_boosts_edges() {
        let mut img = Gray16::black(9, 1);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = if i < 4 { 1000 } else { 5000 };
        }
        let out = img.unsharp_mask(1.0, 2.0);
        assert!(out.at(2, 0) < 1000, "暗い側の縁はより暗く");
        assert!(out.at(5, 0) > 5000, "明るい側の縁はより明るく");
        assert_eq!(out.at(0, 0), 1000, "遠方は変わらない");
        // amount = 0 なら元画像のまま。
        let same = img.unsharp_mask(1.0, 0.0);
        assert_eq!(same.data, img.data);
    }
}
