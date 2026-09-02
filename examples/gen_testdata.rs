//! `testdata/` の TEM 風テスト画像を生成する。
//!
//! 実行: `cargo run --example gen_testdata`
//!
//! 乱数は固定シードの疑似乱数なので、何度実行しても同じ画像になる。
//! TIFF は無圧縮で書かれるため、リポジトリに置く都合でサイズは控えめにしてある。

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};

use image::{ImageBuffer, Luma, Rgb};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata");
    std::fs::create_dir_all(&dir)?;

    // TIFF には FEI / Thermo Fisher 形式の画素サイズ（メートル）を書き込む。
    write_tiff16(
        &dir.join("nanoparticles_16bit.tif"),
        nanoparticles(1024),
        Some((9.3517e-11, 9.3517e-11)),
    )?;
    write_tiff16(
        &dir.join("lattice_fringes_16bit.tif"),
        lattice_fringes(512),
        // 格子間隔 7.4 px がおよそ 0.2 nm になる画素サイズ。
        Some((2.7027e-11, 2.7027e-11)),
    )?;
    write_tiff8(
        &dir.join("carbon_support_8bit.tif"),
        carbon_support(1024),
        // x と y をわざと 4% 変えてある。差が 1% を超えると読み取り失敗になる
        // （異方性を 1 つのスケールへ潰さない）ことの確認用。
        Some((5.0e-10, 4.8e-10)),
    )?;
    // PNG にはスケール情報を持たせない（メタデータが無いときの挙動の確認用）。
    write_png16(
        &dir.join("low_contrast_12bit_in_16bit.png"),
        low_contrast(512),
    )?;
    write_rgb(&dir.join("color_sample.png"), color_sample(256))?;

    Ok(())
}

/// FEI / Thermo Fisher が私的タグ 34682 に入れる INI テキストを組み立てる。
fn fei_metadata(pixel_width_m: f64, pixel_height_m: f64) -> String {
    // 実機は CRLF 区切りで、値は 3 桁指数（9.3517e-011）で書かれる。
    [
        "[Beam]",
        "Beam=EBeam",
        "Scan=EScan",
        "[Scan]",
        "InternalScan=true",
        &format!("PixelWidth={}", fei_float(pixel_width_m)),
        &format!("PixelHeight={}", fei_float(pixel_height_m)),
        "",
    ]
    .join("\r\n")
}

/// `9.3517e-011` のような FEI 流の指数表記にする。
fn fei_float(v: f64) -> String {
    let exponent = v.abs().log10().floor() as i32;
    let mantissa = v / 10f64.powi(exponent);
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa:.4}e{sign}{:03}", exponent.abs())
}

// ------------------------------------------------------------------ 画像生成

/// 支持膜の上に載った暗いナノ粒子。レベル補正・測長の確認用。
/// 12bit 相当（0〜4095）の値しか使わないので、16bit のまま素直に表示すると暗い。
fn nanoparticles(n: u32) -> Vec<f64> {
    let mut rng = Rng::new(0x7EA_0001);
    let mut img = vec![0.0; (n * n) as usize];
    let center = (n as f64 - 1.0) * 0.5;

    // 粒子はあらかじめ位置と大きさを決めておく。
    let particles: Vec<(f64, f64, f64, f64)> = (0..45)
        .map(|_| {
            let cx = rng.range(40.0, n as f64 - 40.0);
            let cy = rng.range(40.0, n as f64 - 40.0);
            let r = rng.range(14.0, 58.0);
            let depth = rng.range(1400.0, 2500.0);
            (cx, cy, r, depth)
        })
        .collect();

    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f64, y as f64);
            // 照明むら（周辺減光）。
            let rr = ((fx - center).powi(2) + (fy - center).powi(2)) / (center * center);
            let mut v = 3050.0 * (1.0 - 0.22 * rr);
            // ゆっくりしたムラ。
            v += 60.0 * ((fx / 190.0).sin() + (fy / 240.0).cos());

            for &(cx, cy, r, depth) in &particles {
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / r;
                // 縁を少しぼかして、粒子内部ほど暗くする。
                let inside = 1.0 - smoothstep(0.86, 1.06, d);
                if inside > 0.0 {
                    v -= depth * inside * (0.75 + 0.25 * (1.0 - d).max(0.0));
                }
            }

            // ショットノイズ相当（信号量の平方根に比例）。実機の粒状感に寄せて強めにしてある。
            v += rng.normal() * (v.max(1.0)).sqrt() * 2.5;
            img[(y * n + x) as usize] = v.clamp(0.0, 4095.0);
        }
    }
    img
}

