//! グレースケール画像と、その上で動く画素処理。
//!
//! アプリ内の画像は常にこの型で持ち回す。ビット深度（8bit か 16bit）は
//! 読み込み時のまま保持し、処理も同じ深度のまま行う（8bit 画像を 16bit へ
//! 拡張しない）。画素値はどちらの深度でも 0..max_value に収め、
//! 8bit へ落とすのは画面表示用テクスチャを作るときだけ。
//!
//! 画素処理は行単位で rayon により並列化する。各行の計算順序は逐次版と
//! 同じなので、結果はスレッド数によらず一致する。

use egui::{Color32, ColorImage};
use rayon::prelude::*;

#[derive(Clone)]
pub struct Gray16 {
    pub width: u32,
    pub height: u32,
    /// 1 画素のビット深度（8 か 16）。8bit 画像は値 0..255 のまま保持し、
    /// 16bit へ拡張しない。画像処理は入力の深度を保つ。
    pub depth: u8,
    /// 左上から行優先。長さは width * height。
    pub data: Vec<u16>,
}

impl Gray16 {
    /// 16bit の黒画像（テスト用の仮画像）。
    #[cfg(test)]
    pub fn black(width: u32, height: u32) -> Self {
        Self::black_with_depth(width, height, 16)
    }

    /// 指定したビット深度の黒画像（処理結果の出力先として、入力と同じ
    /// 深度の画像を作るために使う）。
    fn black_with_depth(width: u32, height: u32, depth: u8) -> Self {
        Self {
            width,
            height,
            depth,
            data: vec![0; (width as usize) * (height as usize)],
        }
    }

    /// 画素値の最大値（8bit なら 255、16bit なら 65535）。
    pub fn max_value(&self) -> u16 {
        if self.depth == 8 {
            u8::MAX as u16
        } else {
            u16::MAX
        }
    }

    #[inline]
    pub fn at(&self, x: u32, y: u32) -> u16 {
        self.data[(y as usize) * (self.width as usize) + (x as usize)]
    }

    pub fn from_dynamic(img: &image::DynamicImage) -> Self {
        // カラー画像はここでグレースケールへ落とす（輝度加重平均）。
        // 元のビット深度は保つ（8bit 画像を 16bit へ拡張しない）。
        use image::DynamicImage as D;
        let is_8bit = matches!(
            img,
            D::ImageLuma8(_) | D::ImageLumaA8(_) | D::ImageRgb8(_) | D::ImageRgba8(_)
        );
        if is_8bit {
            let buf = img.to_luma8();
            Self {
                width: buf.width(),
                height: buf.height(),
                depth: 8,
                data: buf.into_raw().into_iter().map(u16::from).collect(),
            }
        } else {
            let buf = img.to_luma16();
            Self {
                width: buf.width(),
                height: buf.height(),
                depth: 16,
                data: buf.into_raw(),
            }
        }
    }

    pub fn to_luma16_buffer(&self) -> image::ImageBuffer<image::Luma<u16>, Vec<u16>> {
        image::ImageBuffer::from_raw(self.width, self.height, self.data.clone())
            .expect("data length always matches width * height")
    }

    /// 8bit 画像用（`depth == 8` のときだけ呼ぶこと。16bit だと下位へ落ちる）。
    pub fn to_luma8_buffer(&self) -> image::ImageBuffer<image::Luma<u8>, Vec<u8>> {
        image::ImageBuffer::from_raw(
            self.width,
            self.height,
            self.data.iter().map(|&v| v as u8).collect(),
        )
        .expect("data length always matches width * height")
    }

