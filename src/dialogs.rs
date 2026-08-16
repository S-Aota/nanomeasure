//! パラメータ編集用のポップアップ。
//!
//! いずれも「対象コマンドを実際に書き換えて再計算させる」方式なので、
//! 調整中の結果がそのまま画像表示に反映される（ライブプレビュー）。
//! キャンセル時は元の値に戻し、新規追加分だったコマンドは取り除く。

use std::sync::Arc;

use egui::{Color32, Context, Sense, Vec2};

use crate::command::Command;
use crate::document::Document;
use crate::frame::LengthUnit;
use crate::gray::Gray16;

/// 編集対象のコマンドを差し替え、必要な範囲だけ再計算対象にする。
fn set_command(doc: &mut Document, index: usize, cmd: Command) {
    if doc.commands[index].command != cmd {
        doc.commands[index].command = cmd;
        doc.invalidate_from(index);
    }
}

// -------------------------------------------------------------- スケール設定

/// 画素数と実寸法の対応をテキストで入力する。
/// 入力途中の文字列をそのまま保持し、数値として読めた行だけをコマンドへ反映する。
#[derive(Default)]
pub struct ScaleDialog {
    pub open: bool,
    index: usize,
    created: bool,
    original: Option<Command>,
    x_pixels: String,
    x_length: String,
    y_pixels: String,
    y_length: String,
    unit: LengthUnit,
    /// X の入力を Y にもそのまま使う。
    link_axes: bool,
}

impl ScaleDialog {
    pub fn open_new(&mut self, doc: &mut Document) {
        // 既定は現在有効なスケール。メタデータ由来の値があればそれが入る。
        let index = doc.push_command(Command::scale_from(doc.scale()));
        self.start(doc, index, true);
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands[index].command.clone();
        if let Command::SetScale {
            x_pixels,
            x_length,
            y_pixels,
            y_length,
            unit,
        } = original
        {
            self.x_pixels = format_number(x_pixels);
            self.x_length = format_number(x_length);
            self.y_pixels = format_number(y_pixels);
            self.y_length = format_number(y_length);
            self.unit = unit;
            self.link_axes = self.x_pixels == self.y_pixels && self.x_length == self.y_length;
        }
        self.index = index;
        self.created = created;
        self.original = Some(original);
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.open {
            return;
        }
        if self.index >= doc.commands.len() {
            self.open = false;
            return;
        }

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new("スケール設定")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.label("画素数と実寸法の対応を入力してください（スケールバーから読み取った値をそのまま入れられます）。");
                ui.add_space(8.0);

                egui::Grid::new("scale_grid")
                    .num_columns(5)
                    .spacing([6.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("X 方向");
                        ui.add(number_edit(&mut self.x_pixels));
                        ui.label("px  =");
                        ui.add(number_edit(&mut self.x_length));
                        egui::ComboBox::from_id_salt("scale_unit")
                            .selected_text(self.unit.label())
                            .width(64.0)
                            .show_ui(ui, |ui| {
                                for unit in LengthUnit::ALL {
                                    ui.selectable_value(&mut self.unit, unit, unit.label());
                                }
                            });
                        ui.end_row();

                        ui.label("Y 方向");
                        ui.add_enabled(!self.link_axes, number_edit(&mut self.y_pixels));
                        ui.label("px  =");
                        ui.add_enabled(!self.link_axes, number_edit(&mut self.y_length));
                        ui.label(self.unit.label());
                        ui.end_row();
                    });

                ui.checkbox(&mut self.link_axes, "Y 方向も X と同じにする");
                if self.link_axes {
                    self.y_pixels = self.x_pixels.clone();
                    self.y_length = self.x_length.clone();
                }

                ui.add_space(6.0);
                match self.pending() {
                    Some(cmd) => match cmd.scale() {
                        Some(scale) => {
                            ui.label(scale.describe());
                        }
                        None => {
                            ui.colored_label(
                                Color32::from_rgb(255, 140, 140),
                                "画素数と実寸法には正の値を入れてください。",
                            );
                        }
                    },
                    None => {
                        ui.colored_label(
                            Color32::from_rgb(255, 140, 140),
                            "数値として読めない入力があります。",
                        );
                    }
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let valid = self.pending().and_then(|c| c.scale()).is_some();
                    if ui
                        .add_enabled(valid, egui::Button::new("決定"))
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button("キャンセル").clicked() {
                        cancelled = true;
                    }
                });
            });

        // 数値として成立している間だけ反映する（入力途中でエラーを出さないため）。
        if let Some(cmd) = self.pending()
            && cmd.scale().is_some()
        {
            set_command(doc, self.index, cmd);
        }

        if cancelled || !window_open {
            self.revert(doc);
            self.open = false;
        } else if confirmed {
            self.original = None;
            self.open = false;
        }
    }

    /// 現在の入力欄からコマンドを組み立てる。数値として読めなければ `None`。
    fn pending(&self) -> Option<Command> {
        Some(Command::SetScale {
            x_pixels: self.x_pixels.trim().parse().ok()?,
            x_length: self.x_length.trim().parse().ok()?,
            y_pixels: self.y_pixels.trim().parse().ok()?,
            y_length: self.y_length.trim().parse().ok()?,
            unit: self.unit,
        })
    }

    fn revert(&mut self, doc: &mut Document) {
        if self.created {
            doc.remove_command(self.index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, self.index, original);
        }
    }
}

