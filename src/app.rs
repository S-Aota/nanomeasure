//! アプリ本体。ツールバー / タブ / 画像表示 / コマンドリストの組み立て。

use std::path::PathBuf;

use egui::Ui;

use rust_i18n::t;

use crate::command::{
    Command, CommandCategory, CommandItem, Filter, FilterKind, HistoryFile, load_image,
};
use crate::dialogs::{
    ExportDialog, ExportResultDialog, FilterDialog, LevelsDialog, RotateDialog, ScaleDialog,
    SettingsDialog,
};
use crate::document::{Document, SourceCache, prune_source_cache};
use crate::measure_mode::MeasureMode;
use crate::metadata;
use crate::settings::{Settings, format_length};
use crate::view::ViewInfo;

const IMAGE_EXTENSIONS: &[&str] = &["tif", "tiff", "png", "jpg", "jpeg", "bmp"];

/// UI の描画中に集めて、描画後にまとめて処理する要求。
/// （描画中は `self` の一部を借りているため、その場では実行できない）
enum Action {
    PickImage,
    OpenPath(PathBuf),
    NewScale,
    NewRotate,
    NewLevels,
    NewFilter(Filter),
    NewMeasure,
    NewExport,
    NewExportResult,
    EditCommand(usize),
    ToggleCommand(usize),
    DeleteCommand(usize),
    MoveCommand(usize, isize),
    SelectCommand(usize, SelectMode),
    ClearSelection,
    CopyCommands,
    PasteCommands,
    Recompute,
    ReloadSources,
    SaveHistory,
    OpenHistory,
    ApplyHistory,
    ExportImage,
    OpenSettings,
    SwitchTab(usize),
    CloseTab(usize),
    Quit,
}

/// リストの行をクリックしたときの選択のしかた。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectMode {
    /// その行だけを選ぶ。
    Only,
    /// Ctrl クリック: 選択を足し引きする。
    Toggle,
    /// Shift クリック: 起点からの範囲を選ぶ。
    Range,
}

pub struct TemApp {
    docs: Vec<Document>,
    active: usize,
    /// デコード済みファイルの共有キャッシュ。再計算でディスクを叩き直さないため。
    cache: SourceCache,
    scale_dialog: ScaleDialog,
    rotate_dialog: RotateDialog,
    levels_dialog: LevelsDialog,
    filter_dialog: FilterDialog,
    export_dialog: ExportDialog,
    export_result_dialog: ExportResultDialog,
    measure_mode: MeasureMode,
    /// アプリ全体の設定（表示桁数など）。eframe の persistence で保存される。
    settings: Settings,
    settings_dialog: SettingsDialog,
    help_open: bool,
    about_open: bool,
    /// 編集中にタブ切り替えを試みたときの警告。編集中なら切り替えず、
    /// ポップアップを出して操作を無効化する。
    tab_switch_warning: Option<usize>,
    status: String,
    error: Option<String>,
    /// コマンドのコピー用。タブをまたいで貼り付けられるようにアプリ側で持つ。
    clipboard: Vec<CommandItem>,
    /// ステータスバーは画像表示より先に描くので、1 フレーム前の値を使う。
    last_hover: ViewInfo,
}

impl TemApp {
    pub fn new(cc: &eframe::CreationContext<'_>, startup_files: Vec<PathBuf>) -> Self {
        crate::fonts::install_japanese_font(&cc.egui_ctx);
        // 保存済みの言語設定を翻訳へ反映し、ウィンドウタイトルも言語に合わせる。
        let settings = Settings::load(cc.storage);
        rust_i18n::set_locale(settings.language.code());
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").into_owned()));
        let mut app = Self {
            docs: vec![Document::new(t!("tab.empty"))],
            active: 0,
            cache: SourceCache::new(),
            scale_dialog: ScaleDialog::default(),
            rotate_dialog: RotateDialog::default(),
            levels_dialog: LevelsDialog::default(),
            filter_dialog: FilterDialog::default(),
            export_dialog: ExportDialog::default(),
            export_result_dialog: ExportResultDialog::default(),
            measure_mode: MeasureMode::default(),
            settings,
            settings_dialog: SettingsDialog::default(),
            help_open: false,
            about_open: false,
            tab_switch_warning: None,
            status: t!("status.initial").into_owned(),
            error: None,
            clipboard: Vec::new(),
            last_hover: ViewInfo::default(),
        };
        for path in startup_files {
            app.open_path(path);
        }
        app
    }

    fn doc(&self) -> &Document {
        &self.docs[self.active]
    }

    fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.active]
    }

    fn has_image(&self) -> bool {
        self.doc().result().is_some()
    }

    // ------------------------------------------------------------- 描画

    fn ui_menu_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(t!("menu.file").as_ref(), |ui| {
                    if ui.button(t!("menu.insert_image").as_ref()).clicked() {
                        actions.push(Action::PickImage);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("menu.save_history").as_ref()).clicked() {
                        actions.push(Action::SaveHistory);
                        ui.close();
                    }
                    if ui.button(t!("menu.open_history").as_ref()).clicked() {
                        actions.push(Action::OpenHistory);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.has_image(),
                            egui::Button::new(t!("menu.apply_history").as_ref()),
                        )
                        .clicked()
                    {
                        actions.push(Action::ApplyHistory);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.has_image(),
                            egui::Button::new(t!("menu.export_image").as_ref()),
                        )
                        .clicked()
                    {
                        actions.push(Action::ExportImage);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("menu.settings").as_ref()).clicked() {
                        actions.push(Action::OpenSettings);
                        ui.close();
                    }
                    if ui.button(t!("menu.close_tab").as_ref()).clicked() {
                        actions.push(Action::CloseTab(self.active));
                        ui.close();
                    }
                    if ui.button(t!("menu.quit").as_ref()).clicked() {
                        actions.push(Action::Quit);
                        ui.close();
                    }
                });

                ui.menu_button(t!("menu.command").as_ref(), |ui| {
                    ui.menu_button(t!("menu.input").as_ref(), |ui| {
                        if ui.button(t!("menu.insert_image").as_ref()).clicked() {
                            actions.push(Action::PickImage);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.has_image(),
                                egui::Button::new(t!("menu.scale_settings").as_ref()),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewScale);
                            ui.close();
                        }
                    });
                    ui.menu_button(t!("menu.preprocess").as_ref(), |ui| {
                        if ui
                            .add_enabled(
                                self.has_image(),
                                egui::Button::new(t!("menu.rotate").as_ref()),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewRotate);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.has_image(),
                                egui::Button::new(t!("menu.levels").as_ref()),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewLevels);
                            ui.close();
                        }
                        ui.menu_button(t!("menu.filter").as_ref(), |ui| {
                            for (name, kind) in [
                                (
                                    t!("menu.filter_gaussian").as_ref(),
                                    FilterKind::GaussianBlur,
                                ),
                                (t!("menu.filter_median").as_ref(), FilterKind::Median),
                                (t!("menu.filter_unsharp").as_ref(), FilterKind::UnsharpMask),
                            ] {
                                if ui
                                    .add_enabled(self.has_image(), egui::Button::new(name))
                                    .clicked()
                                {
                                    actions.push(Action::NewFilter(Filter::default_of(kind)));
                                    ui.close();
                                }
                            }
                        });
                    });
                    ui.menu_button(t!("menu.analysis").as_ref(), |ui| {
                        if ui
                            .add_enabled(
                                self.has_image(),
                                egui::Button::new(t!("menu.measure").as_ref()),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewMeasure);
                            ui.close();
                        }
                    });
                    ui.menu_button(t!("menu.output").as_ref(), |ui| {
                        if ui
                            .add_enabled(
                                self.has_image(),
                                egui::Button::new(t!("menu.image_output").as_ref()),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewExport);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.has_image(),
                                egui::Button::new(t!("menu.result_output").as_ref()),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewExportResult);
                            ui.close();
                        }
                    });

                    ui.separator();
                    if ui
                        .add_enabled(
                            self.doc().has_selection(),
                            egui::Button::new(t!("menu.copy").as_ref()).shortcut_text("Ctrl+C"),
                        )
                        .clicked()
                    {
                        actions.push(Action::CopyCommands);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.clipboard.is_empty(),
                            egui::Button::new(t!("menu.paste").as_ref()).shortcut_text("Ctrl+V"),
                        )
                        .on_hover_text(t!("menu.paste_hover").as_ref())
                        .clicked()
                    {
                        actions.push(Action::PasteCommands);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.doc().has_selection(),
                            egui::Button::new(t!("menu.clear_selection").as_ref()),
                        )
                        .clicked()
                    {
                        actions.push(Action::ClearSelection);
                        ui.close();
                    }

                    ui.separator();
                    if ui
                        .add(egui::Button::new(t!("menu.recompute").as_ref()).shortcut_text("F5"))
                        .on_hover_text(t!("menu.recompute_hover").as_ref())
                        .clicked()
                    {
                        actions.push(Action::Recompute);
                        ui.close();
                    }
                    if ui
                        .button(t!("menu.reload_sources").as_ref())
                        .on_hover_text(t!("menu.reload_hover").as_ref())
                        .clicked()
                    {
                        actions.push(Action::ReloadSources);
                        ui.close();
                    }
                });

                ui.menu_button(t!("menu.help").as_ref(), |ui| {
                    if ui.button(t!("menu.how_to").as_ref()).clicked() {
                        self.help_open = true;
                        ui.close();
                    }
                    if ui.button(t!("menu.about").as_ref()).clicked() {
                        self.about_open = true;
                        ui.close();
                    }
                });
            });
        });
    }

    fn ui_tab_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("tab_bar").show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    for i in 0..self.docs.len() {
                        let selected = i == self.active;
                        let title = self.docs[i].title.clone();
                        if ui.selectable_label(selected, title).clicked() && !selected {
                            // コマンド編集中（ダイアログ・測長モード）は切り替えない。
                            // 警告を出して操作を無効化する。
                            if self.dialog_open() {
                                self.tab_switch_warning = Some(i);
                            } else {
                                actions.push(Action::SwitchTab(i));
                            }
                        }
                        if ui
                            .add(egui::Button::new("×").small().frame(false))
                            .on_hover_text(t!("tab.close_hover").as_ref())
                            .clicked()
                        {
                            actions.push(Action::CloseTab(i));
                        }
                        ui.separator();
                    }
                    if ui
                        .button("＋")
                        .on_hover_text(t!("tab.insert_hover").as_ref())
                        .clicked()
                    {
                        actions.push(Action::PickImage);
                    }
                });
            });
        });
    }

    fn ui_status_bar(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            // 右に出す文字列は先に作る（両方の閉包で self を借りられないため）。
            let error = self.docs[self.active]
                .error
                .clone()
                .or_else(|| self.error.clone());
            let message = match error {
                Some(err) => egui::RichText::new(err).color(egui::Color32::from_rgb(255, 120, 120)),
                None => egui::RichText::new(self.status.clone()).weak(),
            };

            // 左の情報を先に置き、右のメッセージは残った幅に詰める。
            egui::Sides::new().shrink_right().show(
                ui,
                |ui| self.ui_status_fields(ui),
                |ui| {
                    ui.add(egui::Label::new(message).truncate());
                },
            );
        });
    }

    fn ui_status_fields(&mut self, ui: &mut Ui) {
        let digits = self.settings.length_digits;
        let doc = &self.docs[self.active];
        if let Some(frame) = doc.result() {
            let img = &frame.image;
            match frame.scale {
                Some(scale) => {
                    let (extent_unit, ex, ey) = scale.extent(img.width, img.height);
                    ui.label(format!(
                        "{} × {} px  ({} × {} {})",
                        img.width,
                        img.height,
                        format_length(ex, digits),
                        format_length(ey, digits),
                        extent_unit.label()
                    ));
                    ui.separator();
                    ui.label(format!("{:.0} %", doc.view.zoom * 100.0));
                    ui.separator();
                    ui.label(scale.describe(digits))
                        .on_hover_text(t!("status.scale_change_hover").as_ref());
                    ui.separator();
                    // カーソル位置は画素と実寸法の両方を出す。
                    let per_px = scale.per_px();
                    match (self.last_hover.hover_px, self.last_hover.hover_value) {
                        (Some((x, y)), Some(v)) => ui.monospace(format!(
                            "({x}, {y}) px = ({}, {}) {}  I={v}",
                            format_length(x as f64 * per_px, digits),
                            format_length(y as f64 * per_px, digits),
                            scale.unit.label()
                        )),
                        _ => ui.monospace("(-, -)"),
                    };
                }
                None => {
                    // スケール未設定の画像は、実寸法を出さず画素のまま扱う。
                    ui.label(format!("{} × {} px", img.width, img.height));
                    ui.separator();
                    ui.label(format!("{:.0} %", doc.view.zoom * 100.0));
                    ui.separator();
                    ui.label(t!("status.no_scale").as_ref())
                        .on_hover_text(t!("status.no_scale_hover").as_ref());
                    ui.separator();
                    match (self.last_hover.hover_px, self.last_hover.hover_value) {
                        (Some((x, y)), Some(v)) => ui.monospace(format!("({x}, {y}) px  I={v}")),
                        _ => ui.monospace("(-, -)"),
                    };
                }
            }
            ui.separator();
        }
    }

    fn ui_command_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // 測長モード中は右パネルを測長ツール UI に置き換える。
        if self.measure_mode.open {
            self.measure_mode.show_panel(
                ui,
                &mut self.docs[self.active],
                self.settings.length_digits,
            );
            return;
        }
        let dirty = self.doc().is_dirty();
        let has_selection = self.doc().has_selection();
        let can_paste = !self.clipboard.is_empty();

        egui::Panel::right("command_panel")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // 未反映の変更があるときだけ、ボタンを目立つ色にする。
                    let button = if dirty {
                        egui::Button::new(
                            egui::RichText::new(t!("menu.recompute"))
                                .strong()
                                .color(egui::Color32::BLACK),
                        )
                        .fill(egui::Color32::from_rgb(235, 165, 60))
                    } else {
                        egui::Button::new(t!("menu.recompute").as_ref())
                    };
                    let hint = if dirty {
                        t!("panel.dirty_hint")
                    } else {
                        t!("panel.up_to_date")
                    };
                    if ui.add(button).on_hover_text(hint.as_ref()).clicked() {
                        actions.push(Action::Recompute);
                    }
                    ui.label(t!("panel.processes").as_ref());
                    if dirty {
                        ui.colored_label(
                            egui::Color32::from_rgb(235, 165, 60),
                            t!("panel.not_applied").as_ref(),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            has_selection,
                            egui::Button::new(t!("menu.copy").as_ref()).small(),
                        )
                        .on_hover_text(t!("panel.copy_hover").as_ref())
                        .clicked()
                    {
                        actions.push(Action::CopyCommands);
                    }
                    if ui
                        .add_enabled(
                            can_paste,
                            egui::Button::new(t!("menu.paste").as_ref()).small(),
                        )
                        .on_hover_text(t!("panel.paste_hover").as_ref())
                        .clicked()
                    {
                        actions.push(Action::PasteCommands);
                    }
                    if ui
                        .add_enabled(
                            has_selection,
                            egui::Button::new(t!("panel.clear_selection").as_ref()).small(),
                        )
                        .clicked()
                    {
                        actions.push(Action::ClearSelection);
                    }
                });
                ui.separator();

                let doc = &mut self.docs[self.active];
                if doc.commands.is_empty() {
                    ui.add_space(8.0);
                    ui.weak(t!("panel.no_commands").as_ref());
                    return;
                }

                let selection_fill = ui.visuals().selection.bg_fill;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut global = 0;
                    for cat in CommandCategory::ALL {
                        let count = doc.commands.list(cat).len();
                        ui.add_space(4.0);
                        // カテゴリの見出し。処理はこの並び（入力 → 前処理 →
                        // 解析 → 出力）で実行される。
                        let header = if count > 0 {
                            egui::RichText::new(cat.label()).strong()
                        } else {
                            egui::RichText::new(cat.label()).weak()
                        };
                        ui.label(header);

                        for local in 0..count {
                            let i = global + local;
                            let selected = doc.is_selected(i);
                            // 行の背景は中身の高さが決まってから塗るので、場所だけ先に確保する。
                            let backdrop = ui.painter().add(egui::Shape::Noop);

                            let row = ui.scope(|ui| {
                                // 右の操作ボタンを先に確保し、余った幅にラベルを詰める。
                                egui::Sides::new().shrink_left().show(
                                    ui,
                                    |ui| {
                                        let item = doc.commands.get_mut(i).expect("行は範囲内");
                                        let toggled = ui
                                            .checkbox(&mut item.enabled, "")
                                            .on_hover_text(t!("panel.disable_hover").as_ref())
                                            .changed();

                                        let label = item.command.label(self.settings.length_digits);
                                        let text = if item.enabled {
                                            egui::RichText::new(label)
                                        } else {
                                            egui::RichText::new(label).weak().strikethrough()
                                        };
                                        let resp = ui
                                            .add(
                                                egui::Label::new(text)
                                                    .truncate()
                                                    .sense(egui::Sense::click()),
                                            )
                                            .on_hover_text(t!("panel.row_hover").as_ref());
                                        (toggled, resp)
                                    },
                                    |ui| {
                                        let mut moved = None;
                                        if ui
                                            .small_button("×")
                                            .on_hover_text(t!("panel.delete").as_ref())
                                            .clicked()
                                        {
                                            moved = Some(Action::DeleteCommand(i));
                                        }
                                        if ui
                                            .small_button("▼")
                                            .on_hover_text(t!("panel.move_down").as_ref())
                                            .clicked()
                                        {
                                            moved = Some(Action::MoveCommand(i, 1));
                                        }
                                        if ui
                                            .small_button("▲")
                                            .on_hover_text(t!("panel.move_up").as_ref())
                                            .clicked()
                                        {
                                            moved = Some(Action::MoveCommand(i, -1));
                                        }
                                        moved
                                    },
                                )
                            });
                            let ((toggled, resp), moved) = row.inner;

                            if selected {
                                ui.painter().set(
                                    backdrop,
                                    egui::Shape::rect_filled(
                                        row.response.rect.expand2(egui::vec2(2.0, 1.0)),
                                        3.0,
                                        selection_fill,
                                    ),
                                );
                            }

                            if resp.double_clicked() {
                                actions.push(Action::EditCommand(i));
                            } else if resp.clicked() {
                                let mods = resp.ctx.input(|input| input.modifiers);
                                let mode = if mods.shift {
                                    SelectMode::Range
                                } else if mods.command || mods.ctrl {
                                    SelectMode::Toggle
                                } else {
                                    SelectMode::Only
                                };
                                actions.push(Action::SelectCommand(i, mode));
                            }
                            if toggled {
                                actions.push(Action::ToggleCommand(i));
                            }
                            actions.extend(moved);
                        }
                        global += count;
                    }

                    // リストの下の余白をクリックしたら選択を外す。
                    let rest = ui.available_size();
                    if rest.y > 0.0 && ui.allocate_response(rest, egui::Sense::click()).clicked() {
                        actions.push(Action::ClearSelection);
                    }
                });
            });
    }

    fn ui_help_windows(&mut self, ctx: &egui::Context) {
        let mut help_open = self.help_open;
        egui::Window::new(t!("help.title").as_ref())
            .open(&mut help_open)
            .resizable(true)
            .default_width(700.0)
            .show(ctx, |ui| {
                ui.label(t!("help.drag_drop").as_ref());
                ui.label(t!("help.zoom").as_ref());
                ui.label(t!("help.pan").as_ref());
                ui.label(t!("help.reopen_params").as_ref());
                ui.label(t!("help.disable_item").as_ref());
                ui.label(t!("help.select_rows").as_ref());
                ui.label(t!("help.copy_paste").as_ref());
                ui.label(t!("help.paste_position").as_ref());
                ui.label(t!("help.category_order").as_ref());
                ui.separator();
                ui.label(t!("help.recompute_note").as_ref());
                ui.label(t!("help.preview_note").as_ref());
                ui.label(t!("help.change_insert_file").as_ref());
                ui.label(t!("help.export_on_recompute").as_ref());
                ui.label(t!("help.reload_sources").as_ref());
                ui.separator();
                ui.label(t!("help.auto_scale").as_ref());
                ui.label(t!("help.manual_scale").as_ref());
                ui.separator();
                ui.label(t!("help.filters").as_ref());
                ui.separator();
                ui.label(t!("help.measure_intro").as_ref());
                ui.label(t!("help.measure_create").as_ref());
                ui.label(t!("help.measure_undo").as_ref());
                ui.label(t!("help.measure_snap").as_ref());
                ui.label(t!("help.measure_select_move").as_ref());
                ui.label(t!("help.measure_range_select").as_ref());
                ui.label(t!("help.measure_fitting").as_ref());
                ui.label(t!("help.measure_fit_edit").as_ref());
                ui.label(t!("help.measure_duplicate").as_ref());
                ui.label(t!("help.measure_duplicate_group").as_ref());
                ui.label(t!("help.measure_duplicate_ctrl").as_ref());
                ui.label(t!("help.measure_delete").as_ref());
                ui.label(t!("help.export_image_note").as_ref());
                ui.label(t!("help.export_result_note").as_ref());
                ui.label(t!("help.pan_zoom_again").as_ref());
            });
        self.help_open = help_open;

        let mut about_open = self.about_open;
        egui::Window::new(t!("about.title").as_ref())
            .open(&mut about_open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("tem_measure {}", env!("CARGO_PKG_VERSION")));
                ui.label(t!("about.description").as_ref());
                ui.label(t!("about.bit_depth").as_ref());
            });
        self.about_open = about_open;
    }

    // ------------------------------------------------------- アクション処理

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::PickImage => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(t!("dialogs.filter_image").as_ref(), IMAGE_EXTENSIONS)
                    .set_title(t!("dialogs.title_insert").as_ref())
                    .pick_file()
                {
                    self.insert_image(path);
                }
            }
            Action::OpenPath(path) => self.open_path(path),
            Action::NewScale => {
                let index = self.active;
                self.scale_dialog.open_new(&mut self.docs[index]);
            }
            Action::NewRotate => {
                let index = self.active;
                self.rotate_dialog.open_new(&mut self.docs[index]);
            }
            Action::NewLevels => {
                let index = self.active;
                self.levels_dialog.open_new(&mut self.docs[index]);
            }
            Action::NewFilter(filter) => {
                let index = self.active;
                self.filter_dialog.open_new(&mut self.docs[index], filter);
            }
            Action::NewMeasure => {
                let index = self.active;
                self.close_dialogs();
                self.measure_mode.open_new(&mut self.docs[index], index);
            }
            Action::NewExport => {
                let index = self.active;
                self.close_dialogs();
                self.export_dialog.open_new(&mut self.docs[index]);
            }
            Action::NewExportResult => {
                let index = self.active;
                self.close_dialogs();
                self.export_result_dialog.open_new(&mut self.docs[index]);
            }
            Action::OpenSettings => self.settings_dialog.open = true,
            Action::EditCommand(i) => self.edit_command(i),
            Action::ToggleCommand(i) => self.doc_mut().invalidate_from(i),
            Action::DeleteCommand(i) => {
                self.close_dialogs();
                self.doc_mut().remove_command(i);
            }
            Action::MoveCommand(i, delta) => {
                self.close_dialogs();
                self.doc_mut().move_command(i, delta);
            }
            Action::SelectCommand(i, mode) => {
                let doc = self.doc_mut();
                match mode {
                    SelectMode::Only => doc.select_only(i),
                    SelectMode::Toggle => doc.toggle_selection(i),
                    SelectMode::Range => doc.select_range_to(i),
                }
            }
            Action::ClearSelection => self.doc_mut().clear_selection(),
            Action::CopyCommands => self.copy_commands(),
            Action::PasteCommands => self.paste_commands(),
            Action::Recompute => {
                let active = self.active;
                let dirty = self.docs[active].is_dirty();
                self.docs[active].recompute(&mut self.cache);
                self.status = if dirty {
                    t!("status.executed").into_owned()
                } else {
                    t!("status.no_changes").into_owned()
                };
                // ファイル出力は副作用なので、このボタン（F5）を押したときだけ行う。
                self.run_exports();
                ctx.request_repaint();
            }
            Action::ReloadSources => {
                // 元ファイルを読み直したいときだけキャッシュを捨てる。
                self.cache.clear();
                let active = self.active;
                self.docs[active].invalidate_all();
                self.docs[active].recompute(&mut self.cache);
                self.status = t!("status.reloaded").into_owned();
                ctx.request_repaint();
            }
            Action::SaveHistory => self.save_history(),
            Action::OpenHistory => self.open_history(),
            Action::ApplyHistory => self.apply_history(),
            Action::ExportImage => self.export_image(),
            Action::SwitchTab(i) => {
                // ダイアログ・測長モードは開いたタブのコマンド添字を持つので、
                // 切り替える前に（元のタブで）破棄して閉じる。
                if self.dialog_open() {
                    self.close_dialogs();
                    self.status = t!("status.tab_switched_discard").into_owned();
                }
                self.active = i.min(self.docs.len() - 1);
            }
            Action::CloseTab(i) => self.close_tab(i),
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// 開いているダイアログ・測長モードを破棄して閉じる。プレビュー用に
    /// 書き換えていた値は元に戻し、その状態で計算し直しておく（閉じた後は
    /// 自動計算されないため、戻した値が表示に反映されないまま残らないように）。
    fn close_dialogs(&mut self) {
        let was_open = self.dialog_open();
        self.scale_dialog.open = false;
        self.rotate_dialog.open = false;
        self.levels_dialog.open = false;
        self.filter_dialog.open = false;
        self.export_dialog.open = false;
        self.export_result_dialog.open = false;
        // 測長モードも編集中のダイアログと同様に閉じる（コマンド構造の
        // 変更操作と競合しないように、破棄して終了する）。
        if self.measure_mode.open {
            let tab = self.measure_mode.tab;
            if tab < self.docs.len() {
                self.measure_mode.revert(&mut self.docs[tab]);
                // タブ切り替えで閉じる場合は、編集していたタブの方を戻す。
                self.docs[tab].recompute(&mut self.cache);
            } else {
                self.measure_mode.open = false;
            }
        }
        if was_open {
            self.docs[self.active].recompute(&mut self.cache);
        }
    }

    /// 測長モードで編集中のコマンドの添字（`tab` のタブで編集中のときだけ）。
    /// そのコマンドは編集セッション側が描くので、確定済みの描画・出力から除く。
    fn measure_editing(&self, tab: usize) -> Option<usize> {
        (self.measure_mode.open && self.measure_mode.tab == tab).then_some(self.measure_mode.index)
    }

    /// パラメータ調整中はプレビューのため、結果を先に進めておく必要がある。
    fn dialog_open(&self) -> bool {
        self.scale_dialog.open
            || self.rotate_dialog.open
            || self.levels_dialog.open
            || self.filter_dialog.open
            || self.export_dialog.open
            || self.export_result_dialog.open
            || self.measure_mode.open
    }

    fn copy_commands(&mut self) {
        let doc = self.doc();
        let picked: Vec<CommandItem> = doc
            .selected_indices()
            .into_iter()
            .filter_map(|i| doc.commands.get(i).cloned())
            .collect();
        if picked.is_empty() {
            return;
        }
        self.status = t!("status.copied_count", count = picked.len()).into_owned();
        self.clipboard = picked;
    }

    fn paste_commands(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        // 編集中のダイアログは対象の添字を持っているので、行が動く前に閉じる。
        self.close_dialogs();
        let items = self.clipboard.clone();
        let count = items.len();
        let doc = self.doc_mut();
        // 貼り付けは各カテゴリのリスト末尾への追加（処理順はカテゴリで固定のため）。
        let indices = doc.extend_commands(items);
        // 貼り付けた行を選択し直しておくと、続けて貼っても位置が分かりやすい。
        doc.select_indices(indices);
        self.status = t!("status.pasted_count", count = count).into_owned();
    }

    fn edit_command(&mut self, index: usize) {
        self.close_dialogs();
        let doc = &mut self.docs[self.active];
        let Some(command) = doc.commands.get(index).map(|c| c.command.clone()) else {
            return;
        };
        match command {
            Command::SetScale { .. } => self.scale_dialog.open_edit(doc, index),
            Command::Rotate { .. } => self.rotate_dialog.open_edit(doc, index),
            Command::Levels { .. } => self.levels_dialog.open_edit(doc, index),
            Command::Filter { .. } => self.filter_dialog.open_edit(doc, index),
            Command::Measure { .. } => {
                let tab = self.active;
                self.measure_mode.open_edit(doc, tab, index)
            }
            Command::ExportImage { .. } => self.export_dialog.open_edit(doc, index),
            Command::ExportResult { .. } => self.export_result_dialog.open_edit(doc, index),
            Command::InsertImage { path } => {
                // 画像挿入の「パラメータ」は読み込むファイルそのもの。
                let mut dialog = rfd::FileDialog::new()
                    .add_filter(t!("dialogs.filter_image").as_ref(), IMAGE_EXTENSIONS)
                    .set_title(t!("dialogs.title_change_file").as_ref());
                if let Some(parent) = path.parent() {
                    dialog = dialog.set_directory(parent);
                }
                if let Some(new_path) = dialog.pick_file() {
                    doc.title = file_label(&new_path);
                    if let Some(item) = doc.commands.get_mut(index) {
                        item.command = Command::InsertImage { path: new_path };
                    }
                    doc.invalidate_from(index);
                    doc.view.request_fit();
                    // ファイルの差し替えはすぐに表示へ反映する（出力は F5 のときだけ）。
                    doc.recompute(&mut self.cache);
                    prune_source_cache(&mut self.cache);
                }
            }
        }
    }

    /// 起動引数やドロップで渡されたパスを、拡張子で振り分けて開く。
    fn open_path(&mut self, path: PathBuf) {
        let is_history = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if is_history {
            self.open_history_path(path);
        } else {
            self.insert_image(path);
        }
    }

    fn insert_image(&mut self, path: PathBuf) {
        // 画像を確実に開けるか先に確かめてから、タブとコマンドを作る。
        if let Err(e) = load_image(&path, &mut self.cache) {
            self.error = Some(e);
            return;
        }
        self.error = None;
        self.close_dialogs();

        // TIFF タグから画素の実寸法が読めたら、スケール設定コマンドとして残す。
        // 読めなければ何も足さず、スケール未設定 (None) のまま扱う。
        let auto_scale = metadata::read_tiff_scale(&path);

        if !self.docs[self.active].is_empty() {
            self.docs.push(Document::new(""));
            self.active = self.docs.len() - 1;
        }
        let doc = &mut self.docs[self.active];
        doc.title = file_label(&path);
        doc.push_command(Command::InsertImage { path });
        if let Some(scale) = auto_scale {
            doc.push_command(Command::scale_from(scale));
        }
        doc.view.request_fit();

        self.status = match auto_scale {
            Some(scale) => t!(
                "status.inserted_with_scale",
                scale = scale.describe(self.settings.length_digits)
            )
            .into_owned(),
            None => t!("status.inserted_no_scale").into_owned(),
        };
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.docs.len() {
            return;
        }
        self.close_dialogs();
        self.docs.remove(index);
        prune_source_cache(&mut self.cache);
        if self.docs.is_empty() {
            self.docs.push(Document::new(t!("tab.empty")));
        }
        self.active = self.active.min(self.docs.len() - 1);
    }

    /// アクティブタブの出力コマンド（画像出力・結果出力）を処理順に実行する。
    /// 出力先は各コマンドのテンプレート（`{dir}` / `{filename}` は画像パスから解決）。
    fn run_exports(&mut self) {
        let doc = &self.docs[self.active];
        let skip = self.measure_editing(self.active);
        let digits = self.settings.length_digits;
        let mut saved = Vec::new();
        for (i, item) in doc.commands.iter().enumerate() {
            let result = match &item.command {
                Command::ExportImage { .. } => {
                    crate::export::save_image_export(doc, i, skip, digits)
                }
                Command::ExportResult { .. } => crate::export::save_result_json(doc, i),
                _ => continue,
            };
            match result {
                Ok(Some(path)) => saved.push(path),
                Ok(None) => {}
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            }
        }
        if saved.is_empty() {
            return;
        }
        self.error = None;
        let names: Vec<String> = saved.iter().map(|p| file_label(p)).collect();
        self.status = t!("status.saved_files", names = names.join(", ")).into_owned();
    }

    fn save_history(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(t!("dialogs.filter_history").as_ref(), &["json"])
            .set_file_name("history.json")
            .set_title(t!("dialogs.title_save_history").as_ref())
            .save_file()
        else {
            return;
        };
        let file = HistoryFile::new(self.doc().commands.iter().cloned().collect());
        match file.save(&path) {
            Ok(()) => {
                self.error = None;
                self.status = t!("status.saved_to", path = file_label(&path)).into_owned();
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn open_history(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(t!("dialogs.filter_history").as_ref(), &["json"])
            .set_title(t!("dialogs.title_open_history").as_ref())
            .pick_file()
        else {
            return;
        };
        self.open_history_path(path);
    }

    fn open_history_path(&mut self, path: PathBuf) {
        let file = match HistoryFile::load(&path) {
            Ok(f) => f,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.close_dialogs();

        // 画像挿入コマンドを含むので、そのままタブを復元できる。
        let title = file
            .commands
            .iter()
            .find_map(|c| match &c.command {
                Command::InsertImage { path } => Some(file_label(path)),
                _ => None,
            })
            .unwrap_or_else(|| file_label(&path));

        let mut doc = Document::new(title);
        doc.replace_commands(file.commands);
        doc.view.request_fit();

        if self.docs[self.active].is_empty() {
            self.docs[self.active] = doc;
        } else {
            self.docs.push(doc);
            self.active = self.docs.len() - 1;
        }
        self.error = None;
        self.status = t!("status.history_opened").into_owned();
    }

    fn apply_history(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(t!("dialogs.filter_history").as_ref(), &["json"])
            .set_title(t!("dialogs.title_apply_history").as_ref())
            .pick_file()
        else {
            return;
        };
        let file = match HistoryFile::load(&path) {
            Ok(f) => f,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.close_dialogs();
        // 画像挿入は捨てて、処理だけを今のタブの末尾に足す。
        let items = file.processing_only();
        let count = items.len();
        self.doc_mut().extend_commands(items);
        self.error = None;
        self.status = t!("status.added_count", count = count).into_owned();
    }

    fn export_image(&mut self) {
        let Some(img) = self.doc().image().cloned() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG", &["png"])
            .add_filter("TIFF", &["tif", "tiff"])
            .set_file_name("export.png")
            .set_title(t!("dialogs.title_export_image").as_ref())
            .save_file()
        else {
            return;
        };
        // 内部と同じ 16bit グレースケールのまま書き出す。
        match img.to_luma16_buffer().save(&path) {
            Ok(()) => {
                self.error = None;
                self.status = t!("status.exported_to", path = file_label(&path)).into_owned();
            }
            Err(e) => {
                self.error = Some(t!("status.export_failed", error = format!("{e}")).into_owned())
            }
        }
    }

    /// コピー / 貼り付け / 再計算のキーボード操作。
    /// ダイアログの入力欄と取り合わないよう、ダイアログ表示中は拾わない。
    fn handle_shortcuts(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.dialog_open() {
            return;
        }
        const COPY: egui::KeyboardShortcut =
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::C);
        const PASTE: egui::KeyboardShortcut =
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::V);
        const RECOMPUTE: egui::KeyboardShortcut =
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::F5);

        if ctx.input_mut(|i| i.consume_shortcut(&COPY)) {
            actions.push(Action::CopyCommands);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&PASTE)) {
            actions.push(Action::PasteCommands);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&RECOMPUTE)) {
            actions.push(Action::Recompute);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            actions.push(Action::OpenPath(path));
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                t!("status.drop_here").as_ref(),
                egui::FontId::proportional(24.0),
                egui::Color32::WHITE,
            );
        }
    }
}

