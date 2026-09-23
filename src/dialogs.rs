//! パラメータ編集用のポップアップ。
//!
//! いずれも「対象コマンドを実際に書き換えて再計算させる」方式なので、
//! 調整中の結果がそのまま画像表示に反映される（ライブプレビュー）。
//! キャンセル時は元の値に戻し、新規追加分だったコマンドは取り除く。

use std::sync::Arc;

use egui::{Color32, Context, Sense, Vec2};

use rust_i18n::t;

use crate::command::{Command, Filter, FilterKind};
use crate::document::Document;
use crate::frame::LengthUnit;
use crate::gray::Gray16;
use crate::settings::{Language, Settings};

/// 編集対象のコマンドを差し替え、必要な範囲だけ再計算対象にする。
pub(crate) fn set_command(doc: &mut Document, index: usize, cmd: Command) {
    let Some(item) = doc.commands.get_mut(index) else {
        return;
    };
    if item.command != cmd {
        item.command = cmd;
        doc.invalidate_from(index);
    }
}

// -------------------------------------------------------------- スケール設定

/// 画素数と実寸法の対応をテキストで入力する。
/// 入力途中の文字列をそのまま保持し、数値として読めた行だけをコマンドへ反映する。
#[derive(Default)]
pub struct ScaleDialog {
    pub open: bool,
    /// 編集対象のコマンド。`None` はまだ挿入していない新規設定（空欄スタート）。
    index: Option<usize>,
    created: bool,
    original: Option<Command>,
    pixels: String,
    length: String,
    unit: LengthUnit,
}