    /// `bins` 個のビンに均等分割した輝度ヒストグラム。
    pub fn histogram(&self, bins: usize) -> Vec<u32> {
        let scale = bins as f32 / (self.max_value() as f32 + 1.0);
        // 大きめの塊ごとに部分ヒストグラムを作って足し合わせる
        // （塊が小さいとビン配列の確保と合算が勝ってしまう）。
        self.data
            .par_chunks(1 << 18)
            .map(|chunk| {
                let mut h = vec![0u32; bins];
                for &v in chunk {
                    let b = ((v as f32 * scale) as usize).min(bins - 1);
                    h[b] += 1;
                }
                h
            })
            .reduce(
                || vec![0u32; bins],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        *x += y;
                    }
                    a
                },
            )
    }

    /// 累積ヒストグラムから下側/上側 `frac` を切り捨てた輝度を返す（オートレベル用）。
    pub fn percentiles(&self, frac: f64) -> (u16, u16) {
        // 8bit 画像は 256 ビン（値そのもの）で数える。
        let hist = self.histogram(self.max_value() as usize + 1);
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

        let mut out = Self::black_with_depth(w, h, self.depth);
        out.data
            .par_chunks_mut(w as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let dy = y as f32 - cy;
                for (x, o) in row.iter_mut().enumerate() {
                    let dx = x as f32 - cx;
                    // 出力→入力の逆写像なので -theta の回転行列を掛ける。
                    let sx = cx + cos * dx + sin * dy;
                    let sy = cy - sin * dx + cos * dy;
                    *o = self.sample_bilinear(sx, sy);
                }
            });
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
        (top * (1.0 - fy) + bot * fy)
            .round()
            .clamp(0.0, self.max_value() as f32) as u16
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
        let kernel = gaussian_kernel(sigma);
        let radius = kernel.len() / 2;

        // 横 → 縦の順に畳み込む。端はクランプ（0 埋めだと縁が暗くなるため）。
        let mut tmp = vec![0f32; w * h];
        tmp.par_chunks_mut(w)
            .zip(self.data.par_chunks(w))
            .for_each(|(dst, src)| convolve_row(src, dst, &kernel));

        // 縦方向は行単位で積算する（列方向に飛ぶアクセスを避ける）。
        let mut out = Self::black_with_depth(self.width, self.height, self.depth);
        out.data.par_chunks_mut(w).enumerate().for_each_init(
            || vec![0f32; w],
            |acc, (y, row)| {
                acc.fill(0.0);
                for (i, &k) in kernel.iter().enumerate() {
                    let sy = (y + i).saturating_sub(radius).min(h - 1);
                    for (a, &v) in acc.iter_mut().zip(&tmp[sy * w..(sy + 1) * w]) {
                        *a += v * k;
                    }
                }
                for (o, &a) in row.iter_mut().zip(acc.iter()) {
                    *o = a.round().clamp(0.0, self.max_value() as f32) as u16;
                }
            },
        );
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
        let mut out = Self::black_with_depth(self.width, self.height, self.depth);
        out.data.par_chunks_mut(w).enumerate().for_each_init(
            || Vec::with_capacity((2 * r + 1) * (2 * r + 1)),
            |window, (y, row)| {
                let y0 = y.saturating_sub(r);
                let y1 = (y + r).min(h - 1);
                for (x, o) in row.iter_mut().enumerate() {
                    let x0 = x.saturating_sub(r);
                    let x1 = (x + r).min(w - 1);
                    window.clear();
                    for j in y0..=y1 {
                        window.extend_from_slice(&self.data[j * w + x0..=j * w + x1]);
                    }
                    let mid = window.len() / 2;
                    window.select_nth_unstable(mid);
                    *o = window[mid];
                }
            },
        );
        out
    }

    /// アンシャープマスク。`sigma` でぼかしたものとの差に `amount` を掛けて
    /// 元画像へ足し戻す（amount = 0 なら元のまま、1 が標準的な強さ）。
    pub fn unsharp_mask(&self, sigma: f32, amount: f32) -> Self {
        // ぼかした画像をそのまま出力先として上書きする（余分な確保をしない）。
        let mut out = self.gaussian_blur(sigma);
        out.data
            .par_iter_mut()
            .zip(self.data.par_iter())
            .for_each(|(o, &v)| {
                let s = v as f32 + amount * (v as f32 - *o as f32);
                *o = s.round().clamp(0.0, self.max_value() as f32) as u16;
            });
        out
    }

    /// `in_min`..`in_max` の輝度を 0..max_value へ線形に引き伸ばす。
    /// 入力のビット深度は保たれる（8bit 画像は 8bit のまま引き伸ばす）。
    pub fn apply_levels(&self, in_min: u16, in_max: u16) -> Self {
        let lo = in_min.min(in_max);
        let hi = in_min.max(in_max);
        let max = self.max_value();
        let mut lut = vec![0u16; max as usize + 1];
        if hi == lo {
            // 幅ゼロは二値化として扱う。
            for (v, out) in lut.iter_mut().enumerate() {
                *out = if v as u16 >= hi { max } else { 0 };
            }
        } else {
            let span = (hi - lo) as f32;
            for (v, out) in lut.iter_mut().enumerate() {
                let t = ((v as f32 - lo as f32) / span).clamp(0.0, 1.0);
                *out = (t * max as f32).round() as u16;
            }
        }
        Self {
            width: self.width,
            height: self.height,
            depth: self.depth,
            data: self.data.par_iter().map(|&v| lut[v as usize]).collect(),
        }
    }

    /// factor x factor のブロック平均で縮小する（表示用ミップの生成）。
    #[cfg(test)]
    pub fn downsample_box(&self, factor: u32) -> Self {
        let (width, height, data) = self.reduce_map(factor, |v| v);
        Self {
            width,
            height,
            depth: self.depth,
            data,
        }
    }

    /// 表示用テクスチャの元画像を作る。`factor` x `factor` のブロック平均で
    /// 縮小しつつ、表示レンジ `lo`..`hi` を 0..255 に写す。縮小画像を
    /// 中間に作らず 1 パスで変換する（等倍でも元画像の複製を作らない）。
    pub fn to_display_image(&self, factor: u32, lo: u16, hi: u16) -> ColorImage {
        let lut = display_lut(lo, hi, self.max_value());
        let (w, h, pixels) = self.reduce_map(factor, |v| Color32::from_gray(lut[v as usize]));
        ColorImage::new([w as usize, h as usize], pixels)
    }

    /// ブロック平均で縮小し、各画素を `map` で変換した列を返す（行優先）。
    fn reduce_map<T: Send>(
        &self,
        factor: u32,
        map: impl Fn(u16) -> T + Sync,
    ) -> (u32, u32, Vec<T>) {
        if factor <= 1 {
            return (
                self.width,
                self.height,
                self.data.par_iter().map(|&v| map(v)).collect(),
            );
        }
        let (w, h, f) = (self.width as usize, self.height as usize, factor as usize);
        let nw = (w / f).max(1);
        let nh = (h / f).max(1);
        let data = (0..nh)
            .into_par_iter()
            .flat_map_iter(|y| {
                let (y0, y1) = (y * f, ((y + 1) * f).min(h));
                let map = &map;
                (0..nw).map(move |x| {
                    let (x0, x1) = (x * f, ((x + 1) * f).min(w));
                    let sum: u64 = (y0..y1)
                        .map(|sy| {
                            self.data[sy * w + x0..sy * w + x1]
                                .iter()
                                .map(|&v| v as u64)
                                .sum::<u64>()
                        })
                        .sum();
                    let n = ((y1 - y0) * (x1 - x0)) as u64;
                    map(if n == 0 { 0 } else { (sum / n) as u16 })
                })
            })
            .collect();
        (nw as u32, nh as u32, data)
    }
}