/// 傾いた格子縞。回転コマンドの角度確認に使いやすい。
fn lattice_fringes(n: u32) -> Vec<f64> {
    let mut rng = Rng::new(0x7EA_0002);
    let mut img = vec![0.0; (n * n) as usize];
    let center = (n as f64 - 1.0) * 0.5;
    let angle = 23.0_f64.to_radians();
    let k = TAU / 7.4; // 格子間隔 7.4 px

    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f64, y as f64);
            let (dx, dy) = (fx - center, fy - center);
            // 結晶粒の縁で縞が消えるようにする。
            let grain = 1.0 - smoothstep(0.78, 1.0, (dx * dx + dy * dy).sqrt() / center);

            let p1 = k * (fx * angle.cos() + fy * angle.sin());
            let p2 = k * (fx * (angle + TAU / 6.0).cos() + fy * (angle + TAU / 6.0).sin());
            let fringe = (p1.cos() + p2.cos()) * 0.5;

            let mut v = 1150.0 + grain * 520.0 * fringe;
            v += rng.normal() * 35.0;
            img[(y * n + x) as usize] = v.clamp(0.0, 65535.0);
        }
    }
    img
}

/// アモルファスカーボンの粒状感だけの 8bit 画像。8bit→16bit 拡張の確認用。
fn carbon_support(n: u32) -> Vec<f64> {
    let mut img = vec![0.0; (n * n) as usize];
    let octaves = [(8u32, 26.0), (16, 16.0), (48, 12.0)];
    for (cell, amp) in octaves {
        let noise = ValueNoise::new(0x7EA_0003 + cell as u64, n, cell);
        for y in 0..n {
            for x in 0..n {
                img[(y * n + x) as usize] += amp * noise.sample(x as f64, y as f64);
            }
        }
    }
    for v in &mut img {
        *v = (128.0 + *v).clamp(0.0, 255.0);
    }
    img
}

/// 輝度が 900〜1500 の狭い範囲にしかない画像。レベル補正の効きを見るため。
fn low_contrast(n: u32) -> Vec<f64> {
    let mut rng = Rng::new(0x7EA_0004);
    let mut img = vec![0.0; (n * n) as usize];
    let noise = ValueNoise::new(0x7EA_0005, n, 24);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f64, y as f64);
            let ramp = fx / (n as f64 - 1.0);
            let mut v = 900.0 + 400.0 * ramp + 90.0 * noise.sample(fx, fy);
            // 明るさの目印になる小さな四角。
            if (200..240).contains(&x) && (200..240).contains(&y) {
                v += 200.0;
            }
            v += rng.normal() * 8.0;
            img[(y * n + x) as usize] = v.clamp(0.0, 65535.0);
        }
    }
    img
}

/// カラー画像。読み込み時にグレースケール化されることの確認用。
fn color_sample(n: u32) -> Vec<[f64; 3]> {
    let mut img = vec![[0.0; 3]; (n * n) as usize];
    let center = (n as f64 - 1.0) * 0.5;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f64, y as f64);
            let mut rgb = [
                255.0 * fx / (n as f64 - 1.0),
                255.0 * fy / (n as f64 - 1.0),
                90.0,
            ];
            let d = ((fx - center).powi(2) + (fy - center).powi(2)).sqrt();
            if d < 60.0 {
                rgb = [230.0, 40.0, 40.0];
            }
            img[(y * n + x) as usize] = rgb;
        }
    }
    img
}

// ------------------------------------------------------------------ 書き出し

fn side(len: usize) -> u32 {
    (len as f64).sqrt() as u32
}

fn write_png16(path: &Path, data: Vec<f64>) -> Result<(), Box<dyn std::error::Error>> {
    let n = side(data.len());
    let pixels: Vec<u16> = data.iter().map(|&v| v.round() as u16).collect();
    let buf: ImageBuffer<Luma<u16>, _> =
        ImageBuffer::from_raw(n, n, pixels).ok_or("size mismatch")?;
    buf.save(path)?;
    report(path, n, None);
    Ok(())
}