impl eframe::App for TemApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, crate::settings::SETTINGS_KEY, &self.settings);
    }

    /// persistence 有効化で egui 内部状態（スクロール位置など）まで
    /// 保存されないようにする。保存するのはアプリ設定だけ。
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        self.handle_dropped_files(&ctx, &mut actions);

        // 測長モード中にタブが切り替わったら、編集を破棄して終了する。
        if self.measure_mode.open && self.measure_mode.tab != self.active {
            self.close_dialogs();
            self.status = t!("status.measure_mode_ended").into_owned();
        }

        // 自動で計算するのは次の 2 つの場合だけ。それ以外の変更（並べ替え・
        // 有効無効の切り替え・削除・貼り付け）は「再計算」を押すまで待つ。
        //   - まだ一度も結果が出ていないタブ（開いた直後で何も表示できない）
        //   - パラメータ調整ダイアログを開いている間（プレビューのため）
        let preview = self.dialog_open();
        let doc = &mut self.docs[self.active];
        if doc.is_dirty() && (preview || doc.result().is_none()) {
            doc.recompute(&mut self.cache);
        }

        self.handle_shortcuts(&ctx, &mut actions);
        // 測長モード中は Ctrl+Z / Shift+Ctrl+Z をツール操作の取り消しに使う。
        if self.measure_mode.open && self.measure_mode.tab < self.docs.len() {
            self.measure_mode
                .handle_shortcuts(&ctx, &mut self.docs[self.measure_mode.tab]);
        }
        self.ui_menu_bar(ui, &mut actions);
        self.ui_tab_bar(ui, &mut actions);
        self.ui_status_bar(ui);
        self.ui_command_panel(ui, &mut actions);

        // 表示は読み込んだ画像の輝度のまま（自動コントラストはしない）。
        let index = self.active;
        let range = self.docs[index].display_range();
        let generation = self.docs[index].generation;
        // 測長モード中は画像上の入力（左ドラッグの移動、右ドラッグのパン、
        // ズーム）をすべて measure_mode 側で処理する。
        let interactive = !self.measure_mode.open;
        let skip = self.measure_editing(index);
        self.last_hover = egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                let doc = &mut self.docs[index];
                let image = doc.image().cloned();
                let mut info = doc
                    .view
                    .show(ui, image.as_ref(), generation, range, interactive);

                // 測長オーバーレイ: 後段の回転等で画像が変わっていない
                // 測長コマンドだけを、そのコマンドの画像座標系で描く。
                // 測長モードで編集中のコマンドは draw_session 側が描くので
                // ここではスキップする（二重描画を防ぐ）。
                // 計算結果は再計算時の段に保持されたものを使う（毎フレーム
                // フィッティングし直さない）。
                if let Some(img) = &image {
                    let painter = ui.painter_at(info.vp);
                    for m in doc.measure_overlays(img, skip) {
                        crate::measure_mode::draw_computed(
                            &painter,
                            &info,
                            &m.computed,
                            m.scale,
                            self.settings.length_digits,
                        );
                    }
                }

                // 測長モード中は編集セッションの描画とツール入力処理を重ねる。
                if self.measure_mode.open {
                    let painter = ui.painter_at(info.vp);
                    self.measure_mode.draw_session(
                        &painter,
                        &info,
                        doc,
                        self.settings.length_digits,
                    );
                    let hover = self.measure_mode.handle_overlay_input(ui, &info, doc);
                    info.hover_px = hover.hover_px;
                    info.hover_value = hover.hover_value;
                }
                info
            })
            .inner;

        self.ui_help_windows(&ctx);

        let doc = &mut self.docs[index];
        let digits = self.settings.length_digits;
        self.scale_dialog.show(&ctx, doc, digits);
        self.rotate_dialog.show(&ctx, doc);
        self.levels_dialog.show(&ctx, doc);
        self.filter_dialog.show(&ctx, doc);
        self.export_dialog.show(&ctx, doc);
        self.export_result_dialog.show(&ctx, doc);
        self.settings_dialog.show(&ctx, &mut self.settings);
        if self.measure_mode.open {
            let tab = self.measure_mode.tab;
            if tab < self.docs.len() {
                self.measure_mode
                    .show_confirm_modal(&ctx, &mut self.docs[tab]);
                self.measure_mode.show_fit_popup(&ctx, &mut self.docs[tab]);
            }
        }
        // 編集中にタブ切り替えを試みたときの警告。編集が終わっていれば
        // 警告は消す（ポップアップを出しっぱなしにしない）。
        if self.tab_switch_warning.is_some() && !self.dialog_open() {
            self.tab_switch_warning = None;
        }
        if self.tab_switch_warning.is_some() {
            let mut close = false;
            egui::Window::new(t!("tabswitch.title").as_ref())
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(t!("tabswitch.body1").as_ref());
                    ui.label(t!("tabswitch.body2").as_ref());
                    ui.add_space(8.0);
                    if ui.button(t!("tabswitch.close").as_ref()).clicked() {
                        close = true;
                    }
                });
            if close {
                self.tab_switch_warning = None;
            }
        }
        // このフレームでダイアログ・測長モードが閉じたら（決定・キャンセル
        // どちらも）、最後の値で計算し直しておく。キャンセルで元に戻した値が
        // F5 を押すまで表示に反映されない、ということを防ぐ。
        if preview && !self.dialog_open() && self.docs[index].is_dirty() {
            self.docs[index].recompute(&mut self.cache);
        }

        for action in actions {
            self.handle(&ctx, action);
        }
    }
}

fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod i18n_tests {
    use rust_i18n::t;

    /// キーが欠けているとキー名そのものが表示されるので、全ロケールで
    /// 主要なキーが翻訳されていることを確かめる。
    #[test]
    fn keys_resolve_in_all_locales() {
        for locale in ["en", "ja", "kr", "cn", "tw"] {
            rust_i18n::set_locale(locale);
            for key in [
                "menu.file",
                "panel.row_hover",
                "help.measure_duplicate",
                "cmd.category.input",
                "cmd.measure",
                "dlg.ok",
                "dlg.levels_histogram",
                "mm.tool.distance",
                "mm.tool.angle",
                "mm.hint_angle",
                "mm.shift_angle_hint",
                "mm.fit_off",
                "mm.csv_stats_header",
                "exp.no_image",
                "view.drop_hint",
                "fonts.not_found",
            ] {
                assert_ne!(t!(key).as_ref(), key, "locale {locale}: missing {key}");
            }
            let copied = t!("status.copied_count", count = 3).into_owned();
            assert!(!copied.contains("status.copied_count"));
        }
        // 他のテストに影響しないよう、既定の言語へ戻す。
        rust_i18n::set_locale(crate::settings::Language::default().code());
    }
}
