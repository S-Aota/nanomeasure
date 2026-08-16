//! 画像表示領域。ズーム・パン・表示用テクスチャの管理。
//!
//! 方針: 処理済み画像を一度だけ GPU テクスチャへ載せ、パンとズームは
//! 描画先の矩形を変えるだけで済ませる（CPU 側の再サンプルは毎フレーム発生しない）。
//! 大きく縮小したときの折り返しノイズを避けるため、テクスチャは
//! 「表示倍率が 0.5〜1.0 倍に収まる 2 の冪」の縮小段（ミップ）で作る。
//! ホイール操作中は今のテクスチャを引き伸ばして即座に返し、
//! 倍率が落ち着いてから必要な段のテクスチャを作り直す。

use std::sync::Arc;

use egui::{Color32, Rect, Sense, TextureHandle, TextureOptions, Ui, Vec2};

use crate::gray::Gray16;

const MIN_ZOOM: f32 = 0.01;
const MAX_ZOOM: f32 = 64.0;
/// ズームが止まってからテクスチャを作り直すまでの待ち時間（秒）。
const SETTLE_SECS: f32 = 0.15;

#[derive(Clone, Copy, PartialEq)]
struct TexKey {
    generation: u64,
    lo: u16,
    hi: u16,
    level: u32,
}

pub struct ImageView {
    /// 画像 1 px あたりの画面 px 数。
    pub zoom: f32,
    /// 表示領域の左上から見た、画像左上のオフセット（画面 px）。
    pan: Vec2,
    /// 次に描くときに表示領域へフィットさせる。
    needs_fit: bool,
    tex: Option<TextureHandle>,
    tex_key: Option<TexKey>,
    /// 倍率が変化しなくなってからの経過時間。
    settle: f32,
    last_zoom: f32,
}

impl Default for ImageView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
            needs_fit: true,
            tex: None,
            tex_key: None,
            settle: 0.0,
            last_zoom: 1.0,
        }
    }
}

/// 表示中にカーソル下から拾えた情報（ステータスバー用）。
#[derive(Default)]
pub struct ViewInfo {
    pub hover_px: Option<(u32, u32)>,
    pub hover_value: Option<u16>,
}

impl ImageView {
    /// 次のフレームで表示領域にフィットさせる。
    pub fn request_fit(&mut self) {
        self.needs_fit = true;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        img: Option<&Arc<Gray16>>,
        generation: u64,
        range: (u16, u16),
    ) -> ViewInfo {
        let vp = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(vp, Sense::click_and_drag());
        let painter = ui.painter_at(vp);
        painter.rect_filled(vp, 0.0, Color32::from_gray(24));

        let Some(img) = img else {
            painter.text(
                vp.center(),
                egui::Align2::CENTER_CENTER,
                "ここに画像をドラッグ&ドロップ\n（ファイル → 画像を挿入 でも開けます）",
                egui::FontId::proportional(16.0),
                Color32::from_gray(140),
            );
            return ViewInfo::default();
        };

        if self.needs_fit {
            self.fit(vp.size(), img);
            self.needs_fit = false;
        }

        self.handle_input(&response, vp);
        self.update_texture(ui, img, generation, range);

        let mut info = ViewInfo::default();
        if let Some(tex) = &self.tex {
            let size = Vec2::new(img.width as f32, img.height as f32) * self.zoom;
            let image_rect = Rect::from_min_size(vp.min + self.pan, size);
            painter.image(
                tex.id(),
                image_rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.rect_stroke(
                image_rect,
                0.0,
                egui::Stroke::new(1.0, Color32::from_gray(80)),
                egui::StrokeKind::Outside,
            );

            if let Some(pos) = response.hover_pos() {
                let rel = (pos - image_rect.min) / self.zoom;
                if rel.x >= 0.0
                    && rel.y >= 0.0
                    && (rel.x as u32) < img.width
                    && (rel.y as u32) < img.height
                {
                    let (x, y) = (rel.x as u32, rel.y as u32);
                    info.hover_px = Some((x, y));
                    info.hover_value = Some(img.at(x, y));
                }
            }
        }
        info
    }

    fn fit(&mut self, viewport: Vec2, img: &Gray16) {
        if img.width == 0 || img.height == 0 {
            return;
        }
        let scale = (viewport.x / img.width as f32)
            .min(viewport.y / img.height as f32)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom = scale;
        self.last_zoom = scale;
        let shown = Vec2::new(img.width as f32, img.height as f32) * scale;
        self.pan = (viewport - shown) * 0.5;
    }

    fn handle_input(&mut self, response: &egui::Response, vp: Rect) {
        if response.dragged_by(egui::PointerButton::Primary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            self.pan += response.drag_delta();
        }

        if response.hovered() {
            let (scroll_y, pinch) = response
                .ctx
                .input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let mut factor = pinch;
            if scroll_y != 0.0 {
                factor *= (scroll_y * 0.0022).exp();
            }
            if factor != 1.0 {
                let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                // カーソル位置の画素が動かないようにパンを補正する。
                let anchor =
                    response.hover_pos().map_or(vp.center(), |p| p).to_vec2() - vp.min.to_vec2();
                let image_pt = (anchor - self.pan) / self.zoom;
                self.pan = anchor - image_pt * new_zoom;
                self.zoom = new_zoom;
            }
        }

        let dt = response.ctx.input(|i| i.stable_dt).min(0.1);
        if (self.zoom - self.last_zoom).abs() > f32::EPSILON {
            self.settle = 0.0;
            self.last_zoom = self.zoom;
        } else {
            self.settle += dt;
        }
    }

    /// 今の倍率に合うミップ段を選び、必要ならテクスチャを作り直す。
    fn update_texture(&mut self, ui: &Ui, img: &Arc<Gray16>, generation: u64, range: (u16, u16)) {
        let max_side = ui.ctx().input(|i| i.max_texture_side) as u32;
        let ideal = ideal_level(self.zoom, img.width, img.height, max_side);

        let current = self.tex_key;
        let content_changed = current
            .is_none_or(|k| k.generation != generation || k.lo != range.0 || k.hi != range.1);
        let level_stale = current.is_some_and(|k| k.level != ideal);

        // 内容が変わったら即時、倍率だけの変化なら操作が落ち着いてから作り直す。
        let rebuild = content_changed || (level_stale && self.settle >= SETTLE_SECS);
        if level_stale && !rebuild {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f32(SETTLE_SECS));
        }
        if !rebuild {
            return;
        }

        let reduced = img.downsample_box(ideal);
        let color = reduced.to_color_image(range.0, range.1);
        let options = TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            ..Default::default()
        };
        match &mut self.tex {
            Some(tex) => tex.set(color, options),
            None => self.tex = Some(ui.ctx().load_texture("tem_view", color, options)),
        }
        self.tex_key = Some(TexKey {
            generation,
            lo: range.0,
            hi: range.1,
            level: ideal,
        });
    }
}

/// 表示倍率が 0.5〜1.0 倍に収まる最大の 2 の冪。GPU の最大テクスチャ辺も超えない。
fn ideal_level(zoom: f32, width: u32, height: u32, max_side: u32) -> u32 {
    let mut min_level = 1;
    while width.div_ceil(min_level) > max_side || height.div_ceil(min_level) > max_side {
        min_level *= 2;
    }
    let mut level = 1u32;
    if zoom > 0.0 {
        let budget = 1.0 / zoom;
        while (level as f32) * 2.0 <= budget && level < 1024 {
            level *= 2;
        }
    }
    level.max(min_level)
}