fn number_edit(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text)
        .desired_width(90.0)
        .horizontal_align(egui::Align::Max)
}

/// 末尾の余計な 0 を落として、編集しやすい文字列にする。
fn format_number(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

// ---------------------------------------------------------------- 画像回転

#[derive(Default)]
pub struct RotateDialog {
    pub open: bool,
    index: usize,
    created: bool,
    original: Option<Command>,
    angle: f32,
}

impl RotateDialog {
    pub fn open_new(&mut self, doc: &mut Document) {
        let index = doc.push_command(Command::Rotate { angle_deg: 0.0 });
        self.start(doc, index, true);
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands[index].command.clone();
        self.angle = match original {
            Command::Rotate { angle_deg } => angle_deg,
            _ => 0.0,
        };
        self.index = index;
        self.created = created;
        self.original = Some(original);
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.open {
            return;
        }
        if self.index >= doc.commands.len() {
            self.open = false;
            return;
        }

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new("画像の回転")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("画像サイズを保ったまま中心まわりに回転します（時計回り）。");
                ui.label("はみ出た部分は失われ、埋まらない部分は黒になります。");
                ui.add_space(8.0);
                ui.add(
                    egui::Slider::new(&mut self.angle, 0.0..=360.0)
                        .suffix(" °")
                        .text("角度"),
                );
                ui.horizontal(|ui| {
                    for preset in [0.0, 90.0, 180.0, 270.0] {
                        if ui.button(format!("{preset:.0}°")).clicked() {
                            self.angle = preset;
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("決定").clicked() {
                        confirmed = true;
                    }
                    if ui.button("キャンセル").clicked() {
                        cancelled = true;
                    }
                });
            });

        // ライブプレビュー: スライダーを動かすたびに結果へ反映する。
        set_command(
            doc,
            self.index,
            Command::Rotate {
                angle_deg: self.angle,
            },
        );

        if cancelled || !window_open {
            self.revert(doc);
            self.open = false;
        } else if confirmed {
            self.original = None;
            self.open = false;
        }
    }

    fn revert(&mut self, doc: &mut Document) {
        if self.created {
            doc.remove_command(self.index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, self.index, original);
        }
    }
}

// -------------------------------------------------------------- レベル補正

const HIST_BINS: usize = 256;

#[derive(Default)]
pub struct LevelsDialog {
    pub open: bool,
    index: usize,
    created: bool,
    original: Option<Command>,
    in_min: u16,
    in_max: u16,
    log_scale: bool,
    /// ヒストグラムは入力画像が変わったときだけ計算し直す。
    hist_source: Option<Arc<Gray16>>,
    hist: Vec<u32>,
}

impl LevelsDialog {
    pub fn open_new(&mut self, doc: &mut Document) {
        // 既定値は入力画像の実測レンジ。いきなり真っ白/真っ黒にならないようにする。
        let (lo, hi) = doc
            .image()
            .map_or((0, u16::MAX), |img| img.percentiles(0.001));
        let index = doc.push_command(Command::Levels {
            in_min: lo,
            in_max: hi,
        });
        self.start(doc, index, true);
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands[index].command.clone();
        match original {
            Command::Levels { in_min, in_max } => {
                self.in_min = in_min;
                self.in_max = in_max;
            }
            _ => {
                self.in_min = 0;
                self.in_max = u16::MAX;
            }
        }
        self.index = index;
        self.created = created;
        self.original = Some(original);
        self.hist_source = None;
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.open {
            return;
        }
        if self.index >= doc.commands.len() {
            self.open = false;
            return;
        }
        self.refresh_histogram(doc);

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        let mut auto = false;
        let mut reset = false;

        egui::Window::new("レベル補正")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label("横軸=輝度のヒストグラムです。最小・最大を決めると、その間の輝度が 0〜65535 へ線形に引き伸ばされます。");
                ui.add_space(6.0);
                self.draw_histogram(ui);
                ui.add_space(6.0);
                ui.add(
                    egui::Slider::new(&mut self.in_min, 0..=u16::MAX)
                        .text("最小輝度")
                        .clamping(egui::SliderClamping::Always),
                );
                ui.add(
                    egui::Slider::new(&mut self.in_max, 0..=u16::MAX)
                        .text("最大輝度")
                        .clamping(egui::SliderClamping::Always),
                );
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.log_scale, "対数目盛");
                    if ui.button("自動 (0.1%)").clicked() {
                        auto = true;
                    }
                    if ui.button("全域").clicked() {
                        reset = true;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("決定").clicked() {
                        confirmed = true;
                    }
                    if ui.button("キャンセル").clicked() {
                        cancelled = true;
                    }
                });
            });

        if auto && let Some(src) = &self.hist_source {
            let (lo, hi) = src.percentiles(0.001);
            self.in_min = lo;
            self.in_max = hi;
        }
        if reset {
            self.in_min = 0;
            self.in_max = u16::MAX;
        }
        if self.in_min > self.in_max {
            self.in_max = self.in_min;
        }

        set_command(
            doc,
            self.index,
            Command::Levels {
                in_min: self.in_min,
                in_max: self.in_max,
            },
        );

        if cancelled || !window_open {
            self.revert(doc);
            self.open = false;
        } else if confirmed {
            self.original = None;
            self.open = false;
        }
    }

    /// このコマンドへ入ってくる画像（= 直前段の結果）のヒストグラムを用意する。
    fn refresh_histogram(&mut self, doc: &Document) {
        let source = doc.input_to(self.index).map(|f| f.image.clone());
        let same = match (&self.hist_source, &source) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.hist = source
            .as_ref()
            .map_or_else(Vec::new, |img| img.histogram(HIST_BINS));
        self.hist_source = source;
    }

    fn draw_histogram(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(400.0, 150.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, Color32::from_gray(28));

        if !self.hist.is_empty() {
            let peak = self.hist.iter().copied().max().unwrap_or(1).max(1) as f32;
            let bar_w = rect.width() / HIST_BINS as f32;
            for (i, &count) in self.hist.iter().enumerate() {
                let t = if self.log_scale {
                    (count as f32 + 1.0).ln() / (peak + 1.0).ln()
                } else {
                    count as f32 / peak
                };
                let h = t * rect.height();
                if h <= 0.0 {
                    continue;
                }
                let x = rect.left() + i as f32 * bar_w;
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x, rect.bottom() - h),
                        egui::pos2(x + bar_w.max(1.0), rect.bottom()),
                    ),
                    0.0,
                    Color32::from_gray(190),
                );
            }
        }

        // 最小・最大を示す縦線。
        for (value, color, label) in [
            (self.in_min, Color32::from_rgb(90, 170, 255), "min"),
            (self.in_max, Color32::from_rgb(255, 140, 90), "max"),
        ] {
            let x = rect.left() + rect.width() * (value as f32 / 65535.0);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.5, color),
            );
            painter.text(
                egui::pos2(x + 3.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::monospace(10.0),
                color,
            );
        }
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(1.0, Color32::from_gray(70)),
            egui::StrokeKind::Inside,
        );
    }

    fn revert(&mut self, doc: &mut Document) {
        if self.created {
            doc.remove_command(self.index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, self.index, original);
        }
    }
}
