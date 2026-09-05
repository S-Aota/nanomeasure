//! アプリ本体。ツールバー / タブ / 画像表示 / コマンドリストの組み立て。

use std::path::PathBuf;

use egui::Ui;

use crate::command::{Command, CommandItem, HistoryFile, load_image};
use crate::dialogs::{LevelsDialog, RotateDialog, ScaleDialog};
use crate::document::{Document, SourceCache};
use crate::frame::format_length;
use crate::measure_mode::MeasureMode;
use crate::metadata;
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
    NewMeasure,
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
    measure_mode: MeasureMode,
    /// データは変えずに、表示だけ min/max へ引き伸ばす。
    auto_contrast: bool,
    help_open: bool,
    about_open: bool,
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
        let mut app = Self {
            docs: vec![Document::new("(空)")],
            active: 0,
            cache: SourceCache::new(),
            scale_dialog: ScaleDialog::default(),
            rotate_dialog: RotateDialog::default(),
            levels_dialog: LevelsDialog::default(),
            measure_mode: MeasureMode::default(),
            auto_contrast: true,
            help_open: false,
            about_open: false,
            status: "画像をドラッグ&ドロップするか、ファイル → 画像を挿入 で開いてください。"
                .to_owned(),
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
                ui.menu_button("ファイル", |ui| {
                    if ui.button("画像を挿入...").clicked() {
                        actions.push(Action::PickImage);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("コマンド履歴を保存...").clicked() {
                        actions.push(Action::SaveHistory);
                        ui.close();
                    }
                    if ui.button("コマンド履歴を開く...").clicked() {
                        actions.push(Action::OpenHistory);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.has_image(),
                            egui::Button::new("コマンド履歴を現在の画像に適用..."),
                        )
                        .clicked()
                    {
                        actions.push(Action::ApplyHistory);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(self.has_image(), egui::Button::new("画像を書き出し..."))
                        .clicked()
                    {
                        actions.push(Action::ExportImage);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("タブを閉じる").clicked() {
                        actions.push(Action::CloseTab(self.active));
                        ui.close();
                    }
                    if ui.button("終了").clicked() {
                        actions.push(Action::Quit);
                        ui.close();
                    }
                });

                ui.menu_button("コマンド", |ui| {
                    if ui.button("画像を挿入...").clicked() {
                        actions.push(Action::PickImage);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(self.has_image(), egui::Button::new("スケール設定..."))
                        .clicked()
                    {
                        actions.push(Action::NewScale);
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.has_image(), egui::Button::new("画像の回転..."))
                        .clicked()
                    {
                        actions.push(Action::NewRotate);
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.has_image(), egui::Button::new("レベル補正..."))
                        .clicked()
                    {
                        actions.push(Action::NewLevels);
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.has_image(), egui::Button::new("測長..."))
                        .clicked()
                    {
                        actions.push(Action::NewMeasure);
                        ui.close();
                    }
                    ui.menu_button("フィルタ", |ui| {
                        for name in [
                            "ガウシアンぼかし...",
                            "メディアン...",
                            "アンシャープマスク...",
                        ] {
                            ui.add_enabled(false, egui::Button::new(name))
                                .on_disabled_hover_text("未実装");
                        }
                    });

                    ui.separator();
                    if ui
                        .add_enabled(
                            self.doc().has_selection(),
                            egui::Button::new("コピー").shortcut_text("Ctrl+C"),
                        )
                        .clicked()
                    {
                        actions.push(Action::CopyCommands);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.clipboard.is_empty(),
                            egui::Button::new("貼り付け").shortcut_text("Ctrl+V"),
                        )
                        .on_hover_text("選択行の手前に挿入します。未選択なら末尾へ追加します")
                        .clicked()
                    {
                        actions.push(Action::PasteCommands);
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.doc().has_selection(), egui::Button::new("選択を解除"))
                        .clicked()
                    {
                        actions.push(Action::ClearSelection);
                        ui.close();
                    }

                    ui.separator();
                    if ui
                        .add(egui::Button::new("再計算").shortcut_text("F5"))
                        .on_hover_text("未反映の変更を実行して表示を更新します")
                        .clicked()
                    {
                        actions.push(Action::Recompute);
                        ui.close();
                    }
                    if ui
                        .button("元ファイルを読み直して再計算")
                        .on_hover_text("ディスク上の画像が更新されたときに使います")
                        .clicked()
                    {
                        actions.push(Action::ReloadSources);
                        ui.close();
                    }
                });

                ui.menu_button("ヘルプ", |ui| {
                    if ui.button("操作方法").clicked() {
                        self.help_open = true;
                        ui.close();
                    }
                    if ui.button("バージョン情報").clicked() {
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
                        if ui.selectable_label(selected, title).clicked() {
                            self.active = i;
                        }
                        if ui
                            .add(egui::Button::new("×").small().frame(false))
                            .on_hover_text("タブを閉じる")
                            .clicked()
                        {
                            actions.push(Action::CloseTab(i));
                        }
                        ui.separator();
                    }
                    if ui.button("＋").on_hover_text("画像を挿入").clicked() {
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
                        format_length(ex),
                        format_length(ey),
                        extent_unit.label()
                    ));
                    ui.separator();
                    ui.label(format!("{:.0} %", doc.view.zoom * 100.0));
                    ui.separator();
                    ui.label(scale.describe())
                        .on_hover_text("スケール設定コマンドで変更できます");
                    ui.separator();
                    // カーソル位置は画素と実寸法の両方を出す。
                    let per_px = scale.per_px();
                    match (self.last_hover.hover_px, self.last_hover.hover_value) {
                        (Some((x, y)), Some(v)) => ui.monospace(format!(
                            "({x}, {y}) px = ({}, {}) {}  I={v}",
                            format_length(x as f64 * per_px),
                            format_length(y as f64 * per_px),
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
                    ui.label("スケール未設定").on_hover_text(
                        "メタデータから画素サイズを読み取れませんでした。スケール設定コマンドで設定すると実寸法を出せます。",
                    );
                    ui.separator();
                    match (self.last_hover.hover_px, self.last_hover.hover_value) {
                        (Some((x, y)), Some(v)) => ui.monospace(format!("({x}, {y}) px  I={v}")),
                        _ => ui.monospace("(-, -)"),
                    };
                }
            }
            ui.separator();
        }
        ui.checkbox(&mut self.auto_contrast, "自動コントラスト")
            .on_hover_text(
                "表示だけを min/max に合わせて引き伸ばします（画像データは変わりません）",
            );
    }

    fn ui_command_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // 測長モード中は右パネルを測長ツール UI に置き換える。
        if self.measure_mode.open {
            self.measure_mode
                .show_panel(ui, &mut self.docs[self.active]);
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
                            egui::RichText::new("再計算")
                                .strong()
                                .color(egui::Color32::BLACK),
                        )
                        .fill(egui::Color32::from_rgb(235, 165, 60))
                    } else {
                        egui::Button::new("再計算")
                    };
                    let hint = if dirty {
                        "未反映の変更があります。押すとコマンドを実行して表示を更新します"
                    } else {
                        "表示は最新です"
                    };
                    if ui.add(button).on_hover_text(hint).clicked() {
                        actions.push(Action::Recompute);
                    }
                    ui.label("処理（コマンド）");
                    if dirty {
                        ui.colored_label(egui::Color32::from_rgb(235, 165, 60), "未反映");
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_selection, egui::Button::new("コピー").small())
                        .on_hover_text("選択した処理をコピーします (Ctrl+C)")
                        .clicked()
                    {
                        actions.push(Action::CopyCommands);
                    }
                    if ui
                        .add_enabled(can_paste, egui::Button::new("貼り付け").small())
                        .on_hover_text(
                            "選択行の手前に挿入します。未選択なら末尾へ追加します (Ctrl+V)",
                        )
                        .clicked()
                    {
                        actions.push(Action::PasteCommands);
                    }
                    if ui
                        .add_enabled(has_selection, egui::Button::new("選択解除").small())
                        .clicked()
                    {
                        actions.push(Action::ClearSelection);
                    }
                });
                ui.separator();

                let doc = &mut self.docs[self.active];
                if doc.commands.is_empty() {
                    ui.add_space(8.0);
                    ui.weak("まだ処理はありません。");
                    return;
                }

                let selection_fill = ui.visuals().selection.bg_fill;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for i in 0..doc.commands.len() {
                        let selected = doc.is_selected(i);
                        // 行の背景は中身の高さが決まってから塗るので、場所だけ先に確保する。
                        let backdrop = ui.painter().add(egui::Shape::Noop);

                        let row = ui.scope(|ui| {
                            // 右の操作ボタンを先に確保し、余った幅にラベルを詰める。
                            egui::Sides::new().shrink_left().show(
                                ui,
                                |ui| {
                                    let toggled = ui
                                        .checkbox(&mut doc.commands[i].enabled, "")
                                        .on_hover_text("外すとこの処理を一時的に無効化します")
                                        .changed();

                                    let label = doc.commands[i].command.label();
                                    let text = if doc.commands[i].enabled {
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
                                        .on_hover_text(
                                            "クリックで選択（Ctrl / Shift で複数選択）\n\
                                             ダブルクリックでパラメータを再編集",
                                        );
                                    (toggled, resp)
                                },
                                |ui| {
                                    let mut moved = None;
                                    if ui.small_button("×").on_hover_text("削除").clicked() {
                                        moved = Some(Action::DeleteCommand(i));
                                    }
                                    if ui.small_button("▼").on_hover_text("下へ").clicked() {
                                        moved = Some(Action::MoveCommand(i, 1));
                                    }
                                    if ui.small_button("▲").on_hover_text("上へ").clicked() {
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
        egui::Window::new("操作方法")
            .open(&mut help_open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("・画像表示領域にファイルをドラッグ&ドロップすると画像を挿入します。");
                ui.label("・マウスホイールで拡大縮小（カーソル位置を中心）。");
                ui.label("・左ドラッグ（または中ドラッグ）でパン。");
                ui.label(
                    "・右のリストの行をダブルクリックすると、その処理のパラメータを再編集できます。",
                );
                ui.label("・チェックを外すと、その処理だけを一時的に無効化できます。");
                ui.label("・行をクリックすると選択されます。Ctrl クリックで追加・解除、Shift クリックで範囲選択。");
                ui.label("・選択した処理は Ctrl+C でコピー、Ctrl+V で貼り付けできます。別のタブへも貼り付けられます。");
                ui.label("・貼り付けは選択行の手前に挿入され、未選択なら末尾に追加されます。");
                ui.separator();
                ui.label("・並べ替え・有効無効の切り替え・削除・貼り付けは、すぐには計算されません。「再計算」(F5) を押すまで表示は変わらず、その間ボタンが橙色になります。");
                ui.label("・パラメータ調整ダイアログを開いている間だけは、結果をその場で見られるように自動で計算します。");
                ui.label("・ディスク上の画像が更新されたときは、コマンド → 元ファイルを読み直して再計算 を使ってください。");
                ui.separator();
                ui.label("・TIFF の FEI / Thermo Fisher タグ、または ImageJ の単位情報から画素の実寸法が読めた場合、画像挿入の直後に「スケール設定」コマンドが自動で追加されます。");
                ui.label("・読めなかった場合はスケール未設定となり、実寸法は出せません（画素単位のまま）。コマンド → スケール設定 で、スケールバーから読み取った値を手で入れられます。");
                ui.separator();
                ui.label("・測長: コマンド → 測長... で右パネルが測長ツールに切り替わります。");
                ui.label("・二点間測長・境界線は、画像上を 2 回クリックして作成します（Esc または右クリックで作成途中をキャンセル）。");
                ui.label("・測長モード中は Ctrl+Z / Ctrl+Shift+Z でツール操作の取り消し・やり直しができます。");
                ui.label("・スナップ on のとき、二点間測長の端点は既存の境界線・オフセット線に吸い付きます。");
                ui.label("・Esc でツール選択を解除すると、左ドラッグでパンが使えます。非選択状態では測長・境界線をクリックして選択し、ドラッグで移動できます（端点付近のクリックは端点だけ、線の上は全体が動きます）。");
                ui.label("・フィッティング領域をダブルクリックすると、その測長だけのフィッティング設定を再編集できます。");
                ui.label("・直線複製: 測長・境界線をクリックで選択し、マウス移動で方向と距離を指定して、もう一度クリックで確定。ホイールで複製数 (1-20) を調整し、距離を等分した位置に複製します。");
                ui.label("・中ドラッグまたは右ドラッグでパン、ホイールでズームできます（測長モード中も同じ）。");
            });
        self.help_open = help_open;

        let mut about_open = self.about_open;
        egui::Window::new("バージョン情報")
            .open(&mut about_open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("tem_measure {}", env!("CARGO_PKG_VERSION")));
                ui.label("TEM 画像の解析用ツール");
                ui.label("内部処理は 16bit グレースケールで行います。");
            });
        self.about_open = about_open;
    }

    // ------------------------------------------------------- アクション処理

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::PickImage => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("画像", IMAGE_EXTENSIONS)
                    .set_title("画像を挿入")
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
            Action::NewMeasure => {
                let index = self.active;
                self.close_dialogs();
                self.measure_mode.open_new(&mut self.docs[index], index);
            }
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
                    "コマンドを実行しました。".to_owned()
                } else {
                    "変更はありません。".to_owned()
                };
                ctx.request_repaint();
            }
            Action::ReloadSources => {
                // 元ファイルを読み直したいときだけキャッシュを捨てる。
                self.cache.clear();
                let active = self.active;
                self.docs[active].invalidate_all();
                self.docs[active].recompute(&mut self.cache);
                self.status = "元ファイルを読み直して再計算しました。".to_owned();
                ctx.request_repaint();
            }
            Action::SaveHistory => self.save_history(),
            Action::OpenHistory => self.open_history(),
            Action::ApplyHistory => self.apply_history(),
            Action::ExportImage => self.export_image(),
            Action::CloseTab(i) => self.close_tab(i),
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn close_dialogs(&mut self) {
        self.scale_dialog.open = false;
        self.rotate_dialog.open = false;
        self.levels_dialog.open = false;
        // 測長モードも編集中のダイアログと同様に閉じる（コマンド構造の
        // 変更操作と競合しないように、破棄して終了する）。
        if self.measure_mode.open {
            let tab = self.measure_mode.tab;
            if tab < self.docs.len() {
                self.measure_mode.revert(&mut self.docs[tab]);
            } else {
                self.measure_mode.open = false;
            }
        }
    }

    /// パラメータ調整中はプレビューのため、結果を先に進めておく必要がある。
    fn dialog_open(&self) -> bool {
        self.scale_dialog.open
            || self.rotate_dialog.open
            || self.levels_dialog.open
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
        self.status = format!("{} 件の処理をコピーしました。", picked.len());
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
        let at = doc.paste_position();
        let range = doc.insert_commands(at, items);
        // 貼り付けた行を選択し直しておくと、続けて貼っても位置が分かりやすい。
        doc.select_indices(range);
        self.status = format!(
            "{count} 件の処理を {} 行目に貼り付けました（「再計算」で反映されます）。",
            at + 1
        );
    }

    fn edit_command(&mut self, index: usize) {
        self.close_dialogs();
        let doc = &mut self.docs[self.active];
        match doc.commands[index].command.clone() {
            Command::SetScale { .. } => self.scale_dialog.open_edit(doc, index),
            Command::Rotate { .. } => self.rotate_dialog.open_edit(doc, index),
            Command::Levels { .. } => self.levels_dialog.open_edit(doc, index),
            Command::Measure { .. } => {
                let tab = self.active;
                self.measure_mode.open_edit(doc, tab, index)
            }
            Command::InsertImage { path } => {
                // 画像挿入の「パラメータ」は読み込むファイルそのもの。
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("画像", IMAGE_EXTENSIONS)
                    .set_title("読み込むファイルを変更");
                if let Some(parent) = path.parent() {
                    dialog = dialog.set_directory(parent);
                }
                if let Some(new_path) = dialog.pick_file() {
                    doc.title = file_label(&new_path);
                    doc.commands[index].command = Command::InsertImage { path: new_path };
                    doc.invalidate_from(index);
                    doc.view.request_fit();
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
            Some(scale) => format!(
                "画像を挿入しました（メタデータからスケールを取得: {}）。",
                scale.describe()
            ),
            None => "画像を挿入しました（スケール情報なし）。".to_owned(),
        };
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.docs.len() {
            return;
        }
        self.close_dialogs();
        self.docs.remove(index);
        if self.docs.is_empty() {
            self.docs.push(Document::new("(空)"));
        }
        self.active = self.active.min(self.docs.len() - 1);
    }

    fn save_history(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("コマンド履歴 (JSON)", &["json"])
            .set_file_name("history.json")
            .set_title("コマンド履歴を保存")
            .save_file()
        else {
            return;
        };
        let file = HistoryFile::new(self.doc().commands.clone());
        match file.save(&path) {
            Ok(()) => {
                self.error = None;
                self.status = format!("{} に保存しました。", file_label(&path));
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn open_history(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("コマンド履歴 (JSON)", &["json"])
            .set_title("コマンド履歴を開く")
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
        self.status = "コマンド履歴を開きました。".to_owned();
    }

    fn apply_history(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("コマンド履歴 (JSON)", &["json"])
            .set_title("コマンド履歴を現在の画像に適用")
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
        self.status = format!("{count} 件の処理を追加しました。");
    }

    fn export_image(&mut self) {
        let Some(img) = self.doc().image().cloned() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG", &["png"])
            .add_filter("TIFF", &["tif", "tiff"])
            .set_file_name("export.png")
            .set_title("画像を書き出し")
            .save_file()
        else {
            return;
        };
        // 内部と同じ 16bit グレースケールのまま書き出す。
        match img.to_luma16_buffer().save(&path) {
            Ok(()) => {
                self.error = None;
                self.status = format!("{} に書き出しました。", file_label(&path));
            }
            Err(e) => self.error = Some(format!("書き出しに失敗しました: {e}")),
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
                "ドロップして画像を挿入",
                egui::FontId::proportional(24.0),
                egui::Color32::WHITE,
            );
        }
    }
}

impl eframe::App for TemApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        self.handle_dropped_files(&ctx, &mut actions);

        // 測長モード中にタブが切り替わったら、編集を破棄して終了する。
        if self.measure_mode.open && self.measure_mode.tab != self.active {
            self.close_dialogs();
            self.status = "タブが切り替わったため、測長モードを終了しました。".to_owned();
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

        // レベル補正の調整中は、表示の自動コントラストを切って
        // 補正結果そのものが見えるようにする。
        let auto = self.auto_contrast && !self.levels_dialog.open;
        let index = self.active;
        let range = self.docs[index].display_range(auto);
        let generation = self.docs[index].generation;
        // 測長モード中は画像上の入力（左ドラッグの移動・パン、ズーム）を
        // すべて measure_mode 側で処理する。
        let interactive = !self.measure_mode.open;
        self.last_hover = egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                let doc = &mut self.docs[index];
                let image = doc.image().cloned();
                let mut info = doc.view.show(ui, image.as_ref(), generation, range, interactive);

                // 測長オーバーレイ: 後段の回転等で画像が変わっていない
                // 測長コマンドだけを、そのコマンドの画像座標系で描く。
                // 測長モードで編集中のコマンドは draw_session 側が描くので
                // ここではスキップする（二重描画を防ぐ）。
                if let Some(img) = &image {
                    let painter = ui.painter_at(info.vp);
                    for i in 0..doc.commands.len() {
                        if self.measure_mode.open
                            && self.measure_mode.tab == index
                            && i == self.measure_mode.index
                        {
                            continue;
                        }
                        if let Command::Measure { data } = &doc.commands[i].command
                            && let Some(frame) = doc.input_to(i)
                            && std::sync::Arc::ptr_eq(&frame.image, img)
                        {
                            let computed = data.compute(&frame.image, frame.scale);
                            crate::measure_mode::draw_computed(
                                &painter,
                                &info,
                                &computed,
                                frame.scale,
                            );
                        }
                    }
                }

                // 測長モード中は編集セッションの描画とツール入力処理を重ねる。
                if self.measure_mode.open {
                    let painter = ui.painter_at(info.vp);
                    self.measure_mode.draw_session(&painter, &info, doc);
                    let hover = self.measure_mode.handle_overlay_input(ui, &info, doc);
                    info.hover_px = hover.hover_px;
                    info.hover_value = hover.hover_value;
                }
                info
            })
            .inner;

        self.ui_help_windows(&ctx);

        let doc = &mut self.docs[index];
        self.scale_dialog.show(&ctx, doc);
        self.rotate_dialog.show(&ctx, doc);
        self.levels_dialog.show(&ctx, doc);
        if self.measure_mode.open {
            let tab = self.measure_mode.tab;
            if tab < self.docs.len() {
                self.measure_mode.show_confirm_modal(&ctx, &mut self.docs[tab]);
                self.measure_mode.show_fit_popup(&ctx, &mut self.docs[tab]);
            }
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