/// 私的タグを書きたいので、`image` ではなく `tiff` クレートで直接書き出す。
fn write_tiff16(
    path: &Path,
    data: Vec<f64>,
    pixel_size_m: Option<(f64, f64)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pixels: Vec<u16> = data.iter().map(|&v| v.round() as u16).collect();
    write_tiff::<tiff::encoder::colortype::Gray16>(path, &pixels, side(data.len()), pixel_size_m)
}

fn write_tiff8(
    path: &Path,
    data: Vec<f64>,
    pixel_size_m: Option<(f64, f64)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pixels: Vec<u8> = data.iter().map(|&v| v.round() as u8).collect();
    write_tiff::<tiff::encoder::colortype::Gray8>(path, &pixels, side(data.len()), pixel_size_m)
}

fn write_tiff<C: tiff::encoder::colortype::ColorType>(
    path: &Path,
    pixels: &[C::Inner],
    n: u32,
    pixel_size_m: Option<(f64, f64)>,
) -> Result<(), Box<dyn std::error::Error>>
where
    [C::Inner]: tiff::encoder::TiffValue,
{
    let file = std::fs::File::create(path)?;
    let mut encoder = tiff::encoder::TiffEncoder::new(std::io::BufWriter::new(file))?;
    let mut image = encoder.new_image::<C>(n, n)?;
    if let Some((w, h)) = pixel_size_m {
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Unknown(34682), fei_metadata(w, h).as_str())?;
    }
    image.write_data(pixels)?;
    report(path, n, pixel_size_m);
    Ok(())
}

fn write_rgb(path: &Path, data: Vec<[f64; 3]>) -> Result<(), Box<dyn std::error::Error>> {
    let n = side(data.len());
    let pixels: Vec<u8> = data
        .iter()
        .flat_map(|c| c.iter().map(|&v| v.round() as u8).collect::<Vec<_>>())
        .collect();
    let buf: ImageBuffer<Rgb<u8>, _> =
        ImageBuffer::from_raw(n, n, pixels).ok_or("size mismatch")?;
    buf.save(path)?;
    report(path, n, None);
    Ok(())
}

fn report(path: &Path, n: u32, pixel_size_m: Option<(f64, f64)>) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let scale = match pixel_size_m {
        Some((w, h)) => format!("1 px = {:.5} × {:.5} nm", w * 1e9, h * 1e9),
        None => "スケール情報なし".to_owned(),
    };
    println!(
        "{:<34} {n}x{n}  {:>5.1} MB  {scale}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        size as f64 / 1_048_576.0
    );
}

// ------------------------------------------------------------------ ユーティリティ

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 固定シードの xorshift64*。テスト画像を毎回同じにするために使う。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    fn unit(&mut self) -> f64 {
        self.next_u32() as f64 / u32::MAX as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Box-Muller 法による標準正規乱数。
    fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
    }
}

/// 格子点の乱数を双一次補間する、素朴な value noise。
struct ValueNoise {
    grid: Vec<f64>,
    w: usize,
    cell: f64,
}

impl ValueNoise {
    fn new(seed: u64, size: u32, cell: u32) -> Self {
        let w = (size / cell) as usize + 2;
        let mut rng = Rng::new(seed);
        Self {
            grid: (0..w * w).map(|_| rng.range(-1.0, 1.0)).collect(),
            w,
            cell: cell as f64,
        }
    }

    fn sample(&self, x: f64, y: f64) -> f64 {
        let (gx, gy) = (x / self.cell, y / self.cell);
        let (x0, y0) = (gx.floor() as usize, gy.floor() as usize);
        let (fx, fy) = (gx - x0 as f64, gy - y0 as f64);
        let at = |x: usize, y: usize| self.grid[y.min(self.w - 1) * self.w + x.min(self.w - 1)];
        let (sx, sy) = (smoothstep(0.0, 1.0, fx), smoothstep(0.0, 1.0, fy));
        let top = at(x0, y0) * (1.0 - sx) + at(x0 + 1, y0) * sx;
        let bot = at(x0, y0 + 1) * (1.0 - sx) + at(x0 + 1, y0 + 1) * sx;
        top * (1.0 - sy) + bot * sy
    }
}