/// 正規化したガウシアンカーネル（半径 3σ、最低 1）。
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let sigma = sigma.max(0.1);
    let radius = ((sigma * 3.0).ceil() as usize).max(1);
    let kernel: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let d = i as f32 - radius as f32;
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter().map(|k| k / sum).collect()
}

/// 1 行の横方向畳み込み。端はクランプ。縁だけ添字を丸め、内側は
/// 窓をそのまま掛ける（タップ毎の範囲判定を省く）。
fn convolve_row(src: &[u16], dst: &mut [f32], kernel: &[f32]) {
    let w = src.len();
    let r = kernel.len() / 2;
    let clamped = |x: usize| -> f32 {
        let mut acc = 0.0;
        for (i, &k) in kernel.iter().enumerate() {
            let sx = (x + i).saturating_sub(r).min(w - 1);
            acc += src[sx] as f32 * k;
        }
        acc
    };
    if w <= 2 * r {
        for (x, d) in dst.iter_mut().enumerate() {
            *d = clamped(x);
        }
        return;
    }
    for x in (0..r).chain(w - r..w) {
        dst[x] = clamped(x);
    }
    for x in r..w - r {
        let mut acc = 0.0;
        for (&v, &k) in src[x - r..=x + r].iter().zip(kernel) {
            acc += v as f32 * k;
        }
        dst[x] = acc;
    }
}