impl ScaleDialog {
    /// 新しいスケール設定を始める。有効なスケールがあれば、それを
    /// 編集しやすい値に戻して入れる。無ければ空欄で始める
    /// （メタデータが無い画像に、勝手な既定値を入れないため）。
    pub fn open_new(&mut self, doc: &mut Document) {
        match doc.scale() {
            Some(scale) => {
                let index = doc.push_command(Command::scale_from(scale));
                self.start(doc, index, true);
            }
            None => {
                self.pixels.clear();
                self.length.clear();
                self.unit = LengthUnit::Nanometer;
                self.index = None;
                self.created = false;
                self.original = None;
                self.open = true;
            }
        }
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands.get(index).map(|c| c.command.clone());
        if let Some(Command::SetScale {
            pixels,
            length,
            unit,
        }) = original
        {
            self.pixels = format_number(pixels);
            self.length = format_number(length);
            self.unit = unit;
        }
        self.index = Some(index);
        self.created = created;
        self.original = original;
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, digits: u8) {
        if !self.open {
            return;
        }
        if self.index.is_some_and(|i| i >= doc.commands.len()) {
            self.open = false;
            return;
        }

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new(t!("dlg.scale_title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.label(t!("dlg.scale_intro").as_ref());
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.add(number_edit(&mut self.pixels));
                    ui.label("px  =");
                    ui.add(number_edit(&mut self.length));
                    egui::ComboBox::from_id_salt("scale_unit")
                        .selected_text(self.unit.label())
                        .width(64.0)
                        .show_ui(ui, |ui| {
                            for unit in LengthUnit::ALL {
                                ui.selectable_value(&mut self.unit, unit, unit.label());
                            }
                        });
                });

                ui.add_space(6.0);
                match self.pending() {
                    Some(cmd) => match cmd.scale() {
                        Some(scale) => {
                            ui.label(scale.describe(digits));
                        }
                        None => {
                            ui.colored_label(
                                Color32::from_rgb(255, 140, 140),
                                t!("dlg.scale_positive").as_ref(),
                            );
                        }
                    },
                    None => {
                        ui.colored_label(
                            Color32::from_rgb(255, 140, 140),
                            t!("dlg.scale_not_number").as_ref(),
                        );
                    }
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let valid = self.pending().and_then(|c| c.scale()).is_some();
                    if ui
                        .add_enabled(valid, egui::Button::new(t!("dlg.ok").as_ref()))
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button(t!("dlg.cancel").as_ref()).clicked() {
                        cancelled = true;
                    }
                });
            });

        // 数値として成立している間だけ反映する（入力途中でエラーを出さないため）。
        if let Some(cmd) = self.pending()
            && cmd.scale().is_some()
        {
            match self.index {
                Some(index) => set_command(doc, index, cmd),
                None => {
                    // 空欄スタートの新規設定は、初めて正しい値が揃った時点で挿入する。
                    let index = doc.push_command(cmd);
                    self.index = Some(index);
                    self.created = true;
                }
            }
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
            pixels: self.pixels.trim().parse().ok()?,
            length: self.length.trim().parse().ok()?,
            unit: self.unit,
        })
    }

    fn revert(&mut self, doc: &mut Document) {
        let Some(index) = self.index else {
            return;
        };
        if self.created {
            doc.remove_command(index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, index, original);
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
        let original = doc.commands.get(index).map(|c| c.command.clone());
        self.angle = match original {
            Some(Command::Rotate { angle_deg }) => angle_deg,
            _ => 0.0,
        };
        self.index = index;
        self.created = created;
        self.original = original;
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

        egui::Window::new(t!("dlg.rotate_title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(t!("dlg.rotate_keep_size").as_ref());
                ui.label(t!("dlg.rotate_clip").as_ref());
                ui.add_space(8.0);
                ui.add(
                    egui::Slider::new(&mut self.angle, 0.0..=360.0)
                        .suffix(" °")
                        .text(t!("dlg.angle").as_ref()),
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
                    if ui.button(t!("dlg.ok").as_ref()).clicked() {
                        confirmed = true;
                    }
                    if ui.button(t!("dlg.cancel").as_ref()).clicked() {
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
    /// 入力画像の画素値の最大値（8bit なら 255、16bit なら 65535）。
    /// スライダーの範囲とヒストグラムの目盛りに使う。
    max_value: u16,
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
        let original = doc.commands.get(index).map(|c| c.command.clone());
        // 入力画像の深度に合わせてスライダーの上限を決める。
        // 画像が無いうちは 16bit の範囲としておく（履歴から開いた場合も
        // ここで入力の範囲へ収める）。
        self.max_value = doc.image().map_or(u16::MAX, |img| img.max_value());
        match original {
            Some(Command::Levels { in_min, in_max }) => {
                self.in_min = in_min.min(self.max_value);
                self.in_max = in_max.min(self.max_value);
            }
            _ => {
                self.in_min = 0;
                self.in_max = self.max_value;
            }
        }
        self.index = index;
        self.created = created;
        self.original = original;
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

        egui::Window::new(t!("dlg.levels_title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(t!("dlg.levels_histogram", max = self.max_value).as_ref());
                ui.add_space(6.0);
                self.draw_histogram(ui);
                ui.add_space(6.0);
                ui.add(
                    egui::Slider::new(&mut self.in_min, 0..=self.max_value)
                        .text(t!("dlg.min_brightness").as_ref())
                        .clamping(egui::SliderClamping::Always),
                );
                ui.add(
                    egui::Slider::new(&mut self.in_max, 0..=self.max_value)
                        .text(t!("dlg.max_brightness").as_ref())
                        .clamping(egui::SliderClamping::Always),
                );
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.log_scale, t!("dlg.log_scale").as_ref());
                    if ui.button(t!("dlg.auto_01").as_ref()).clicked() {
                        auto = true;
                    }
                    if ui.button(t!("dlg.full_range").as_ref()).clicked() {
                        reset = true;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t!("dlg.ok").as_ref()).clicked() {
                        confirmed = true;
                    }
                    if ui.button(t!("dlg.cancel").as_ref()).clicked() {
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
            self.in_max = self.max_value;
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
        // スライダー・目盛りの上限も実際の入力画像の深度に合わせる。
        self.max_value = source.as_ref().map_or(u16::MAX, |img| img.max_value());
        // 上限が下がったら（画像が出て 8bit と分かった等）値も範囲へ収める。
        self.in_min = self.in_min.min(self.max_value);
        self.in_max = self.in_max.min(self.max_value);
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
            let x = rect.left() + rect.width() * (value as f32 / self.max_value as f32);
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

// -------------------------------------------------------------- 画像出力

/// アノテーション付き画像の出力先テンプレートと形式を入力する。
/// 形式は拡張子で指定する（tif / png / jpg）。
#[derive(Default)]
pub struct ExportDialog {
    pub open: bool,
    index: Option<usize>,
    created: bool,
    original: Option<Command>,
    output: String,
    annotation_scale: f32,
    color: bool,
}

impl ExportDialog {
    pub fn open_new(&mut self, doc: &mut Document) {
        let index = doc.push_command(Command::ExportImage {
            output: crate::export::DEFAULT_EXPORT_PATH.to_owned(),
            annotation_scale: 1.0,
            color: false,
        });
        self.start(doc, index, true);
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands.get(index).map(|c| c.command.clone());
        match &original {
            Some(Command::ExportImage {
                output,
                annotation_scale,
                color,
            }) => {
                self.output = output.clone();
                self.annotation_scale = *annotation_scale;
                self.color = *color;
            }
            _ => {
                self.output = crate::export::DEFAULT_EXPORT_PATH.to_owned();
                self.annotation_scale = 1.0;
                self.color = false;
            }
        }
        self.index = Some(index);
        self.created = created;
        self.original = original;
        self.open = true;
    }

    /// ウィンドウを表示する。「決定」はコマンドを確定するだけで、
    /// ファイル保存は「再計算」(F5) のときに行う。
    pub fn show(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.open {
            return;
        }
        if self.index.is_some_and(|i| i >= doc.commands.len()) {
            self.open = false;
            return;
        }
        let index = self.index.expect("open なら index あり");

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new(t!("dlg.export_title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label(t!("dlg.export_intro").as_ref());
                ui.label(t!("dlg.export_ext").as_ref());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(t!("dlg.output_path").as_ref());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.output)
                            .desired_width(330.0)
                            .hint_text(crate::export::DEFAULT_EXPORT_PATH),
                    );
                });
                ui.label(t!("dlg.template_hint").as_ref());
                let path = std::path::Path::new(self.output.trim());
                if self.output.trim().is_empty() {
                    ui.colored_label(
                        Color32::from_rgb(255, 140, 140),
                        t!("dlg.enter_output").as_ref(),
                    );
                } else if !crate::export::validate_extension(path) {
                    ui.colored_label(
                        Color32::from_rgb(255, 140, 140),
                        t!("dlg.unsupported_ext").as_ref(),
                    );
                }
                ui.add_space(8.0);
                ui.checkbox(&mut self.color, t!("dlg.export_color").as_ref())
                    .on_hover_text(t!("dlg.export_color_hover").as_ref());
                ui.add_space(8.0);
                ui.add(
                    egui::Slider::new(&mut self.annotation_scale, 0.5..=4.0)
                        .text(t!("dlg.annotation_scale").as_ref()),
                );
                ui.label(t!("dlg.annotation_scale_hint").as_ref());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let valid =
                        !self.output.trim().is_empty() && crate::export::validate_extension(path);
                    if ui
                        .add_enabled(valid, egui::Button::new(t!("dlg.ok").as_ref()))
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button(t!("dlg.cancel").as_ref()).clicked() {
                        cancelled = true;
                    }
                });
            });

        // ライブ反映（他ダイアログと同じ方式。このコマンド自体にプレビューは無い）。
        set_command(
            doc,
            index,
            Command::ExportImage {
                output: self.output.clone(),
                annotation_scale: self.annotation_scale,
                color: self.color,
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
        let Some(index) = self.index else {
            return;
        };
        if self.created {
            doc.remove_command(index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, index, original);
        }
    }
}

// -------------------------------------------------------------- 結果出力

/// 測定結果 JSON の出力先テンプレートを入力する（画像出力と同じ方式）。
#[derive(Default)]
pub struct ExportResultDialog {
    pub open: bool,
    index: Option<usize>,
    created: bool,
    original: Option<Command>,
    output: String,
}

impl ExportResultDialog {
    pub fn open_new(&mut self, doc: &mut Document) {
        let index = doc.push_command(Command::ExportResult {
            output: crate::export::DEFAULT_RESULT_PATH.to_owned(),
        });
        self.start(doc, index, true);
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands.get(index).map(|c| c.command.clone());
        self.output = match &original {
            Some(Command::ExportResult { output }) => output.clone(),
            _ => crate::export::DEFAULT_RESULT_PATH.to_owned(),
        };
        self.index = Some(index);
        self.created = created;
        self.original = original;
        self.open = true;
    }

    /// ウィンドウを表示する。「決定」はコマンドを確定するだけで、
    /// ファイル保存は「再計算」(F5) のときに行う。
    pub fn show(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.open {
            return;
        }
        if self.index.is_some_and(|i| i >= doc.commands.len()) {
            self.open = false;
            return;
        }
        let index = self.index.expect("open なら index あり");

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new(t!("dlg.result_title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label(t!("dlg.result_intro").as_ref());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(t!("dlg.output_path").as_ref());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.output)
                            .desired_width(330.0)
                            .hint_text(crate::export::DEFAULT_RESULT_PATH),
                    );
                });
                ui.label(t!("dlg.template_hint").as_ref());
                if self.output.trim().is_empty() {
                    ui.colored_label(
                        Color32::from_rgb(255, 140, 140),
                        t!("dlg.enter_output").as_ref(),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let valid = !self.output.trim().is_empty();
                    if ui
                        .add_enabled(valid, egui::Button::new(t!("dlg.ok").as_ref()))
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button(t!("dlg.cancel").as_ref()).clicked() {
                        cancelled = true;
                    }
                });
            });

        // ライブ反映（他ダイアログと同じ方式。このコマンド自体にプレビューは無い）。
        set_command(
            doc,
            index,
            Command::ExportResult {
                output: self.output.clone(),
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
        let Some(index) = self.index else {
            return;
        };
        if self.created {
            doc.remove_command(index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, index, original);
        }
    }
}

// -------------------------------------------------------------- フィルタ

/// 前処理フィルタのパラメータ編集。処理が重いので、スライダーをドラッグ
/// している間は反映せず、離したときに 1 回だけ適用する（ライブプレビューが
/// 毎フレームの再計算にならないように）。クリックやキー入力など、
/// ドラッグ以外の変更はその場で適用する。
#[derive(Default)]
pub struct FilterDialog {
    pub open: bool,
    index: Option<usize>,
    created: bool,
    original: Option<Command>,
    filter: Filter,
}

impl FilterDialog {
    /// 新しいフィルタコマンドを追加して編集を始める。既定値は `initial`。
    pub fn open_new(&mut self, doc: &mut Document, initial: Filter) {
        let index = doc.push_command(Command::Filter { filter: initial });
        self.start(doc, index, true);
    }

    pub fn open_edit(&mut self, doc: &mut Document, index: usize) {
        self.start(doc, index, false);
    }

    fn start(&mut self, doc: &Document, index: usize, created: bool) {
        let original = doc.commands.get(index).map(|c| c.command.clone());
        self.filter = match original {
            Some(Command::Filter { filter }) => filter,
            _ => Filter::GaussianBlur { sigma: 1.0 },
        };
        self.index = Some(index);
        self.created = created;
        self.original = original;
        self.open = true;
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.open {
            return;
        }
        if self.index.is_some_and(|i| i >= doc.commands.len()) {
            self.open = false;
            return;
        }
        let index = self.index.expect("open なら index あり");

        let mut window_open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        let mut apply = false;

        egui::Window::new(t!("dlg.filter_title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.label(t!("dlg.filter_intro").as_ref());
                ui.add_space(8.0);
                let mut kind = self.filter.kind();
                ui.horizontal(|ui| {
                    ui.radio_value(
                        &mut kind,
                        FilterKind::GaussianBlur,
                        t!("dlg.filter_gaussian").as_ref(),
                    );
                    ui.radio_value(
                        &mut kind,
                        FilterKind::Median,
                        t!("dlg.filter_median").as_ref(),
                    );
                    ui.radio_value(
                        &mut kind,
                        FilterKind::UnsharpMask,
                        t!("dlg.filter_unsharp").as_ref(),
                    );
                });
                if kind != self.filter.kind() {
                    self.filter = Filter::default_of(kind);
                    apply = true;
                }
                ui.add_space(8.0);

                match &mut self.filter {
                    Filter::GaussianBlur { sigma } => {
                        let resp = ui.add(
                            egui::Slider::new(sigma, 0.1..=20.0)
                                .suffix(" px")
                                .text(t!("dlg.sigma_stddev").as_ref()),
                        );
                        apply |= resp.drag_stopped() || (resp.changed() && !resp.dragged());
                    }
                    Filter::Median { radius } => {
                        let resp = ui.add(
                            egui::Slider::new(radius, 1..=3)
                                .suffix(" px")
                                .text(t!("dlg.radius").as_ref()),
                        );
                        ui.label(t!("dlg.median_hint").as_ref());
                        apply |= resp.drag_stopped() || (resp.changed() && !resp.dragged());
                    }
                    Filter::UnsharpMask { sigma, amount } => {
                        let resp = ui.add(
                            egui::Slider::new(sigma, 0.1..=20.0)
                                .suffix(" px")
                                .text(t!("dlg.sigma_blur").as_ref()),
                        );
                        apply |= resp.drag_stopped() || (resp.changed() && !resp.dragged());
                        let resp = ui.add(
                            egui::Slider::new(amount, 0.0..=5.0).text(t!("dlg.strength").as_ref()),
                        );
                        apply |= resp.drag_stopped() || (resp.changed() && !resp.dragged());
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t!("dlg.ok").as_ref()).clicked() {
                        confirmed = true;
                    }
                    if ui.button(t!("dlg.cancel").as_ref()).clicked() {
                        cancelled = true;
                    }
                });
            });

        if apply {
            set_command(
                doc,
                index,
                Command::Filter {
                    filter: self.filter,
                },
            );
        }

        if cancelled || !window_open {
            self.revert(doc);
            self.open = false;
        } else if confirmed {
            // 未適用の変更（ドラッグ直後の決定など）を取りこぼさないようにする。
            set_command(
                doc,
                index,
                Command::Filter {
                    filter: self.filter,
                },
            );
            self.original = None;
            self.open = false;
        }
    }

    fn revert(&mut self, doc: &mut Document) {
        let Some(index) = self.index else {
            return;
        };
        if self.created {
            doc.remove_command(index);
        } else if let Some(original) = self.original.take() {
            set_command(doc, index, original);
        }
    }
}

// -------------------------------------------------------------- 設定

/// アプリ全体の設定ウィンドウ。コマンドには紐付かないので、
/// 変更は即座に反映され、閉じたあとも eframe の persistence で保存される。
#[derive(Default)]
pub struct SettingsDialog {
    pub open: bool,
}

impl SettingsDialog {
    pub fn show(&mut self, ctx: &Context, settings: &mut Settings) {
        if !self.open {
            return;
        }
        let mut window_open = true;
        egui::Window::new(t!("settings.title").as_ref())
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add(
                    egui::Slider::new(&mut settings.length_digits, 1..=5)
                        .text(t!("settings.length_digits").as_ref()),
                );
                ui.label(t!("settings.length_digits_hint").as_ref());
                ui.add_space(8.0);
                // 言語選択。選んだ瞬間から反映し、次回起動後も設定が引き継がれる。
                ui.label(t!("settings.language").as_ref());
                let before = settings.language;
                egui::ComboBox::from_id_salt("language")
                    .selected_text(settings.language.label())
                    .show_ui(ui, |ui| {
                        for lang in Language::ALL {
                            ui.selectable_value(&mut settings.language, lang, lang.label());
                        }
                    });
                if settings.language != before {
                    rust_i18n::set_locale(settings.language.code());
                }
            });
        self.open = window_open;
    }
}