/// 表示レンジ `lo`..`hi` を 0..255 に写す 16bit → 8bit の変換表。
/// `max` は画素値の最大値（8bit 画像は 255、16bit 画像は 65535）。
/// レンジが幅ゼロ（一様画像など）のときは全域 0..max として扱う。
fn display_lut(lo: u16, hi: u16, max: u16) -> Vec<u8> {
    let (lo, hi) = if hi > lo { (lo, hi) } else { (0, max) };
    let span = (hi - lo) as f32;
    (0..=u16::MAX)
        .map(|v| {
            let t = ((v as f32 - lo as f32) / span).clamp(0.0, 1.0);
            (t * 255.0).round() as u8
        })
        .collect()
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
            depth: 16,
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
            depth: 16,
            data: vec![0, 1000, 2000, 3000, 4000],
        };
        let out = img.apply_levels(1000, 3000);
        assert_eq!(out.data[0], 0, "下限より暗い画素は 0");
        assert_eq!(out.data[1], 0);
        assert_eq!(out.data[2], 32768, "中点は中間輝度");
        assert_eq!(out.data[3], u16::MAX);
        assert_eq!(out.data[4], u16::MAX, "上限より明るい画素は最大値");
    }

    /// 8bit 画像は 16bit へ拡張されず、値もそのまま（×257 しない）であること。
    #[test]
    fn from_dynamic_keeps_8bit_values() {
        let img = image::DynamicImage::ImageLuma8(
            image::ImageBuffer::from_raw(2, 1, vec![0u8, 255]).unwrap(),
        );
        let gray = Gray16::from_dynamic(&img);
        assert_eq!(gray.depth, 8);
        assert_eq!(gray.max_value(), 255);
        assert_eq!(gray.data, vec![0, 255]);
    }

    /// レベル補正は入力のビット深度を保ち、8bit 画像は 0..255 へ引き伸ばす。
    #[test]
    fn levels_keeps_8bit_depth_and_stretches_to_255() {
        let img = Gray16 {
            width: 4,
            height: 1,
            depth: 8,
            data: vec![0, 100, 125, 255],
        };
        let out = img.apply_levels(100, 150);
        assert_eq!(out.depth, 8);
        assert_eq!(out.data[0], 0, "下限より暗い画素は 0");
        assert_eq!(out.data[1], 0);
        assert_eq!(out.data[2], 128, "中点は中間輝度 (255 の半分)");
        assert_eq!(out.data[3], 255, "上限より明るい画素は最大値");
    }

    /// 一様な 8bit 画像は、レンジ幅ゼロでも値がそのまま表示されること
    /// （0..65535 前提だと黒へ落ちてしまう）。
    #[test]
    fn flat_8bit_image_displays_as_its_value() {
        let img = Gray16 {
            width: 1,
            height: 1,
            depth: 8,
            data: vec![200],
        };
        let disp = img.to_display_image(1, 0, 0);
        assert_eq!(disp.pixels[0], Color32::from_gray(200));
    }

    /// 回転・ぼかしも入力のビット深度を保つ。
    #[test]
    fn rotate_and_blur_keep_8bit_depth() {
        let img = Gray16 {
            width: 16,
            height: 16,
            depth: 8,
            data: vec![100; 256],
        };
        let out = img.rotate(30.0);
        assert_eq!(out.depth, 8);
        assert!(out.data.iter().all(|&v| v <= 255), "8bit の範囲に収まる");
        let out = img.gaussian_blur(1.0);
        assert_eq!(out.depth, 8);
        assert_eq!(out.data, vec![100; 256], "一様画像はぼかしても変わらない");
    }

    /// 等倍の表示画像は LUT 変換だけで、縮小版はブロック平均を通すこと。
    #[test]
    fn display_image_maps_range_and_reduces() {
        let img = Gray16 {
            width: 2,
            height: 2,
            depth: 16,
            data: vec![0, 100, 200, 300],
        };
        let full = img.to_display_image(1, 0, 300);
        assert_eq!(full.size, [2, 2]);
        assert_eq!(full.pixels[3], Color32::from_gray(255));
        assert_eq!(full.pixels[0], Color32::from_gray(0));
        let half = img.to_display_image(2, 0, 300);
        assert_eq!(half.size, [1, 1]);
        // 平均 150 → 150/300*255 = 127.5 → 128
        assert_eq!(half.pixels[0], Color32::from_gray(128));
    }

    /// 縁の丸めと内側の窓掛けが同じ結果になること（逐次の定義どおり）。
    #[test]
    fn gaussian_blur_matches_naive_definition() {
        let mut img = Gray16::black(23, 5);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = ((i * 7919) % 65536) as u16;
        }
        let out = img.gaussian_blur(1.3);
        let kernel = gaussian_kernel(1.3);
        let r = kernel.len() / 2;
        let (w, h) = (23usize, 5usize);
        let mut tmp = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                tmp[y * w + x] = kernel
                    .iter()
                    .enumerate()
                    .map(|(i, &k)| {
                        img.data[y * w + (x + i).saturating_sub(r).min(w - 1)] as f32 * k
                    })
                    .fold(0.0, |a, b| a + b);
            }
        }
        for y in 0..h {
            for x in 0..w {
                let acc = kernel
                    .iter()
                    .enumerate()
                    .map(|(i, &k)| tmp[(y + i).saturating_sub(r).min(h - 1) * w + x] * k)
                    .fold(0.0, |a, b| a + b);
                assert_eq!(
                    out.at(x as u32, y as u32),
                    acc.round().clamp(0.0, 65535.0) as u16
                );
            }
        }
    }

    #[test]
    fn downsample_box_averages() {
        let img = Gray16 {
            width: 2,
            height: 2,
            depth: 16,
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
            depth: 16,
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
            depth: 16,
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
            depth: 16,
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
