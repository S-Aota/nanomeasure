//! 測長モード。右の command_panel の位置に測長ツール UI を表示し、
//! 画像上のクリックでツールを作成する。
//!
//! データは `Command::Measure` へ常にライブ反映される（既存ダイアログと
//! 同じ方式）。決定で確定、キャンセルは確認を経て破棄。ツール操作は
//! Ctrl+Z / Ctrl+Shift+Z で undo / redo できる。

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, Context, CursorIcon, FontId, Painter, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};

use crate::command::Command;
use crate::dialogs::set_command;
use crate::document::Document;
use crate::frame::{format_length, Scale};
use crate::gray::Gray16;
use crate::measure::{
    AngleMode, ComputedMeasure, ComputedTool, FitMode, FitSettings, FitSign, MeasureData,
    MeasureTool,
    NewMeasureMode, Pt2, SnapLine, ToolKind, format_measurement, snap_angle_four, snap_distance,
};
use crate::measure_fit::{self, FitRegion, GaussFit};
use crate::view::ViewInfo;

/// undo / redo スタックの深さ上限。
const UNDO_LIMIT: usize = 100;

/// スナップ判定のしきい値（画面 px）。ズームによらず操作感を一定にする。
const SNAP_PX: f32 = 10.0;

/// 配置済みツールの選択判定のしきい値（画面 px）。端点・線分の両方に使う。
/// 画面 px 基準なので表示倍率を考慮した扱いになり、拡大しても画面上の
/// 判定半径は変わらない。
const PICK_PX: f32 = 20.0;

/// 範囲選択枠の辺の判定幅（画面 px）。
const RANGE_EDGE_PX: f32 = 6.0;

/// 二点間測長（赤）。
pub(crate) const COLOR_DISTANCE: Color32 = Color32::from_rgb(235, 70, 70);
/// 境界線・オフセット線（紫）。
pub(crate) const COLOR_GUIDE: Color32 = Color32::from_rgb(175, 90, 235);
/// フィッティング領域の枠。フィッティング設定 1〜3 に合わせて色を変える。
pub(crate) const COLOR_REGION_OFF: Color32 = Color32::from_rgb(240, 160, 60); // 1: オレンジ
pub(crate) const COLOR_REGION_GAUSSIAN: Color32 = Color32::from_rgb(90, 180, 240); // 2: 水色
pub(crate) const COLOR_REGION_DERIV: Color32 = Color32::from_rgb(150, 220, 90); // 3: 黄緑
/// 作成中・選択中の一時表示（橙）。
const COLOR_IN_PROGRESS: Color32 = Color32::from_rgb(255, 150, 60);

pub(crate) fn region_color(mode: FitMode) -> Color32 {
    match mode {
        FitMode::Off => COLOR_REGION_OFF,
        FitMode::Gaussian => COLOR_REGION_GAUSSIAN,
        FitMode::DerivativeGaussian => COLOR_REGION_DERIV,
    }
}

/// フィッティング枠の 4 辺それぞれの色。添字は `corners()` の辺の順
/// （0: -fit 側の辺, 1: +avg 側, 2: +fit 側, 3: -avg 側。辺 1/3 が
/// フィッティング方向に走る）。
/// ガウシアン系はフィッティング方向に走る辺を符号で明/暗にし、
/// 微分系は輝度が高くなる側の辺を明るく、逆の辺を暗くする。
pub(crate) fn region_edge_colors(mode: FitMode, sign: FitSign) -> [Color32; 4] {
    let base = region_color(mode);
    let bright = base.gamma_multiply(1.6);
    let dark = base.gamma_multiply(0.55);
    match (mode, sign) {
        (FitMode::Off, _) | (_, FitSign::Any) => [base; 4],
        (FitMode::Gaussian, FitSign::Positive) => [base, bright, base, bright],
        (FitMode::Gaussian, FitSign::Negative) => [base, dark, base, dark],
        // 微分の正 = fit 方向に輝度が上がる = +fit 側の辺（2）が明るい。
        (FitMode::DerivativeGaussian, FitSign::Positive) => [dark, base, bright, base],
        (FitMode::DerivativeGaussian, FitSign::Negative) => [bright, base, dark, base],
    }
}

/// 作成中のツールの状態（未確定なので data.tools には入っていない）。
#[derive(Clone, Copy, Debug, PartialEq)]
enum InProgress {
    /// 端点 1 を置いた後の二点間測長。p1_line はスナップ中の直線。
    Distance { p1: Pt2, p1_line: Option<SnapLine> },
    /// 端点 1 を置いた後の境界線。
    Boundary { p1: Pt2 },
    /// オフセット線: 選択した境界線と現在の距離（符号付き px）。
    OffsetPick { source: u64, distance: f64 },
    /// 直線複製: 元ツール、ドラッグ開始位置・現在位置、複製数。
    LinearDuplicate {
        src: u64,
        start: Pt2,
        current: Pt2,
        count: usize,
    },
}

/// ツールボタンの選択。None は非選択状態（Esc で解除、パンが使える）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolButton {
    #[default]
    Distance,
    Boundary,
    Offset,
    LinearDuplicate,
    /// 四角形で測長をまとめて選択し、一括移動する。
    RangeSelect,
}

impl ToolButton {
    pub fn label(self) -> &'static str {
        match self {
            Self::Distance => "二点間測長",
            Self::Boundary => "境界線",
            Self::Offset => "オフセット線",
            Self::LinearDuplicate => "直線複製",
            Self::RangeSelect => "範囲選択",
        }
    }
}

/// 範囲選択で選ばれた測長と選択枠（画像座標の軸平行矩形）。
#[derive(Clone, Debug, PartialEq)]
struct RangeSelection {
    ids: Vec<u64>,
    min: Pt2,
    max: Pt2,
}

/// undo / redo 1 段分。測長データと範囲選択（枠）をまとめて記録する。
/// 範囲選択の移動・変形は測長と同じ操作として戻す必要があるため。
#[derive(Clone)]
struct Snapshot {
    data: MeasureData,
    range_selection: Option<RangeSelection>,
}

/// 範囲選択ツールのドラッグ状態。
#[derive(Clone, Debug, PartialEq)]
enum RangeState {
    /// 枠を作成中（対角をドラッグ）。
    Drawing { start: Pt2, current: Pt2 },
    /// 枠の中をドラッグして選択測長をまとめて平行移動中。
    /// `orig` はドラッグ開始時の各測長の p1/p2。
    Moving {
        start: Pt2,
        orig_min: Pt2,
        orig_max: Pt2,
        orig: Vec<(u64, Pt2, Pt2)>,
    },
    /// 枠の辺・角をドラッグして拡大縮小中。反対側の辺を基準に、選択された
    /// 測長の p1/p2 も枠と同じ線形写像で動かす。`orig` はドラッグ開始時の
    /// 各測長の p1/p2。
    Resizing {
        orig_min: Pt2,
        orig_max: Pt2,
        /// どの辺がドラッグに追従するか（x/y それぞれ min/max 側）。
        min_x: bool,
        max_x: bool,
        min_y: bool,
        max_y: bool,
        orig: Vec<(u64, Pt2, Pt2)>,
    },
}

/// 枠への当たり判定の結果。
enum RangeHit {
    None,
    Move,
    Resize {
        min_x: bool,
        max_x: bool,
        min_y: bool,
        max_y: bool,
    },
}

/// 移動する端点の側。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointWhich {
    P1,
    P2,
}

/// 非選択状態で選択したツールのドラッグ移動の状態。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// 端点だけを動かす（もう片方の端点は固定）。
    Endpoint { id: u64, which: EndpointWhich },
    /// ツール全体を平行移動する。
    Whole {
        id: u64,
        start: Pt2,
        orig_p1: Pt2,
        orig_p2: Pt2,
    },
}

/// フィッティング設定ポップアップの編集対象（ツールごとの設定を持つ）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FitPopupTarget {
    /// 二点間測長の端点 1 / 端点 2。
    Dist1 { tool: u64 },
    Dist2 { tool: u64 },
    /// 境界線。
    Boundary { tool: u64 },
}

/// 結果リストの UI から集めた操作（描画中は data を借りているため後処理する）。
enum ResultAction {
    SelectNewGroup,
    SelectGroup(u64),
    BeginRename(u64),
    Rename(u64, String),
    DeleteTool(u64),
    /// グループと、そのグループに属するツールをまとめて削除。
    DeleteGroup(u64),
    /// グループ内で測定結果を上下に動かす（番号は表示時に振り直す）。
    MoveTool { gid: u64, id: u64, delta: isize },
    /// グループ内を代表座標（anchor_point）の x / y で昇順に並べ替える。
    SortGroup { gid: u64, key: SortKey },
    /// 全グループの統計データをクリップボードへコピー。
    CopyStats,
    /// 1 グループの測長結果一覧をクリップボードへコピー。
    CopyGroupData(u64),
}

/// グループ内ソートのキー。代表座標のどちらの軸で比較するか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortKey {
    X,
    Y,
}

#[derive(Default)]
pub struct MeasureMode {
    pub open: bool,
    /// 編集対象のタブとコマンド行。
    pub(crate) tab: usize,
    pub(crate) index: usize,
    created: bool,
    original: Option<Command>,
    /// 作業コピー。変更は常に doc 側のコマンドへライブ反映する。
    data: MeasureData,
    /// 選択中のツールボタン。None = 非選択（Esc で解除。パンは右ドラッグ）。
    tool: Option<ToolButton>,
    /// 二点間測長のフィッティング設定 UI で端点 1/2 のどちらを表示するか。
    ep_tab: bool,
    /// 非選択状態で選択した配置済みツール。
    selected: Option<u64>,
    /// 選択ツールのドラッグ移動中。
    drag: Option<Drag>,
    /// ドラッグ開始時のフィッティング済み計算結果。ドラッグ中は選択ツール
    /// だけ生の位置で描き、他のツールはこのキャッシュで描く（毎フレームの
    /// フィッティング再計算を避けつつ、二重線の重なりも出さない）。
    drag_fitted: Option<ComputedMeasure>,
    /// フィッティング設定ポップアップの編集対象。
    popup: Option<FitPopupTarget>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// キャンセルの確認モーダルを表示中か。
    confirm_cancel: bool,
    /// 名前編集中のグループ（id）。
    rename: Option<u64>,
    rename_text: String,
    /// 作成中のツール（端点 1 だけ置かれたものや、選択中の元ツール）。
    in_progress: Option<InProgress>,
    /// 直線複製のホイール入力の累積（20.0 たまるごとに複製数を 1 増減）。
    scroll_accum: f32,
    /// 範囲選択で選ばれた測長と枠。
    range_selection: Option<RangeSelection>,
    /// 範囲選択ツールのドラッグ状態（枠の作成・移動・リサイズ）。
    range_state: Option<RangeState>,
}

impl MeasureMode {
    /// 新しい測長コマンドを追加してモードに入る。
    pub fn open_new(&mut self, doc: &mut Document, tab: usize) {
        let index = doc.push_command(Command::Measure {
            data: MeasureData::default(),
        });
        self.start(doc, tab, index, true);
    }

    /// 既存の測長コマンドを編集する。
    pub fn open_edit(&mut self, doc: &mut Document, tab: usize, index: usize) {
        self.start(doc, tab, index, false);
    }

    fn start(&mut self, doc: &Document, tab: usize, index: usize, created: bool) {
        let original = doc.commands.get(index).map(|c| c.command.clone());
        self.data = match &original {
            Some(Command::Measure { data }) => data.clone(),
            _ => MeasureData::default(),
        };
        self.tab = tab;
        self.index = index;
        self.created = created;
        self.original = original;
        self.open = true;
        self.tool = None;
        self.ep_tab = false;
        self.selected = None;
        self.drag = None;
        self.drag_fitted = None;
        self.popup = None;
        self.undo.clear();
        self.redo.clear();
        self.confirm_cancel = false;
        self.rename = None;
        self.rename_text.clear();
        self.in_progress = None;
        self.scroll_accum = 0.0;
        self.range_selection = None;
        self.range_state = None;
    }

    /// 編集を破棄してモードを閉じる（新規分のコマンドは取り除く）。
    pub fn revert(&mut self, doc: &mut Document) {
        if !self.open {
            return;
        }
        if matches!(
            doc.commands.get(self.index).map(|c| &c.command),
            Some(Command::Measure { .. })
        ) {
            if self.created {
                doc.remove_command(self.index);
            } else if let Some(original) = self.original.take() {
                set_command(doc, self.index, original);
            }
        }
        self.open = false;
        self.undo.clear();
        self.redo.clear();
    }

    fn finish(&mut self) {
        self.original = None;
        self.open = false;
        self.undo.clear();
        self.redo.clear();
    }

    // -------------------------------------------------- undo / redo

    /// 1 論理操作の記録を開始する（スライダーのドラッグ開始時などに呼ぶ）。
    fn begin_change(&mut self) {
        self.undo.push(Snapshot {
            data: self.data.clone(),
            range_selection: self.range_selection.clone(),
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// 現状をスナップショットへまとめる。
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            data: self.data.clone(),
            range_selection: self.range_selection.clone(),
        }
    }

    /// 記録済みの変更を doc 側へ反映する。
    fn apply_change(&mut self, doc: &mut Document) {
        // リンク中は端点 2 の設定を端点 1 に追従させる（doc と self.data を
        // 常に一致させるため、反映のたびに同期する）。
        if self.data.link_fit {
            self.data.dist_fit2 = self.data.dist_fit1;
        }
        set_command(
            doc,
            self.index,
            Command::Measure {
                data: self.data.clone(),
            },
        );
    }

    /// クリック 1 回で完結する変更（ラジオ選択など）。
    fn change_once(&mut self, doc: &mut Document) {
        self.begin_change();
        self.apply_change(doc);
    }

    /// 現状を undo に積んでから変更を適用する（ツール追加・削除など）。
    fn mutate(&mut self, doc: &mut Document, f: impl FnOnce(&mut MeasureData)) {
        self.begin_change();
        f(&mut self.data);
        self.apply_change(doc);
    }

    pub fn handle_shortcuts(&mut self, ctx: &Context, doc: &mut Document) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let undo = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
        let redo = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::Z,
        );
        if ctx.input_mut(|i| i.consume_shortcut(&undo)) {
            self.undo(doc);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&redo)) {
            self.redo(doc);
        }
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(self.snapshot());
            if self.redo.len() > UNDO_LIMIT {
                self.redo.remove(0);
            }
            self.restore_snapshot(prev, doc);
        }
    }

    fn redo(&mut self, doc: &mut Document) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.restore_snapshot(next, doc);
        }
    }

    /// スナップショットの内容（測長データ + 範囲選択の枠）を復元する。
    /// ドラッグ途中の状態は復元対象にしない。
    fn restore_snapshot(&mut self, snap: Snapshot, doc: &mut Document) {
        self.data = snap.data;
        self.range_selection = snap.range_selection;
        self.range_state = None;
        self.apply_change(doc);
    }

    // ------------------------------------------------------ パネル UI

    /// 右パネル（command_panel と同じ場所）に測長 UI を表示する。
    /// 戻り値は「保存」ボタンが押されたか（呼び出し側でファイルへ保存する）。
    pub fn show_panel(&mut self, ui: &mut Ui, doc: &mut Document) -> bool {
        if !self.open {
            return false;
        }
        // 編集中にコマンド行が消えたり差し替わったら閉じる。
        if !matches!(
            doc.commands.get(self.index).map(|c| &c.command),
            Some(Command::Measure { .. })
        ) {
            self.open = false;
            return false;
        }

        let mut confirmed = false;
        let mut cancel_requested = false;
        let mut save_requested = false;

        egui::Panel::right("command_panel")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.strong("測長");
                ui.separator();

                save_requested |= self.settings_ui(ui, doc);
                ui.separator();
                self.tools_ui(ui, doc);
                ui.separator();

                // 決定・キャンセルは最下部に固定し、残りの高さを結果リストに使う。
                egui::Panel::bottom("measure_confirm_buttons").show(ui, |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if ui.button("決定").clicked() {
                            confirmed = true;
                        }
                        if ui.button("キャンセル").clicked() {
                            cancel_requested = true;
                        }
                    });
                    ui.add_space(2.0);
                });
                self.results_ui(ui, doc);
            });

        if confirmed {
            self.finish();
        } else if cancel_requested {
            // 変更が無ければそのまま破棄してよい。
            let unchanged = self
                .original
                .as_ref()
                .is_some_and(|o| o == &Command::Measure { data: self.data.clone() });
            if unchanged {
                self.revert(doc);
            } else {
                self.confirm_cancel = true;
            }
        }
        save_requested
    }

    /// フィッティング領域をダブルクリック/右クリックしたときに開く
    /// 設定ポップアップ。
    pub fn show_fit_popup(&mut self, ctx: &Context, doc: &mut Document) {
        let Some(target) = self.popup else {
            return;
        };
        let mut open = true;
        let mut outcome = FitUiOutcome::default();
        let mut target_gone = false;
        egui::Window::new("端点のフィッティング設定")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("この端点のフィッティング方法と検出領域を変更できます。");
                ui.add_space(4.0);
                // ツールごとの設定を直接編集する（この測長だけに効く）。
                let fit = match target {
                    FitPopupTarget::Dist1 { tool } => self
                        .data
                        .tools
                        .iter_mut()
                        .find(|t| t.id() == tool)
                        .and_then(|t| match t {
                            MeasureTool::Distance { fit1, .. } => Some(fit1),
                            _ => None,
                        }),
                    FitPopupTarget::Dist2 { tool } => self
                        .data
                        .tools
                        .iter_mut()
                        .find(|t| t.id() == tool)
                        .and_then(|t| match t {
                            MeasureTool::Distance { fit2, .. } => Some(fit2),
                            _ => None,
                        }),
                    FitPopupTarget::Boundary { tool } => self
                        .data
                        .tools
                        .iter_mut()
                        .find(|t| t.id() == tool)
                        .and_then(|t| match t {
                            MeasureTool::Boundary { fit, .. } => Some(fit),
                            _ => None,
                        }),
                };
                match fit {
                    Some(fit) => outcome = fit_settings_ui(ui, fit),
                    // 対象のツールが消えていたら閉じる。
                    None => target_gone = true,
                }
                ui.separator();
                // フィッティングの確認用プロット（輝度とフィット曲線）。
                self.fit_plot_ui(ui, doc, target);
            });
        if !open || target_gone {
            self.popup = None;
        }
        self.handle_fit_outcome(doc, outcome);
    }

    /// ポップアップ下部のフィッティング確認プロット。輝度プロファイル
    /// （微分モードはその微分）を灰線、フィット曲線を橙線で描く。
    /// クリック位置そのままのモードではクリック位置（領域の中心）に縦線を引く。
    fn fit_plot_ui(&self, ui: &mut Ui, doc: &Document, target: FitPopupTarget) {
        let Some(img) = doc.input_to(self.index).map(|f| f.image.clone()) else {
            return;
        };
        let scale = doc.input_to(self.index).and_then(|f| f.scale);
        let computed = self.data.compute(&img, scale);
        let tool_id = match target {
            FitPopupTarget::Dist1 { tool }
            | FitPopupTarget::Dist2 { tool }
            | FitPopupTarget::Boundary { tool } => tool,
        };
        let Some(t) = computed.by_id(tool_id) else {
            return;
        };
        let region_index = match target {
            FitPopupTarget::Dist1 { .. } => 0,
            FitPopupTarget::Dist2 { .. } => 1,
            FitPopupTarget::Boundary { .. } => 0,
        };
        let Some(region) = t.fit_regions.get(region_index) else {
            return;
        };
        let (profile, fit) = measure_fit::fit_profile(&img, region);
        draw_profile_plot(ui, &profile, fit, region);
    }

    /// キャンセルの確認モーダル。
    pub fn show_confirm_modal(&mut self, ctx: &Context, doc: &mut Document) {
        if !self.confirm_cancel || !self.open {
            return;
        }
        let mut close = false;
        let mut confirmed = false;
        egui::Window::new("キャンセルの確認")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("編集内容を保存せずに測長モードを終了しますか？");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("はい").clicked() {
                        confirmed = true;
                        close = true;
                    }
                    if ui.button("いいえ").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.confirm_cancel = false;
        }
        if confirmed {
            self.revert(doc);
        }
    }

    /// 設定 UI。戻り値は「保存」ボタンが押されたか。
    fn settings_ui(&mut self, ui: &mut Ui, doc: &mut Document) -> bool {
        let mut save_requested = false;
        ui.strong("設定");
        ui.horizontal(|ui| {
            ui.label("角度:");
            if ui
                .radio_value(&mut self.data.prefs.angle, AngleMode::FourDir, "4方向")
                .changed()
                || ui
                    .radio_value(&mut self.data.prefs.angle, AngleMode::Free, "自由")
                    .changed()
            {
                self.change_once(doc);
            }
        });
        ui.horizontal(|ui| {
            ui.label("スナップ:");
            if ui
                .radio_value(&mut self.data.prefs.snap, true, "on")
                .changed()
                || ui
                    .radio_value(&mut self.data.prefs.snap, false, "off")
                    .changed()
            {
                self.change_once(doc);
            }
        });
        ui.horizontal(|ui| {
            ui.label("新規測長:");
            if ui
                .radio_value(
                    &mut self.data.prefs.new_measure,
                    NewMeasureMode::NewGroup,
                    "グループを追加",
                )
                .changed()
                || ui
                    .radio_value(
                        &mut self.data.prefs.new_measure,
                        NewMeasureMode::Keep,
                        "そのまま",
                    )
                    .changed()
            {
                self.change_once(doc);
            }
        });

        // 測定結果 JSON の出力先。{dir} / {filename} は保存時に画像パスから
        // 解決するので、自由なパスを書いてもよい。
        ui.horizontal(|ui| {
            ui.label("出力:");
            let resp = ui
                .add(
                    egui::TextEdit::singleline(&mut self.data.output_path)
                        .desired_width(180.0)
                        .hint_text("{dir}/{filename}_result.json"),
                )
                .on_hover_text(
                    "測定結果 JSON の出力先。{dir} は開いている画像のフォルダ、\n\
                     {filename} は拡張子なしのファイル名に置き換わります。\n\
                     空欄にすると保存しません。",
                );
            if resp.gained_focus() {
                self.begin_change();
            }
            if resp.changed() {
                self.apply_change(doc);
            }
        });
        ui.horizontal(|ui| {
            if ui
                .button("保存")
                .on_hover_text("測定結果を JSON で保存（再計算のときにも保存されます）")
                .clicked()
            {
                save_requested = true;
            }
            ui.weak("再計算時に自動保存");
        });
        save_requested
    }

    fn tools_ui(&mut self, ui: &mut Ui, doc: &mut Document) {
        ui.strong("ツール");
        ui.add_space(2.0);
        // 章ごとに分けて配置する。
        ui.label(egui::RichText::new("測長").small().weak());
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            self.tool_button(ui, ToolButton::Distance);
        });
        ui.label(egui::RichText::new("補助線").small().weak());
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            self.tool_button(ui, ToolButton::Boundary);
            self.tool_button(ui, ToolButton::Offset);
        });
        ui.label(egui::RichText::new("複製").small().weak());
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            self.tool_button(ui, ToolButton::LinearDuplicate);
        });
        ui.label(egui::RichText::new("選択").small().weak());
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            self.tool_button(ui, ToolButton::RangeSelect);
        });
        ui.add_space(4.0);

        // フィッティング設定（二点間測長と境界線のみ）。
        match self.tool {
            Some(ToolButton::Distance) => {
                ui.weak("新しく作る測長の既定値。個別の変更は画像上の領域をダブルクリック");
                ui.horizontal(|ui| {
                    ui.label("端点:");
                    if ui.selectable_label(!self.ep_tab, "1").clicked() {
                        self.ep_tab = false;
                    }
                    if ui.selectable_label(self.ep_tab, "2").clicked() {
                        self.ep_tab = true;
                    }
                    if ui.toggle_value(&mut self.data.link_fit, "link").changed() {
                        // ON では端点 2 が端点 1 のコピーに置き換わるので、
                        // ON/OFF とも 1 操作として記録する。
                        self.begin_change();
                        self.apply_change(doc);
                    }
                });
                let outcome = if self.ep_tab {
                    let mut outcome = FitUiOutcome::default();
                    // リンク中は端点 2 の個別編集を無効化（値は端点 1 のコピー）。
                    ui.add_enabled_ui(!self.data.link_fit, |ui| {
                        outcome = fit_settings_ui(ui, &mut self.data.dist_fit2);
                    });
                    outcome
                } else {
                    fit_settings_ui(ui, &mut self.data.dist_fit1)
                };
                self.handle_fit_outcome(doc, outcome);
            }
            Some(ToolButton::Boundary) => {
                ui.weak("新しく作る境界線の既定値。個別の変更は画像上の領域をダブルクリック");
                let outcome = fit_settings_ui(ui, &mut self.data.boundary_fit);
                self.handle_fit_outcome(doc, outcome);
            }
            Some(ToolButton::Offset) => {
                ui.weak("画像上の境界線をクリック → クリックで距離を決定");
                self.offset_fine_tune_ui(ui, doc);
            }
            Some(ToolButton::LinearDuplicate) => {
                ui.weak("測長・境界線をクリック → マウス移動で方向と距離を指定 → クリックで確定。ホイールで複製数 (1-20)");
            }
            Some(ToolButton::RangeSelect) => {
                ui.weak("ドラッグで四角形を作ると、中心が枠内の測長をまとめて選択。枠の中のドラッグで一括移動、枠の辺・角のドラッグで測長ごと拡大縮小");
            }
            None => {
                ui.weak("画像上の測長・境界線を直接ドラッグで移動（Esc で解除）");
            }
        }
    }

    fn tool_button(&mut self, ui: &mut Ui, tool: ToolButton) {
        let selected = self.tool == Some(tool);
        if ui.selectable_label(selected, tool.label()).clicked() {
            // もう一度押すと選択解除（Esc と同じ）。
            self.tool = if selected { None } else { Some(tool) };
            self.in_progress = None;
            self.selected = None;
            self.drag = None;
            self.drag_fitted = None;
            self.range_selection = None;
            self.range_state = None;
        }
    }

    fn handle_fit_outcome(&mut self, doc: &mut Document, outcome: FitUiOutcome) {
        if outcome.began_change || outcome.committed {
            self.begin_change();
        }
        if outcome.changed || outcome.committed {
            self.apply_change(doc);
        }
    }

    /// 最後に作成したオフセット線の距離を数値で微調整する。
    fn offset_fine_tune_ui(&mut self, ui: &mut Ui, doc: &mut Document) {
        let last = self
            .data
            .tools
            .iter()
            .rev()
            .find_map(|t| match t {
                MeasureTool::Offset { id, distance, .. } => Some((*id, *distance)),
                _ => None,
            });
        let Some((id, mut distance)) = last else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label("オフセット:");
            let resp = ui.add(egui::DragValue::new(&mut distance).speed(0.5));
            if resp.drag_started() {
                self.begin_change();
            }
            if resp.changed() {
                if let Some(t) = self.data.tools.iter_mut().find(|t| t.id() == id)
                    && let MeasureTool::Offset { distance: d, .. } = t
                {
                    *d = distance;
                }
                self.apply_change(doc);
            }
            ui.label("px");
        });
    }

    fn results_ui(&mut self, ui: &mut Ui, doc: &mut Document) {
        ui.strong("測定結果");
        let (img, scale) = match doc.input_to(self.index) {
            Some(frame) => (frame.image.clone(), frame.scale),
            None => return,
        };
        let computed = self.data.compute(&img, scale);
        let mut actions = Vec::new();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // 特殊行 "new group"。チェックされていたら新測定でグループ生成。
                if ui
                    .radio(self.data.active_group.is_none(), "new group")
                    .clicked()
                {
                    actions.push(ResultAction::SelectNewGroup);
                }

                for g in &self.data.groups {
                    let gid = g.id;
                    let selected = self.data.active_group == Some(gid);
                    ui.horizontal(|ui| {
                        if ui.radio(selected, "").clicked() {
                            actions.push(ResultAction::SelectGroup(gid));
                        }
                        if self.rename == Some(gid) {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.rename_text)
                                    .desired_width(110.0),
                            );
                            if resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                actions.push(ResultAction::Rename(
                                    gid,
                                    self.rename_text.trim().to_owned(),
                                ));
                            } else if resp.lost_focus() {
                                // Enter 以外（Esc やクリック逸れ）は名前変更を取り消す。
                                actions.push(ResultAction::Rename(gid, String::new()));
                            }
                        } else {
                            let name = if g.name.is_empty() {
                                "(名前なし)".to_owned()
                            } else {
                                g.name.clone()
                            };
                            let resp = ui
                                .add(egui::Label::new(name).sense(egui::Sense::click()))
                                .on_hover_text("ダブルクリックで名前を変更。右クリックでコピー");
                            if resp.double_clicked() {
                                actions.push(ResultAction::BeginRename(gid));
                            }
                            // 右クリックメニュー: クリップボードへのコピー。
                            resp.context_menu(|ui| {
                                if ui.button("統計データをコピー").clicked() {
                                    ui.close();
                                    actions.push(ResultAction::CopyStats);
                                }
                                let has_data = !self.data.group_tools(gid).is_empty();
                                if ui
                                    .add_enabled(
                                        has_data,
                                        egui::Button::new("データ一覧をコピー"),
                                    )
                                    .on_disabled_hover_text("このグループに測長結果はありません")
                                    .clicked()
                                {
                                    ui.close();
                                    actions.push(ResultAction::CopyGroupData(gid));
                                }
                                if ui
                                    .add_enabled(has_data, egui::Button::new("x 座標でソート"))
                                    .on_disabled_hover_text("このグループに測長結果はありません")
                                    .clicked()
                                {
                                    ui.close();
                                    actions.push(ResultAction::SortGroup { gid, key: SortKey::X });
                                }
                                if ui
                                    .add_enabled(has_data, egui::Button::new("y 座標でソート"))
                                    .on_disabled_hover_text("このグループに測長結果はありません")
                                    .clicked()
                                {
                                    ui.close();
                                    actions.push(ResultAction::SortGroup { gid, key: SortKey::Y });
                                }
                            });
                        }
                        // グループごとの一括削除。
                        if ui
                            .small_button("×")
                            .on_hover_text("グループとその測長結果をすべて削除")
                            .clicked()
                        {
                            actions.push(ResultAction::DeleteGroup(gid));
                        }
                        if let Some(stat) = group_stat(&computed, gid, scale) {
                            ui.label(egui::RichText::new(stat).weak());
                        }
                    });

                    // グループ内の測定結果（インデント + 自動採番）。番号は
                    // 並べ替えるたびに先頭から振り直す。
                    let ids = self.data.group_tools(gid);
                    for (n, tid) in ids.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            match computed.by_id(*tid) {
                                Some(t) => {
                                    let value = t
                                        .length_px
                                        .map(|l| format_measurement(l, scale))
                                        .unwrap_or_default();
                                    ui.label(format!("#{}  {}", n + 1, value));
                                }
                                None => {
                                    ui.label(format!("#{}", n + 1));
                                }
                            }
                            if ui
                                .small_button("×")
                                .on_hover_text("この測定を削除")
                                .clicked()
                            {
                                actions.push(ResultAction::DeleteTool(*tid));
                            }
                            if ui
                                .add_enabled(n > 0, egui::Button::new("▲").small())
                                .on_hover_text("グループ内で上へ")
                                .clicked()
                            {
                                actions.push(ResultAction::MoveTool {
                                    gid,
                                    id: *tid,
                                    delta: -1,
                                });
                            }
                            if ui
                                .add_enabled(n + 1 < ids.len(), egui::Button::new("▼").small())
                                .on_hover_text("グループ内で下へ")
                                .clicked()
                            {
                                actions.push(ResultAction::MoveTool {
                                    gid,
                                    id: *tid,
                                    delta: 1,
                                });
                            }
                        });
                    }
                }
            });

        for action in actions {
            match action {
                ResultAction::SelectNewGroup => {
                    self.data.active_group = None;
                    self.apply_change(doc);
                }
                ResultAction::SelectGroup(gid) => {
                    self.data.active_group = Some(gid);
                    self.apply_change(doc);
                }
                ResultAction::BeginRename(gid) => {
                    self.rename = Some(gid);
                    self.rename_text = self
                        .data
                        .groups
                        .iter()
                        .find(|g| g.id == gid)
                        .map(|g| g.name.clone())
                        .unwrap_or_default();
                }
                ResultAction::Rename(gid, name) => {
                    self.rename = None;
                    if !name.is_empty() {
                        // 空文字列はキャンセル扱い（名前を空にはしない）。
                        self.mutate(doc, |data| {
                            if let Some(g) = data.groups.iter_mut().find(|g| g.id == gid) {
                                g.name = name;
                            }
                        });
                    }
                }
                ResultAction::DeleteTool(id) => {
                    // オフセット線がこのツールを参照していたら一緒に消す。
                    let ids: Vec<u64> = self
                        .data
                        .tools
                        .iter()
                        .filter(|t| {
                            matches!(t, MeasureTool::Offset { source, .. } if *source == id)
                        })
                        .map(|t| t.id())
                        .collect();
                    self.mutate(doc, |data| {
                        data.tools
                            .retain(|t| t.id() != id && !ids.contains(&t.id()));
                    });
                }
                ResultAction::DeleteGroup(gid) => {
                    if self.rename == Some(gid) {
                        self.rename = None;
                    }
                    // グループ内のツールと、それらを参照するオフセット線を消す。
                    let ids: Vec<u64> = self
                        .data
                        .tools
                        .iter()
                        .filter(|t| {
                            matches!(
                                t,
                                MeasureTool::Distance { group, .. }
                                    | MeasureTool::Boundary { group, .. }
                                    if *group == gid
                            )
                        })
                        .map(|t| t.id())
                        .collect();
                    self.mutate(doc, |data| {
                        data.tools.retain(|t| {
                            !matches!(t, MeasureTool::Offset { source, .. } if ids.contains(source))
                                && !ids.contains(&t.id())
                        });
                        data.groups.retain(|g| g.id != gid);
                        if data.active_group == Some(gid) {
                            data.active_group = None;
                        }
                    });
                }
                ResultAction::MoveTool { gid, id, delta } => {
                    self.mutate(doc, |data| move_tool_in_group(data, gid, id, delta));
                }
                ResultAction::SortGroup { gid, key } => {
                    self.mutate(doc, |data| sort_group(data, &computed, gid, key));
                }
                ResultAction::CopyStats => {
                    ui.ctx().copy_text(stats_csv(&self.data, &computed, scale));
                }
                ResultAction::CopyGroupData(gid) => {
                    ui.ctx().copy_text(group_data_csv(gid, &self.data, &computed, scale));
                }
            }
        }
    }
}

/// グループ内の表示順でツールを上下に動かす。`data.tools` 内で隣接する
/// 同じグループのツールと位置を入れ替える（番号は表示時に振り直される）。
fn move_tool_in_group(data: &mut MeasureData, gid: u64, id: u64, delta: isize) {
    let Some(i) = data.tools.iter().position(|t| t.id() == id) else {
        return;
    };
    let step = delta.signum();
    let mut j = i as isize + step;
    while j >= 0 && (j as usize) < data.tools.len() {
        if matches!(&data.tools[j as usize], MeasureTool::Distance { group, .. } if *group == gid)
        {
            data.tools.swap(i, j as usize);
            return;
        }
        j += step;
    }
}

/// グループ内の測定ツールを代表座標の x / y で昇順に並べ替える。
/// move_tool_in_group と同様、ツール本体（ID はそのまま）を並べ替える。
/// グループ所属ツールをソート済み順に取り出して元のスロットへ書き戻すので、
/// グループ外のツールの並びは変わらない。
fn sort_group(data: &mut MeasureData, computed: &ComputedMeasure, gid: u64, key: SortKey) {
    let mut ids = data.group_tools(gid);
    let pos = |id: u64| computed.by_id(id).and_then(|t| t.anchor_point());
    ids.sort_by(|a, b| {
        let (Some(pa), Some(pb)) = (pos(*a), pos(*b)) else {
            return std::cmp::Ordering::Equal;
        };
        let (va, vb) = match key {
            SortKey::X => (pa.x, pb.x),
            SortKey::Y => (pa.y, pb.y),
        };
        va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let members: Vec<MeasureTool> = ids
        .iter()
        .filter_map(|&id| data.tools.iter().find(|t| t.id() == id).cloned())
        .collect();
    let mut it = members.into_iter();
    for t in &mut data.tools {
        if matches!(t, MeasureTool::Distance { group, .. } if *group == gid) {
            if let Some(m) = it.next() {
                *t = m;
            }
        }
    }
}

/// 測長コマンド `index` の測定結果を JSON で保存する。
/// 出力先は `data.output_path`（`{dir}` / `{filename}` は画像パスから解決）。
/// 空文字列のときは保存しない（Ok(None)）。戻り値は実際に保存したパス。
pub fn save_measure_json(doc: &Document, index: usize) -> Result<Option<PathBuf>, String> {
    let Some(item) = doc.commands.get(index) else {
        return Err("測長コマンドではありません".to_owned());
    };
    let Command::Measure { data } = &item.command else {
        return Err("測長コマンドではありません".to_owned());
    };
    let template = data.output_path.trim();
    if template.is_empty() {
        return Ok(None);
    }
    let img_path = doc
        .image_path_at(index)
        .ok_or_else(|| "画像がありません（画像を挿入してから保存してください）".to_owned())?
        .to_path_buf();
    let Some(frame) = doc.input_to(index) else {
        return Err("結果がまだ計算されていません".to_owned());
    };
    let path = resolve_output_path(template, &img_path);
    // JSON の先頭にはファイル名を出す。
    let filename = img_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned());
    let computed = data.compute(&frame.image, frame.scale);
    let groups: Vec<serde_json::Value> = data
        .groups
        .iter()
        .map(|g| {
            let values: Vec<String> = data
                .group_tools(g.id)
                .iter()
                .filter_map(|tid| {
                    computed
                        .by_id(*tid)
                        .and_then(|t| t.length_px)
                        .map(|l| format_measurement(l, frame.scale))
                })
                .collect();
            serde_json::json!({ "name": g.name, "values": values })
        })
        .collect();
    let json = serde_json::json!({ "filename": filename, "groups": groups });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{} に保存できません: {e}", path.to_string_lossy()))?;
    Ok(Some(path))
}

/// `{dir}` / `{filename}` を画像パスから解決する（画像出力コマンドでも共用）。
pub(crate) fn resolve_output_path(template: &str, img_path: &Path) -> PathBuf {
    let dir = img_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".".to_owned());
    let stem = img_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            img_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".to_owned())
        });
    PathBuf::from(template.replace("{dir}", &dir).replace("{filename}", &stem))
}

/// フィッティング設定 UI の変更検出結果。
#[derive(Default)]
struct FitUiOutcome {
    /// スライダーのドラッグが始まった（この 1 操作の記録を開始する）。
    began_change: bool,
    /// 値が変わった（doc へ反映する）。
    changed: bool,
    /// ラジオのクリック（1 クリック = 1 操作として即記録）。
    committed: bool,
}

/// 符号選択（auto / positive / negative を全部表示して 1 つ選ぶ）。
/// 変更されたら true。
fn sign_select_ui(ui: &mut Ui, sign: &mut FitSign) -> bool {
    let mut changed = false;
    for value in [FitSign::Any, FitSign::Positive, FitSign::Negative] {
        if ui.selectable_label(*sign == value, value.label()).clicked() {
            *sign = value;
            changed = true;
        }
    }
    changed
}

/// フィッティング設定の共通 UI。
fn fit_settings_ui(ui: &mut Ui, settings: &mut FitSettings) -> FitUiOutcome {
    let mut outcome = FitUiOutcome::default();
    // モード選択。符号はモード 2/3 共通（同時に有効になるのは 1 つなので、
    // 有効なツールの符号をそのまま使う）。両方の行のボタンで同じ値を選ぶ。
    for (label, mode) in [
        ("1. クリック位置そのまま", FitMode::Off),
        ("2. ガウシアン（境界線検出）", FitMode::Gaussian),
        ("3. 微分ガウシアン（ステップ）", FitMode::DerivativeGaussian),
    ] {
        ui.horizontal(|ui| {
            if ui.radio_value(&mut settings.mode, mode, label).changed() {
                outcome.committed = true;
            }
            if mode != FitMode::Off && sign_select_ui(ui, &mut settings.sign) {
                outcome.committed = true;
            }
        });
    }
    let w = ui.add(egui::Slider::new(&mut settings.width_px, 1..=20).text("検出領域の横"));
    if w.drag_started() {
        outcome.began_change = true;
    }
    if w.changed() {
        outcome.changed = true;
    }
    let l = ui.add(egui::Slider::new(&mut settings.length_px, 1..=50).text("検出領域の縦"));
    if l.drag_started() {
        outcome.began_change = true;
    }
    if l.changed() {
        outcome.changed = true;
    }
    outcome
}

/// プロファイルとフィット曲線のプロット。フィット中心の位置に点を打ち、
/// クリック位置そのままのモードではクリック位置（= 領域の中心）に縦線を引く。
fn draw_profile_plot(ui: &mut Ui, profile: &[f64], fit: Option<GaussFit>, region: &FitRegion) {
    let n = profile.len();
    let label = if region.mode == FitMode::DerivativeGaussian {
        "輝度の微分"
    } else {
        "輝度"
    };
    ui.label(egui::RichText::new(label).small().weak());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(360.0, 100.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(28));
    if n < 2 {
        return;
    }
    let (mut lo, mut hi) = profile.iter().cloned().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(l, h), v| (l.min(v), h.max(v)),
    );
    if hi - lo < 1e-9 {
        lo -= 1.0;
        hi += 1.0;
    }
    let x = |i: f64| rect.left() + (i / (n - 1) as f64) as f32 * rect.width();
    let y = |v: f64| {
        rect.bottom() - ((v - lo) / (hi - lo)) as f32 * (rect.height() - 4.0) - 2.0
    };

    // プロファイル（灰）。
    let points: Vec<Pos2> = profile
        .iter()
        .enumerate()
        .map(|(i, &v)| Pos2::new(x(i as f64), y(v)))
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(1.5, Color32::from_gray(170))));

    // フィット曲線（橙）とフィット中心の点。
    if let Some(f) = fit {
        let points: Vec<Pos2> = (0..n)
            .map(|i| {
                let xi = i as f64;
                let v = f.amplitude * (-((xi - f.mu).powi(2)) / (2.0 * f.sigma * f.sigma)).exp()
                    + f.baseline;
                Pos2::new(x(xi), y(v))
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.5, COLOR_IN_PROGRESS)));
        painter.circle_filled(
            Pos2::new(x(f.mu), y(f.amplitude + f.baseline)),
            2.5,
            COLOR_IN_PROGRESS,
        );
    }

    // クリック位置そのままのモード: クリック位置 = 領域の中心 =
    // プロファイルの中央。そこに縦線を引く。
    if region.mode == FitMode::Off {
        let cx = x((n - 1) as f64 * 0.5);
        painter.line_segment(
            [Pos2::new(cx, rect.top() + 2.0), Pos2::new(cx, rect.bottom() - 2.0)],
            Stroke::new(1.0, Color32::from_rgb(255, 220, 120)),
        );
    }
}

/// 単位なしの数値文字列（スケールがあれば実寸の値、なければ px）。
fn value_number(px: f64, scale: Option<Scale>) -> String {
    match scale {
        Some(s) => format_length(px * s.per_px()),
        None => format_length(px),
    }
}

/// CSV の 1 フィールド（カンマ・引用符・改行を含む名前は引用符で囲む）。
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// 全グループの統計データを CSV にする。
/// ヘッダー: グループ名, サンプル数, 平均, 標準偏差（単位なし・実寸の値）。
fn stats_csv(data: &MeasureData, computed: &ComputedMeasure, scale: Option<Scale>) -> String {
    let mut out = String::from("グループ名, サンプル数, 平均, 標準偏差\n");
    for g in &data.groups {
        let values: Vec<f64> = computed
            .tools
            .iter()
            .filter(|t| t.group == Some(g.id) && t.kind == ToolKind::Distance)
            .filter_map(|t| t.length_px)
            .collect();
        if values.is_empty() {
            // 測長結果の無いグループは行を出さない。
            continue;
        }
        let n = values.len();
        let mean = values.iter().sum::<f64>() / n as f64;
        let sd = if n < 2 {
            String::new()
        } else {
            let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
            value_number(var.sqrt(), scale)
        };
        out += &format!(
            "{}, {}, {}, {}\n",
            csv_field(&g.name),
            n,
            value_number(mean, scale),
            sd,
        );
    }
    out
}

/// 1 グループの測長結果一覧を CSV にする。ヘッダー: 番号, 値。
fn group_data_csv(
    gid: u64,
    data: &MeasureData,
    computed: &ComputedMeasure,
    scale: Option<Scale>,
) -> String {
    let mut out = String::from("番号, 値\n");
    for (n, tid) in data.group_tools(gid).iter().enumerate() {
        if let Some(t) = computed.by_id(*tid)
            && let Some(len) = t.length_px
        {
            out += &format!("{}, {}\n", n + 1, value_number(len, scale));
        }
    }
    out
}

/// グループの平均と標準偏差（標本 n−1）をまとめた表示文字列。
fn group_stat(
    computed: &crate::measure::ComputedMeasure,
    gid: u64,
    scale: Option<crate::frame::Scale>,
) -> Option<String> {
    let values: Vec<f64> = computed
        .tools
        .iter()
        .filter(|t| t.group == Some(gid) && t.kind == ToolKind::Distance)
        .filter_map(|t| t.length_px)
        .collect();
    if values.is_empty() {
        return None;
    }
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let avg = format_measurement(mean, scale);
    if n < 2 {
        return Some(format!("{avg} (n=1)"));
    }
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let sigma = format_measurement(var.sqrt(), scale);
    Some(format!("平均 {avg}  σ {sigma} (n={n})"))
}

// ------------------------------------------------------ オーバーレイ描画

/// 画像 px → 画面座標。
fn to_screen(info: &ViewInfo, p: Pt2) -> Pos2 {
    let rect = info.image_rect.unwrap_or(Rect::NOTHING);
    rect.min + Vec2::new(p.x as f32, p.y as f32) * info.zoom
}

/// 画面座標 → 画像 px（画像の外なら None）。
fn to_image(info: &ViewInfo, pos: Pos2) -> Option<Pt2> {
    let rect = info.image_rect?;
    let p = (pos - rect.min) / info.zoom;
    Some(Pt2::new(p.x as f64, p.y as f64))
}

/// 確定済みの測長コマンド（または測長モードの編集中データ）を画像に重ねて描く。
pub fn draw_computed(
    painter: &Painter,
    info: &ViewInfo,
    computed: &ComputedMeasure,
    scale: Option<Scale>,
) {
    if info.image_rect.is_none() {
        return;
    }
    for t in &computed.tools {
        // フィッティング領域の枠（点線）。色はフィッティング設定で変え、
        // 符号固定モードは辺ごとに明暗を付けて方向を示す。
        for region in &t.fit_regions {
            let pts: Vec<Pos2> = region.corners().iter().map(|&c| to_screen(info, c)).collect();
            let colors = region_edge_colors(region.mode, region.sign);
            for i in 0..4 {
                painter.add(egui::Shape::dashed_line(
                    &[pts[i], pts[(i + 1) % 4]],
                    Stroke::new(2.5, colors[i]),
                    6.0,
                    5.0,
                ));
            }
        }
        match t.kind {
            ToolKind::Distance => {
                let a = to_screen(info, t.p1);
                let b = to_screen(info, t.p2);
                draw_line_and_arrows(painter, a, b, COLOR_DISTANCE);
                if let Some(len) = t.length_px {
                    draw_value_label(painter, a, b, format_measurement(len, scale), COLOR_DISTANCE);
                }
            }
            ToolKind::Boundary => {
                // 長さは結果リストに出るので、画像中には描かない。
                let a = to_screen(info, t.p1);
                let b = to_screen(info, t.p2);
                painter.line_segment([a, b], Stroke::new(2.0, COLOR_GUIDE));
            }
            ToolKind::Offset => {
                // 元の境界線と同じ長さの線分。元の境界線の中点から矢印で
                // 関係を示し、オフセット距離を画像中に表示する。
                if let Some(src) = t.source.and_then(|s| computed.by_id(s)) {
                    draw_offset_link(painter, info, (src.p1, src.p2), (t.p1, t.p2));
                }
                let a = to_screen(info, t.p1);
                let b = to_screen(info, t.p2);
                painter.line_segment([a, b], Stroke::new(2.0, COLOR_GUIDE));
                if let Some(d) = t.distance_px {
                    let sign = if d >= 0.0 { "+" } else { "-" };
                    let text = format!("{sign}{}", format_measurement(d.abs(), scale));
                    draw_value_label(painter, a, b, text, COLOR_GUIDE);
                }
            }
        }
    }
}

/// 線と両端の矢印頭。
fn draw_line_and_arrows(painter: &Painter, a: Pos2, b: Pos2, color: Color32) {
    painter.line_segment([a, b], Stroke::new(2.0, color));
    draw_arrow_head(painter, a, a - b, color);
    draw_arrow_head(painter, b, b - a, color);
}

/// 線の先端から ±25° に開いた 2 本の短線で矢印頭を描く。
fn draw_arrow_head(painter: &Painter, tip: Pos2, dir: Vec2, color: Color32) {
    let len = dir.length();
    if len < 1.0 {
        return;
    }
    let u = dir / len;
    let rotate = |v: Vec2, angle: f32| {
        let (sin, cos) = angle.sin_cos();
        Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
    };
    let stroke = Stroke::new(2.0, color);
    painter.line_segment([tip, tip - rotate(u, 25f32.to_radians()) * 9.0], stroke);
    painter.line_segment([tip, tip - rotate(u, -25f32.to_radians()) * 9.0], stroke);
}

/// オフセット線と元の境界線の関係を示す矢印（元の中点 → オフセット線の中点）。
fn draw_offset_link(painter: &Painter, info: &ViewInfo, src: (Pt2, Pt2), off: (Pt2, Pt2)) {
    let sm = (src.0 + src.1) * 0.5;
    let om = (off.0 + off.1) * 0.5;
    let (a, b) = (to_screen(info, sm), to_screen(info, om));
    // 距離 0 で線が重なっているときは描かない。
    if (a - b).length() < 2.0 {
        return;
    }
    painter.line_segment([a, b], Stroke::new(1.0, COLOR_GUIDE));
    draw_arrow_head(painter, b, a - b, COLOR_GUIDE);
}

/// 線の中点から少し浮かせて測定値を描く。
fn draw_value_label(painter: &Painter, a: Pos2, b: Pos2, text: String, color: Color32) {
    let mid = a.to_vec2() + (b - a) * 0.5;
    let d = b - a;
    let n = if d.length() > 1.0 { d.rot90() / d.length() } else { Vec2::Y };
    painter.text(
        egui::pos2(mid.x + n.x * 8.0, mid.y + n.y * 8.0),
        Align2::CENTER_BOTTOM,
        text,
        FontId::proportional(12.0),
        color,
    );
}

/// 凸四角形（領域枠）の中に点があるか。
fn point_in_quad(p: Pt2, q: [Pt2; 4]) -> bool {
    let mut pos = 0;
    let mut neg = 0;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross > 0.0 {
            pos += 1;
        } else if cross < 0.0 {
            neg += 1;
        }
    }
    pos == 4 || neg == 4
}

/// 動かす端点の反対側の端点（4 方向固定の基準点）。
fn other_endpoint(data: &MeasureData, id: u64, which: EndpointWhich) -> Option<Pt2> {
    for t in &data.tools {
        match (t, which) {
            (MeasureTool::Distance { id: i, p1, p2, .. }, EndpointWhich::P1) if *i == id => {
                return Some(*p2);
            }
            (MeasureTool::Distance { id: i, p1, p2, .. }, EndpointWhich::P2) if *i == id => {
                return Some(*p1);
            }
            (MeasureTool::Boundary { id: i, p1, p2, .. }, EndpointWhich::P1) if *i == id => {
                return Some(*p2);
            }
            (MeasureTool::Boundary { id: i, p1, p2, .. }, EndpointWhich::P2) if *i == id => {
                return Some(*p1);
            }
            _ => {}
        }
    }
    None
}

/// ツールの端点を書き換える（ドラッグ移動用）。
fn set_endpoint(data: &mut MeasureData, id: u64, which: EndpointWhich, p: Pt2) {
    for t in &mut data.tools {
        match (t, which) {
            (MeasureTool::Distance { id: i, p1, .. }, EndpointWhich::P1) if *i == id => {
                *p1 = p;
            }
            (MeasureTool::Distance { id: i, p2, .. }, EndpointWhich::P2) if *i == id => {
                *p2 = p;
            }
            (MeasureTool::Boundary { id: i, p1, .. }, EndpointWhich::P1) if *i == id => {
                *p1 = p;
            }
            (MeasureTool::Boundary { id: i, p2, .. }, EndpointWhich::P2) if *i == id => {
                *p2 = p;
            }
            _ => {}
        }
    }
}

/// リサイズ前の矩形から新しい矩形への線形写像（反対側の辺が基準になる）。
/// 元の矩形の幅が 0 の軸は平行移動だけにする。
fn resize_map(omin: Pt2, omax: Pt2, nmin: Pt2, nmax: Pt2, p: Pt2) -> Pt2 {
    let sx = if (omax.x - omin.x).abs() > 1e-9 {
        (nmax.x - nmin.x) / (omax.x - omin.x)
    } else {
        1.0
    };
    let sy = if (omax.y - omin.y).abs() > 1e-9 {
        (nmax.y - nmin.y) / (omax.y - omin.y)
    } else {
        1.0
    };
    Pt2::new(nmin.x + (p.x - omin.x) * sx, nmin.y + (p.y - omin.y) * sy)
}

/// 線分（a→b）までの距離。
fn distance_to_segment(pos: Pt2, a: Pt2, b: Pt2) -> f64 {
    let ab = b - a;
    let len2 = ab.x * ab.x + ab.y * ab.y;
    let t = if len2 > 0.0 {
        (((pos - a).x * ab.x + (pos - a).y * ab.y) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (pos - (a + ab * t)).length()
}

impl MeasureMode {
    /// 測長モード中の編集セッションを描画する（確定済みツール + 作成中ツール）。
    pub fn draw_session(&mut self, painter: &Painter, info: &ViewInfo, doc: &Document) {
        let Some(frame) = doc.input_to(self.index) else {
            return;
        };
        let (img, scale) = (frame.image.clone(), frame.scale);
        // ドラッグ中: 選択ツールだけドラッグ位置（生の p1/p2）で描き、
        // 他のツールはドラッグ開始時のフィッティング済み位置で描く。
        // 全ツールを生の位置で描くと、通常のフィッティング後の線と
        // 重なって二重に見えるため（選択ツールだけはそれが許容される）。
        let computed = if let Some(fitted) = &self.drag_fitted {
            let raw = self.data.compute_without_fit(&img, scale);
            let mut tools = fitted.tools.clone();
            // 選択ツール（範囲選択なら選択中の測長すべて）と、それを参照する
            // オフセット線は生の位置（ドラッグ追従）。
            let raw_ids: Vec<u64> = if let Some(sel) = self.selected {
                vec![sel]
            } else if matches!(
                self.range_state,
                Some(RangeState::Moving { .. } | RangeState::Resizing { .. })
            ) {
                self.range_selection
                    .as_ref()
                    .map_or(Vec::new(), |s| s.ids.clone())
            } else {
                Vec::new()
            };
            for r in &raw.tools {
                if (raw_ids.contains(&r.id) || raw_ids.iter().any(|&id| r.source == Some(id)))
                    && let Some(t) = tools.iter_mut().find(|t| t.id == r.id)
                {
                    *t = r.clone();
                }
            }
            ComputedMeasure { tools }
        } else {
            self.data.compute(&img, scale)
        };
        draw_computed(painter, info, &computed, scale);

        // 選択中のツールを橙の太線でハイライトする。
        if let Some(sel) = self.selected
            && let Some(t) = computed.by_id(sel)
        {
            painter.line_segment(
                [to_screen(info, t.p1), to_screen(info, t.p2)],
                Stroke::new(3.0, COLOR_IN_PROGRESS),
            );
        }

        // 範囲選択: 選択中の測長のハイライトと選択枠。
        self.draw_range_overlay(painter, info, &computed);

        // ホバー時の視覚フィードバック（スナップ・選択可能・ドラッグ中）。
        self.hover_feedback(painter, info, &computed);

        let Some(prog) = &self.in_progress else {
            return;
        };
        let cursor = painter
            .ctx()
            .input(|i| i.pointer.hover_pos())
            .and_then(|p| to_image(info, p));
        match *prog {
            InProgress::Distance { p1, p1_line } => {
                let Some(cursor) = cursor else { return };
                let lines = self.data.snap_lines(&img, scale);
                let (p1, p2, snapped) = self.resolve_distance(p1, p1_line, cursor, &lines, info.zoom);
                let p2 = self.resolve_angle(p1, p2, snapped);
                let a = to_screen(info, p1);
                let b = to_screen(info, p2);
                draw_line_and_arrows(painter, a, b, COLOR_IN_PROGRESS);
                // 端点 1 が補助線にスナップ中なら紫のドットで示す
                // （端点 2 側のドットは hover_feedback が描く）。
                if p1_line.is_some() {
                    painter.circle_filled(a, 3.5, COLOR_GUIDE);
                }
            }
            InProgress::Boundary { p1 } => {
                let Some(cursor) = cursor else { return };
                let p2 = self.resolve_angle(p1, cursor, false);
                painter.line_segment(
                    [to_screen(info, p1), to_screen(info, p2)],
                    Stroke::new(2.0, COLOR_IN_PROGRESS),
                );
            }
            InProgress::OffsetPick { source, distance } => {
                if let Some(t) = computed.by_id(source) {
                    let n = (t.p2 - t.p1).normalize().perp();
                    let (pa, pb) = (t.p1 + n * distance, t.p2 + n * distance);
                    // プレビューにも元境界線との関係を示す矢印を出す。
                    draw_offset_link(painter, info, (t.p1, t.p2), (pa, pb));
                    painter.line_segment(
                        [to_screen(info, pa), to_screen(info, pb)],
                        Stroke::new(2.0, COLOR_IN_PROGRESS),
                    );
                }
            }
            InProgress::LinearDuplicate {
                src,
                start,
                current,
                count,
            } => {
                // ドラッグ距離を複製数で分割した位置にコピーをプレビュー表示する。
                if let Some(t) = computed.by_id(src) {
                    let delta = current - start;
                    for k in 1..=count {
                        let off = delta * (k as f64 / count as f64);
                        let a = to_screen(info, t.p1 + off);
                        let b = to_screen(info, t.p2 + off);
                        let color = COLOR_IN_PROGRESS.gamma_multiply(0.6);
                        match t.kind {
                            ToolKind::Distance => {
                                draw_line_and_arrows(painter, a, b, color);
                            }
                            _ => {
                                painter.line_segment([a, b], Stroke::new(1.5, color));
                            }
                        }
                    }
                }
                if let Some(c) = cursor {
                    painter.text(
                        to_screen(info, c) + Vec2::new(10.0, -8.0),
                        Align2::LEFT_BOTTOM,
                        format!("×{count}"),
                        FontId::proportional(14.0),
                        COLOR_IN_PROGRESS,
                    );
                }
            }
        }
    }

    /// 範囲選択: 選択中（または作成中）の測長を橙の太線でハイライトし、
    /// 選択枠と四隅のハンドルを描く。
    fn draw_range_overlay(&self, painter: &Painter, info: &ViewInfo, computed: &ComputedMeasure) {
        let (ids, rect): (Vec<u64>, Option<(Pt2, Pt2)>) = match &self.range_state {
            Some(RangeState::Drawing { start, current }) => {
                (self.distance_ids_in(*start, *current), Some((*start, *current)))
            }
            _ => (
                self.range_selection
                    .as_ref()
                    .map_or(Vec::new(), |s| s.ids.clone()),
                self.range_selection.as_ref().map(|s| (s.min, s.max)),
            ),
        };
        for id in &ids {
            if let Some(t) = computed.by_id(*id) {
                painter.line_segment(
                    [to_screen(info, t.p1), to_screen(info, t.p2)],
                    Stroke::new(4.0, COLOR_IN_PROGRESS.gamma_multiply(0.65)),
                );
            }
        }
        if let Some((min, max)) = rect {
            let rect = Rect::from_two_pos(to_screen(info, min), to_screen(info, max));
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(1.5, COLOR_IN_PROGRESS),
                egui::StrokeKind::Inside,
            );
            // リサイズできることを示す四隅のハンドル。
            for corner in [
                rect.left_top(),
                rect.right_top(),
                rect.left_bottom(),
                rect.right_bottom(),
            ] {
                painter.rect_filled(
                    Rect::from_center_size(corner, Vec2::splat(5.0)),
                    0.0,
                    COLOR_IN_PROGRESS,
                );
            }
        }
    }

    /// 中心位置（p1/p2 の中点）が枠内の二点間測長の ID。枠は作成中の
    /// 生の矩形でも、確定済みの min/max でもよい。
    fn distance_ids_in(&self, a: Pt2, b: Pt2) -> Vec<u64> {
        let (min_x, max_x) = (a.x.min(b.x), a.x.max(b.x));
        let (min_y, max_y) = (a.y.min(b.y), a.y.max(b.y));
        self.data
            .tools
            .iter()
            .filter_map(|t| match t {
                MeasureTool::Distance { id, p1, p2, .. } => {
                    let c = (*p1 + *p2) * 0.5;
                    (c.x >= min_x && c.x <= max_x && c.y >= min_y && c.y <= max_y)
                        .then_some(*id)
                }
                _ => None,
            })
            .collect()
    }

    /// 選択中測長のドラッグ開始時の p1/p2 スナップショット。
    fn range_orig(&self) -> Vec<(u64, Pt2, Pt2)> {
        let Some(sel) = &self.range_selection else {
            return Vec::new();
        };
        self.data
            .tools
            .iter()
            .filter_map(|t| match t {
                MeasureTool::Distance { id, p1, p2, .. } if sel.ids.contains(id) => {
                    Some((*id, *p1, *p2))
                }
                _ => None,
            })
            .collect()
    }

    /// 枠への当たり判定。辺（±t）→ 変形、内側 → 移動、それ以外 → なし。
    fn range_hit(sel: &RangeSelection, pos: Pt2, t: f64) -> RangeHit {
        let min_x = (pos.x - sel.min.x).abs() <= t
            && pos.y >= sel.min.y - t
            && pos.y <= sel.max.y + t;
        let max_x = (pos.x - sel.max.x).abs() <= t
            && pos.y >= sel.min.y - t
            && pos.y <= sel.max.y + t;
        let min_y = (pos.y - sel.min.y).abs() <= t
            && pos.x >= sel.min.x - t
            && pos.x <= sel.max.x + t;
        let max_y = (pos.y - sel.max.y).abs() <= t
            && pos.x >= sel.min.x - t
            && pos.x <= sel.max.x + t;
        if min_x || max_x || min_y || max_y {
            RangeHit::Resize {
                min_x,
                max_x,
                min_y,
                max_y,
            }
        } else if pos.x > sel.min.x && pos.x < sel.max.x && pos.y > sel.min.y && pos.y < sel.max.y
        {
            RangeHit::Move
        } else {
            RangeHit::None
        }
    }

    /// ホバー時の視覚フィードバック。スナップが効く位置ではカーソル位置に
    /// 紫のリングとスナップ先のドットを描き、選択可能なツールでは
    /// ハイライトとカーソルアイコンの変更を行う。
    fn hover_feedback(&self, painter: &Painter, info: &ViewInfo, computed: &ComputedMeasure) {
        let Some(pos) = painter
            .ctx()
            .input(|i| i.pointer.hover_pos())
            .and_then(|p| to_image(info, p))
        else {
            return;
        };
        let zoom = info.zoom;
        let set_cursor = |icon| painter.ctx().output_mut(|o| o.cursor_icon = icon);

        // ---- スナップ（設定 on・二点間測長の作成中と二点間測長の端点
        //      ドラッグ中のみ）----
        let snapping = self.data.prefs.snap
            && (self.tool == Some(ToolButton::Distance)
                || matches!(
                    self.drag,
                    Some(Drag::Endpoint { id, .. })
                        if matches!(self.data.tool_by_id(id), Some(MeasureTool::Distance { .. }))
                ));
        if snapping {
            // スナップ対象は境界線・オフセット線（線分ハイライト用に端点も持つ）。
            let lines: Vec<(SnapLine, Pt2, Pt2)> = computed
                .tools
                .iter()
                .filter(|t| matches!(t.kind, ToolKind::Boundary | ToolKind::Offset))
                .map(|t| (SnapLine::from_points(t.p1, t.p2), t.p1, t.p2))
                .collect();
            let threshold = SNAP_PX as f64 / zoom as f64;
            let nearest = lines
                .iter()
                .filter(|(l, _, _)| l.distance_to(pos) <= threshold)
                .min_by(|(a, _, _), (b, _, _)| {
                    a.distance_to(pos)
                        .partial_cmp(&b.distance_to(pos))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some((line, a, b)) = nearest {
                // カーソル位置の紫リング + 対象直線のハイライト。
                painter.circle_stroke(to_screen(info, pos), 9.0, Stroke::new(2.0, COLOR_GUIDE));
                painter.line_segment(
                    [to_screen(info, *a), to_screen(info, *b)],
                    Stroke::new(5.0, COLOR_GUIDE.gamma_multiply(0.3)),
                );
                // スナップ先のドットはクリック時と同じ計算で実際の着地点を示す。
                match self.in_progress {
                    // 端点 2 を置く前・端点ドラッグ中: カーソルの射影が着地点。
                    None => {
                        painter.circle_filled(to_screen(info, line.project(pos)), 3.5, COLOR_GUIDE);
                    }
                    // 端点 2 を置く: snap_distance と同じ計算（垂線の足のケースを含む）。
                    Some(InProgress::Distance { p1, p1_line }) => {
                        let raw: Vec<SnapLine> = lines.iter().map(|(l, _, _)| *l).collect();
                        let (pa, pb) = snap_distance(p1, p1_line.as_ref(), pos, &raw, threshold);
                        if p1_line.is_some() {
                            painter.circle_filled(to_screen(info, pa), 3.5, COLOR_GUIDE);
                        }
                        painter.circle_filled(to_screen(info, pb), 3.5, COLOR_GUIDE);
                    }
                    _ => {
                        painter.circle_filled(to_screen(info, line.project(pos)), 3.5, COLOR_GUIDE);
                    }
                }
            }
        }

        // ---- ドラッグ中のカーソルと端点リング ----
        match self.drag {
            Some(Drag::Endpoint { id, which }) => {
                if let Some(t) = computed.by_id(id) {
                    let p = match which {
                        EndpointWhich::P1 => t.p1,
                        EndpointWhich::P2 => t.p2,
                    };
                    painter.circle_stroke(
                        to_screen(info, p),
                        7.0,
                        Stroke::new(2.0, COLOR_IN_PROGRESS),
                    );
                }
                set_cursor(CursorIcon::Grabbing);
                return;
            }
            Some(Drag::Whole { id, .. }) => {
                if let Some(t) = computed.by_id(id) {
                    for p in [t.p1, t.p2] {
                        painter.circle_stroke(
                            to_screen(info, p),
                            7.0,
                            Stroke::new(2.0, COLOR_IN_PROGRESS),
                        );
                    }
                }
                set_cursor(CursorIcon::Move);
                return;
            }
            None => {}
        }

        // ---- 範囲選択の枠: 内側で移動、辺・角でリサイズのカーソル ----
        if self.tool == Some(ToolButton::RangeSelect)
            && self.range_state.is_none()
            && let Some(sel) = &self.range_selection
        {
            let t = RANGE_EDGE_PX as f64 / zoom as f64;
            match Self::range_hit(sel, pos, t) {
                RangeHit::Move => {
                    set_cursor(CursorIcon::Move);
                    return;
                }
                RangeHit::Resize {
                    min_x,
                    max_x,
                    min_y,
                    max_y,
                } => {
                    let corner = (min_x || max_x) && (min_y || max_y);
                    if corner && ((min_x && min_y) || (max_x && max_y)) {
                        set_cursor(CursorIcon::ResizeNwSe);
                    } else if corner {
                        set_cursor(CursorIcon::ResizeNeSw);
                    } else if min_x || max_x {
                        set_cursor(CursorIcon::ResizeHorizontal);
                    } else {
                        set_cursor(CursorIcon::ResizeVertical);
                    }
                    return;
                }
                RangeHit::None => {}
            }
        }

        // ---- ホバーで選択可能な対象（選択モード・オフセット線の元選択・
        //      直線複製の元選択）----
        let picking = match self.tool {
            None => true,
            Some(ToolButton::Offset | ToolButton::LinearDuplicate) => self.in_progress.is_none(),
            Some(ToolButton::Distance | ToolButton::Boundary | ToolButton::RangeSelect) => false,
        };
        if !picking {
            return;
        }
        let pred = |t: &ComputedTool| match self.tool {
            Some(ToolButton::Offset) => t.kind == ToolKind::Boundary,
            _ => matches!(t.kind, ToolKind::Distance | ToolKind::Boundary),
        };
        let threshold = PICK_PX as f64 / zoom as f64;
        // 端点優先（クリック時の判定と同じ順序）。
        let mut best: Option<Pt2> = None;
        for t in computed.tools.iter().filter(|t| pred(t)) {
            for p in [t.p1, t.p2] {
                let d = (pos - p).length();
                if d <= threshold && best.is_none_or(|bp| d < (pos - bp).length()) {
                    best = Some(p);
                }
            }
        }
        if let Some(p) = best {
            painter.circle_stroke(to_screen(info, p), 7.0, Stroke::new(2.0, COLOR_IN_PROGRESS));
            set_cursor(CursorIcon::PointingHand);
            return;
        }
        if let Some(t) = nearest_tool(computed, pos, threshold, pred) {
            painter.line_segment(
                [to_screen(info, t.p1), to_screen(info, t.p2)],
                Stroke::new(4.0, COLOR_IN_PROGRESS.gamma_multiply(0.45)),
            );
            set_cursor(CursorIcon::Grab);
        }
    }

    /// 測長モード中の画像上の入力処理（パン・ズーム・ツール操作・選択移動）。
    /// 戻り値はステータスバー用のホバー情報。
    pub fn handle_overlay_input(
        &mut self,
        ui: &mut Ui,
        info: &ViewInfo,
        doc: &mut Document,
    ) -> ViewInfo {
        let mut hover = ViewInfo {
            vp: info.vp,
            image_rect: info.image_rect,
            zoom: info.zoom,
            ..Default::default()
        };
        // 左ドラッグは常にこちらで処理する（選択モードでは直接ドラッグで
        // 移動）。パンは右ドラッグ。view 側の入力処理は測長モード中は
        // 無効にしている（app 側で interactive = false）。
        let resp = ui.interact(
            info.vp,
            egui::Id::new("measure_overlay"),
            Sense::click_and_drag(),
        );

        // パン（右ドラッグのみ。左ドラッグはツール操作に使う）。
        if resp.dragged_by(egui::PointerButton::Secondary) {
            doc.view.pan_by(resp.drag_delta());
        }

        // 直線複製中はホイールを複製数の調整だけに使い、ズームは止める。
        // smooth_scroll_delta は 1 ノッチが複数フレームに分散するため、
        // 累積して 20.0 たまるごとに 1 ずつ増減させる（速すぎないように）。
        const SCROLL_STEP: f32 = 20.0;
        let linear_duplicate_active =
            matches!(self.in_progress, Some(InProgress::LinearDuplicate { .. }));
        if linear_duplicate_active {
            if let Some(InProgress::LinearDuplicate { count, .. }) = &mut self.in_progress {
                let scroll = resp.ctx.input(|i| i.smooth_scroll_delta.y);
                self.scroll_accum += scroll;
                while self.scroll_accum >= SCROLL_STEP {
                    *count = (*count + 1).min(20);
                    self.scroll_accum -= SCROLL_STEP;
                }
                while self.scroll_accum <= -SCROLL_STEP {
                    *count = (*count).saturating_sub(1).max(1);
                    self.scroll_accum += SCROLL_STEP;
                }
            }
        } else if resp.hovered() {
            let (scroll_y, pinch) = resp
                .ctx
                .input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let mut factor = pinch;
            if scroll_y != 0.0 {
                factor *= (scroll_y * 0.0022).exp();
            }
            if factor != 1.0 {
                let anchor =
                    resp.hover_pos().map_or(info.vp.center(), |p| p).to_vec2() - info.vp.min.to_vec2();
                doc.view.zoom_about(anchor, factor);
            }
        }

        // ステータスバー用のホバー画素値。
        let img = doc.input_to(self.index).map(|f| f.image.clone());
        if let (Some(img), Some(pos)) = (&img, resp.hover_pos())
            && let Some(rel) = to_image(info, pos)
            && rel.x >= 0.0
            && rel.y >= 0.0
            && (rel.x as u32) < img.width
            && (rel.y as u32) < img.height
        {
            hover.hover_px = Some((rel.x as u32, rel.y as u32));
            hover.hover_value = Some(img.at(rel.x as u32, rel.y as u32));
        }

        let cursor = resp.hover_pos().and_then(|p| to_image(info, p));

        // 選択モード: 押したまま動かすだけでドラッグ移動を始める
        // （事前のクリックで選択は不要）。
        if self.tool.is_none()
            && self.in_progress.is_none()
            && resp.drag_started()
            && let (Some(img), Some(cursor)) = (&img, cursor)
        {
            let scale = doc.input_to(self.index).and_then(|f| f.scale);
            let computed = self.data.compute(img, scale);
            let threshold = PICK_PX as f64 / info.zoom as f64;
            self.select_at(&computed, cursor, threshold);
        }
        // 選択ツールのドラッグ移動（端点 or 全体）。
        if let Some(drag) = self.drag
            && let Some(cursor) = cursor
        {
            if resp.drag_started() {
                self.begin_change();
            }
            if resp.dragged() || resp.drag_started() {
                self.apply_drag(doc, &drag, cursor, &img, info.zoom);
            }
            if resp.drag_stopped() {
                self.drag = None;
                self.drag_fitted = None;
                self.selected = None;
            }
        }

        // 範囲選択ツール: 枠の作成・一括移動・リサイズ。
        if self.tool == Some(ToolButton::RangeSelect) {
            // 当たり判定は「押した位置」で行う（drag_started の時点では
            // ポインタがドラッグしきい値ぶん動いており、角のハンドルから
            // 外れて新規作成と誤判定されるため）。
            let press = resp.interact_pointer_pos().and_then(|p| to_image(info, p));
            self.handle_range_input(doc, &resp, cursor, press, &img, info.zoom);
        }

        // 直線複製: カーソル追従（4 方向固定を適用）。確定はクリック側。
        if let Some(InProgress::LinearDuplicate { start, current, .. }) = &mut self.in_progress
            && let Some(cursor) = cursor
        {
            *current = if self.data.prefs.angle == AngleMode::FourDir {
                snap_angle_four(*start, cursor)
            } else {
                cursor
            };
        }

        // オフセット線選択中の距離追従。
        if let Some(cursor) = cursor
            && let Some(img) = &img
            && let Some(InProgress::OffsetPick { source, distance }) = &mut self.in_progress
        {
            let scale = doc.input_to(self.index).and_then(|f| f.scale);
            let computed = self.data.compute(img, scale);
            if let Some(t) = computed.by_id(*source) {
                let n = (t.p2 - t.p1).normalize().perp();
                let v = cursor - t.p1;
                *distance = n.x * v.x + n.y * v.y;
            }
        }

        // Esc: 作成中ツール → 選択 → ツールボタンの順に解除する。
        if resp.ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_escape();
        }

        let click_pos = resp.interact_pointer_pos().and_then(|p| to_image(info, p));
        // 右クリックは作成中ツール・選択のキャンセルに使う。
        if resp.secondary_clicked() {
            self.cancel_escape();
        }
        // ダブルクリック: フィッティング領域の上なら設定ポップアップ。
        // ツール選択中はダブルクリックがツール操作（2 回クリック）と
        // 重なるので、ツール非選択時のみ開く。
        if self.tool.is_none()
            && resp.double_clicked()
            && let (Some(img), Some(pos)) = (&img, click_pos)
        {
            let scale = doc.input_to(self.index).and_then(|f| f.scale);
            let computed = self.data.compute(img, scale);
            if let Some(target) = self.fit_target_at(&computed, pos) {
                self.popup = Some(target);
            }
        }

        // 左クリックでツール操作（または選択モードでの選択）。
        if resp.clicked()
            && let (Some(img), Some(pos)) = (&img, click_pos)
        {
            self.on_click(doc, img, pos, info.zoom);
        }

        hover
    }

    /// Esc の解除順序: 作成中ツール → 選択 → 範囲選択 → ツールボタン。
    fn cancel_escape(&mut self) {
        if self.in_progress.is_some() {
            self.in_progress = None;
        } else if self.selected.is_some() || self.drag.is_some() {
            self.selected = None;
            self.drag = None;
            self.drag_fitted = None;
        } else if self.range_state.is_some() || self.range_selection.is_some() {
            self.range_state = None;
            self.range_selection = None;
        } else {
            self.tool = None;
        }
    }

    /// ドラッグ移動をデータへ反映する（ライブ反映のみ、undo 記録は
    /// `drag_started` のときに 1 回だけ行う）。
    fn apply_drag(
        &mut self,
        doc: &mut Document,
        drag: &Drag,
        cursor: Pt2,
        img: &Option<std::sync::Arc<Gray16>>,
        zoom: f32,
    ) {
        let Some(img) = img else { return };
        let scale = doc.input_to(self.index).and_then(|f| f.scale);
        match *drag {
            Drag::Endpoint { id, which } => {
                // スナップが効かなければ 4 方向固定を適用する
                // （もう一方の端点を基準に方向を丸める）。
                let (mut p, snapped) = self.resolve_endpoint_move(id, cursor, img, scale, zoom);
                if !snapped && self.data.prefs.angle == AngleMode::FourDir
                    && let Some(other) = other_endpoint(&self.data, id, which)
                {
                    p = snap_angle_four(other, p);
                }
                set_endpoint(&mut self.data, id, which, p);
                self.apply_change(doc);
            }
            Drag::Whole {
                id,
                start,
                orig_p1,
                orig_p2,
            } => {
                // 全体移動は 4 方向固定の対象外（平行移動のまま）。
                let d = cursor - start;
                set_endpoint(&mut self.data, id, EndpointWhich::P1, orig_p1 + d);
                set_endpoint(&mut self.data, id, EndpointWhich::P2, orig_p2 + d);
                self.apply_change(doc);
            }
        }
    }

    /// 範囲選択ツールの画像上入力。ドラッグ開始位置（押した位置）で
    /// 「枠の作成・一括移動・拡大縮小」を決め、ドラッグ中は状態を更新する。
    /// 移動・変形中の測長は生の位置で描き、フィッティングの再計算は
    /// ドラッグ終了後の通常描画に任せる。
    fn handle_range_input(
        &mut self,
        doc: &mut Document,
        resp: &egui::Response,
        cursor: Option<Pt2>,
        press: Option<Pt2>,
        img: &Option<std::sync::Arc<Gray16>>,
        zoom: f32,
    ) {
        let edge = RANGE_EDGE_PX as f64 / zoom as f64;

        if resp.drag_started()
            && let Some(press) = press
        {
            match &self.range_selection {
                Some(sel) => match Self::range_hit(sel, press, edge) {
                    RangeHit::Resize {
                        min_x,
                        max_x,
                        min_y,
                        max_y,
                    } => {
                        // ドラッグ中は生の位置で描く（フィッティング再計算を止める）。
                        if let Some(img) = img {
                            let scale = doc.input_to(self.index).and_then(|f| f.scale);
                            self.drag_fitted = Some(self.data.compute(img, scale));
                        }
                        self.range_state = Some(RangeState::Resizing {
                            orig_min: sel.min,
                            orig_max: sel.max,
                            min_x,
                            max_x,
                            min_y,
                            max_y,
                            orig: self.range_orig(),
                        });
                        self.begin_change();
                    }
                    RangeHit::Move => {
                        // ドラッグ中は生の位置で描く（フィッティング再計算を止める）。
                        if let Some(img) = img {
                            let scale = doc.input_to(self.index).and_then(|f| f.scale);
                            self.drag_fitted = Some(self.data.compute(img, scale));
                        }
                        self.range_state = Some(RangeState::Moving {
                            start: press,
                            orig_min: sel.min,
                            orig_max: sel.max,
                            orig: self.range_orig(),
                        });
                        self.begin_change();
                    }
                    RangeHit::None => {
                        // 枠の外: 新しい枠の作成を始める。
                        self.range_state =
                            Some(RangeState::Drawing { start: press, current: press });
                    }
                },
                None => {
                    self.range_state = Some(RangeState::Drawing { start: press, current: press });
                }
            }
        }

        if resp.dragged()
            && let Some(cursor) = cursor
        {
            match self.range_state.clone() {
                Some(RangeState::Drawing { start, .. }) => {
                    self.range_state = Some(RangeState::Drawing { start, current: cursor });
                }
                Some(RangeState::Moving {
                    start,
                    orig_min,
                    orig_max,
                    orig,
                }) => {
                    let d = cursor - start;
                    for (id, p1, p2) in &orig {
                        set_endpoint(&mut self.data, *id, EndpointWhich::P1, *p1 + d);
                        set_endpoint(&mut self.data, *id, EndpointWhich::P2, *p2 + d);
                    }
                    self.range_selection = Some(RangeSelection {
                        ids: orig.iter().map(|(id, _, _)| *id).collect(),
                        min: orig_min + d,
                        max: orig_max + d,
                    });
                    self.apply_change(doc);
                }
                Some(RangeState::Resizing {
                    orig_min,
                    orig_max,
                    min_x,
                    max_x,
                    min_y,
                    max_y,
                    orig,
                }) => {
                    // 追従する辺だけを動かし、測長の p1/p2 も枠と同じ線形写像で
                    // 動かす（反対側の辺が基準になる）。枠は 1 px 未満に
                    // つぶれないよう制限する。
                    let (mut nmin, mut nmax) = (orig_min, orig_max);
                    if min_x {
                        nmin.x = cursor.x.min(orig_max.x - 1.0);
                    }
                    if max_x {
                        nmax.x = cursor.x.max(orig_min.x + 1.0);
                    }
                    if min_y {
                        nmin.y = cursor.y.min(orig_max.y - 1.0);
                    }
                    if max_y {
                        nmax.y = cursor.y.max(orig_min.y + 1.0);
                    }
                    for (id, p1, p2) in &orig {
                        set_endpoint(
                            &mut self.data,
                            *id,
                            EndpointWhich::P1,
                            resize_map(orig_min, orig_max, nmin, nmax, *p1),
                        );
                        set_endpoint(
                            &mut self.data,
                            *id,
                            EndpointWhich::P2,
                            resize_map(orig_min, orig_max, nmin, nmax, *p2),
                        );
                    }
                    self.range_selection = Some(RangeSelection {
                        ids: orig.iter().map(|(id, _, _)| *id).collect(),
                        min: nmin,
                        max: nmax,
                    });
                    self.apply_change(doc);
                }
                None => {}
            }
        }

        if resp.drag_stopped() {
            match self.range_state.take() {
                Some(RangeState::Drawing { start, current }) => {
                    // 中心位置が枠内の測長を選ぶ。空なら選択なし
                    // （極小のドラッグも実質クリックで、空になる）。
                    let ids = self.distance_ids_in(start, current);
                    self.range_selection = if ids.is_empty() {
                        None
                    } else {
                        Some(RangeSelection {
                            ids,
                            min: Pt2::new(start.x.min(current.x), start.y.min(current.y)),
                            max: Pt2::new(start.x.max(current.x), start.y.max(current.y)),
                        })
                    };
                }
                // フィッティングの再計算はこの後の通常描画（drag_fitted = None）で行う。
                Some(RangeState::Moving { .. }) | Some(RangeState::Resizing { .. }) => {
                    self.drag_fitted = None;
                }
                None => {}
            }
        }

        // クリック（ドラッグなし）: 選択解除。
        if resp.clicked() {
            self.range_selection = None;
            self.range_state = None;
        }
    }

    /// 端点移動時のスナップ（スナップ on の二点間測長のみ）。
    /// 戻り値の bool はスナップが効いたかどうか。
    fn resolve_endpoint_move(
        &self,
        id: u64,
        cursor: Pt2,
        img: &Gray16,
        scale: Option<Scale>,
        zoom: f32,
    ) -> (Pt2, bool) {
        if !self.data.prefs.snap
            || !matches!(self.data.tool_by_id(id), Some(MeasureTool::Distance { .. }))
        {
            return (cursor, false);
        }
        let lines = self.data.snap_lines(img, scale);
        let threshold = SNAP_PX as f64 / zoom as f64;
        let nearest = lines
            .iter()
            .filter(|l| l.distance_to(cursor) <= threshold)
            .min_by(|a, b| {
                a.distance_to(cursor)
                    .partial_cmp(&b.distance_to(cursor))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        match nearest {
            Some(line) => (line.project(cursor), true),
            None => (cursor, false),
        }
    }

    /// クリック位置にあるフィッティング領域の設定対象を返す。
    fn fit_target_at(&self, computed: &ComputedMeasure, pos: Pt2) -> Option<FitPopupTarget> {
        for t in &computed.tools {
            for region in &t.fit_regions {
                if point_in_quad(pos, region.corners()) {
                    return Some(match t.kind {
                        ToolKind::Boundary => FitPopupTarget::Boundary { tool: t.id },
                        ToolKind::Distance => {
                            if (region.center - t.p1).length()
                                < (region.center - t.p2).length()
                            {
                                FitPopupTarget::Dist1 { tool: t.id }
                            } else {
                                FitPopupTarget::Dist2 { tool: t.id }
                            }
                        }
                        _ => continue,
                    });
                }
            }
        }
        None
    }

    fn on_click(&mut self, doc: &mut Document, img: &Gray16, pos: Pt2, zoom: f32) {
        let scale = doc.input_to(self.index).and_then(|f| f.scale);
        let computed = self.data.compute(img, scale);
        let snap_lines = if self.data.prefs.snap {
            self.data.snap_lines(img, scale)
        } else {
            Vec::new()
        };
        // スナップは狭く、ツール選択の判定は広めに取る。
        let snap_threshold = SNAP_PX as f64 / zoom as f64;
        let pick_threshold = PICK_PX as f64 / zoom as f64;

        match self.tool {
            Some(ToolButton::Distance) => match self.in_progress.take() {
                None => {
                    // 端点 1 を置く。近くの補助線があればスナップして記録する。
                    let p1_line = snap_lines
                        .iter()
                        .filter(|l| l.distance_to(pos) <= snap_threshold)
                        .min_by(|a, b| {
                            a.distance_to(pos)
                                .partial_cmp(&b.distance_to(pos))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .copied();
                    let p1 = p1_line.map_or(pos, |l| l.project(pos));
                    self.in_progress = Some(InProgress::Distance { p1, p1_line });
                }
                Some(InProgress::Distance { p1, p1_line }) => {
                    // 端点 2 を置いて確定。
                    let (p1, p2, snapped) = self.resolve_distance(p1, p1_line, pos, &snap_lines, zoom);
                    let p2 = self.resolve_angle(p1, p2, snapped);
                    let (mut fit1, mut fit2) = (self.data.dist_fit1, self.data.dist_fit2);
                    // 片側だけスナップした端点はフィッティングせずクリック位置
                    // そのまま（補助線近傍の輝度でフィットが端点をずらすのを防ぐ）。
                    let p1_snapped = p1_line.is_some();
                    let p2_snapped = snap_lines
                        .iter()
                        .any(|l| l.distance_to(pos) <= snap_threshold);
                    if p1_snapped && !p2_snapped {
                        fit1.mode = FitMode::Off;
                    } else if p2_snapped && !p1_snapped {
                        fit2.mode = FitMode::Off;
                    }
                    self.mutate(doc, |data| {
                        let id = data.next_id();
                        let group = data.group_for_new_measurement();
                        data.tools.push(MeasureTool::Distance {
                            id,
                            p1,
                            p2,
                            group,
                            fit1,
                            fit2,
                        });
                        data.apply_new_measure_mode();
                    });
                }
                _ => self.in_progress = None,
            },
            Some(ToolButton::Boundary) => match self.in_progress.take() {
                None => self.in_progress = Some(InProgress::Boundary { p1: pos }),
                Some(InProgress::Boundary { p1 }) => {
                    let p2 = self.resolve_angle(p1, pos, false);
                    let fit = self.data.boundary_fit;
                    self.mutate(doc, |data| {
                        let id = data.next_id();
                        // 補助線は測定結果に出ないのでグループを作らず、既存の
                        // アクティブグループも変えない（group は使われないので 0）。
                        data.tools.push(MeasureTool::Boundary {
                            id,
                            p1,
                            p2,
                            group: 0,
                            fit,
                        });
                    });
                }
                _ => self.in_progress = None,
            },
            Some(ToolButton::Offset) => match self.in_progress.take() {
                None => {
                    // 境界線を選択。
                    if let Some(t) = nearest_tool(&computed, pos, pick_threshold, |t| {
                        t.kind == ToolKind::Boundary
                    }) {
                        let n = (t.p2 - t.p1).normalize().perp();
                        let v = pos - t.p1;
                        let distance = n.x * v.x + n.y * v.y;
                        self.in_progress = Some(InProgress::OffsetPick {
                            source: t.id,
                            distance,
                        });
                    }
                }
                Some(InProgress::OffsetPick { source, distance }) => {
                    // クリックで距離を確定。
                    self.mutate(doc, |data| {
                        let id = data.next_id();
                        data.tools.push(MeasureTool::Offset {
                            id,
                            source,
                            distance,
                        });
                    });
                }
                _ => self.in_progress = None,
            },
            Some(ToolButton::LinearDuplicate) => match self.in_progress.take() {
                None => {
                    // 複製元の測長・境界線を選ぶ。
                    if let Some(t) = nearest_tool(&computed, pos, pick_threshold, |t| {
                        matches!(t.kind, ToolKind::Distance | ToolKind::Boundary)
                    }) {
                        self.selected = Some(t.id);
                        self.scroll_accum = 0.0;
                        self.in_progress = Some(InProgress::LinearDuplicate {
                            src: t.id,
                            start: pos,
                            current: pos,
                            count: 1,
                        });
                    }
                }
                Some(InProgress::LinearDuplicate {
                    src,
                    start,
                    count,
                    ..
                }) => {
                    // クリックで確定。方向は 4 方向固定を適用した位置を使う。
                    let current = if self.data.prefs.angle == AngleMode::FourDir {
                        snap_angle_four(start, pos)
                    } else {
                        pos
                    };
                    let delta = current - start;
                    if delta.length() >= 0.5 {
                        // 複製元と同じグループに、距離を分割した位置へ配置する。
                        // フィッティング設定も複製元から引き継ぐ（適用時に再計算）。
                        let source = self.data.tool_by_id(src).cloned();
                        let Some(source) = source else {
                            self.selected = None;
                            return;
                        };
                        let (base1, base2, group, fit1, fit2, is_boundary) = match source {
                            MeasureTool::Distance { p1, p2, group, fit1, fit2, .. } => {
                                (p1, p2, group, fit1, fit2, false)
                            }
                            MeasureTool::Boundary { p1, p2, group, fit, .. } => {
                                (p1, p2, group, fit, fit, true)
                            }
                            _ => {
                                self.selected = None;
                                return;
                            }
                        };
                        self.mutate(doc, |data| {
                            for k in 1..=count {
                                let off = delta * (k as f64 / count as f64);
                                let id = data.next_id();
                                let p1 = base1 + off;
                                let p2 = base2 + off;
                                data.tools.push(if is_boundary {
                                    MeasureTool::Boundary {
                                        id,
                                        p1,
                                        p2,
                                        group,
                                        fit: fit1,
                                    }
                                } else {
                                    MeasureTool::Distance {
                                        id,
                                        p1,
                                        p2,
                                        group,
                                        fit1,
                                        fit2,
                                    }
                                });
                            }
                        });
                    }
                    self.selected = None;
                }
                // ツールボタン切替で in_progress はクリアされるので、
                // 他の作成中状態が残ることはない（万一残っていても破棄）。
                _ => {}
            },
            None => {
                // 選択モードではクリック単独では何もしない（ドラッグ開始時に
                // handle_overlay_input 側で直接選択して移動する）。
            }
            Some(ToolButton::RangeSelect) => {
                // 選択の解除は handle_range_input のクリック判定が行う。
            }
        }
    }

    /// クリック位置で選択方法を決める。端点に近ければ端点移動、
    /// 線分上なら全体移動。どちらでもなければ選択解除。
    fn select_at(&mut self, computed: &ComputedMeasure, pos: Pt2, threshold: f64) {
        let mut best: Option<(u64, EndpointWhich, f64)> = None;
        for t in computed
            .tools
            .iter()
            .filter(|t| matches!(t.kind, ToolKind::Distance | ToolKind::Boundary))
        {
            for (which, p) in [(EndpointWhich::P1, t.p1), (EndpointWhich::P2, t.p2)] {
                let d = (pos - p).length();
                if d <= threshold && best.map_or(true, |(_, _, bd)| d < bd) {
                    best = Some((t.id, which, d));
                }
            }
        }
        if let Some((id, which, _)) = best {
            self.selected = Some(id);
            self.drag = Some(Drag::Endpoint { id, which });
            self.drag_fitted = Some(computed.clone());
            return;
        }
        if let Some(t) = nearest_tool(computed, pos, threshold, |t| {
            matches!(t.kind, ToolKind::Distance | ToolKind::Boundary)
        }) {
            self.selected = Some(t.id);
            self.drag = Some(Drag::Whole {
                id: t.id,
                start: pos,
                orig_p1: t.p1,
                orig_p2: t.p2,
            });
            self.drag_fitted = Some(computed.clone());
        } else {
            self.selected = None;
            self.drag = None;
            self.drag_fitted = None;
        }
    }

    /// スナップ適用後の端点位置。`snapped` はどちらかの端点が線に乗ったか。
    fn resolve_distance(
        &self,
        p1: Pt2,
        p1_line: Option<SnapLine>,
        cursor: Pt2,
        lines: &[SnapLine],
        zoom: f32,
    ) -> (Pt2, Pt2, bool) {
        if self.data.prefs.snap {
            let threshold = SNAP_PX as f64 / zoom as f64;
            let (a, b) = snap_distance(p1, p1_line.as_ref(), cursor, lines, threshold);
            let snapped = (a - p1).length() > 1e-9 || (b - cursor).length() > 1e-9;
            return (a, b, snapped);
        }
        (p1, cursor, false)
    }

    /// 角度 4 方向スナップ（スナップが効いていないときだけ）。
    fn resolve_angle(&self, p1: Pt2, p2: Pt2, snapped: bool) -> Pt2 {
        if !snapped && self.data.prefs.angle == AngleMode::FourDir {
            return snap_angle_four(p1, p2);
        }
        p2
    }
}

/// `pos` に最も近い、条件を満たすツール（線分距離がしきい値以下）。
fn nearest_tool(
    computed: &ComputedMeasure,
    pos: Pt2,
    threshold: f64,
    pred: impl Fn(&ComputedTool) -> bool,
) -> Option<&ComputedTool> {
    computed
        .tools
        .iter()
        .filter(|t| pred(t))
        .filter(|t| distance_to_segment(pos, t.p1, t.p2) <= threshold)
        .min_by(|a, b| {
            distance_to_segment(pos, a.p1, a.p2)
                .partial_cmp(&distance_to_segment(pos, b.p1, b.p2))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::SourceCache;

    fn pt(x: f64, y: f64) -> Pt2 {
        Pt2::new(x, y)
    }

    /// id 1..=3 の測長を持つグループを作る。中点は (100,15) / (20,50) / (55,5)。
    fn data_with_group() -> (MeasureData, u64) {
        let mut data = MeasureData::default();
        let g = data.group_for_new_measurement();
        for (i, (p1, p2)) in [
            ((100.0, 10.0), (100.0, 20.0)),
            ((10.0, 50.0), (30.0, 50.0)),
            ((50.0, 5.0), (60.0, 5.0)),
        ]
        .iter()
        .enumerate()
        {
            data.tools.push(MeasureTool::Distance {
                id: i as u64 + 1,
                p1: pt(p1.0, p1.1),
                p2: pt(p2.0, p2.1),
                group: g,
                fit1: crate::measure::FitSettings::default(),
                fit2: crate::measure::FitSettings::default(),
            });
        }
        (data, g)
    }

    fn computed_of(data: &MeasureData) -> ComputedMeasure {
        data.compute(&crate::gray::Gray16::black(200, 200), None)
    }

    /// 範囲選択の中心位置判定（p1/p2 の中点）と枠への当たり判定。
    #[test]
    fn range_selection_helpers() {
        let (data, _g) = data_with_group(); // 中点は (100,15) / (20,50) / (55,5)
        let mut mode = MeasureMode::default();
        mode.index = 0;
        mode.data = data;

        // 中心位置が枠内の測長だけが選ばれる。
        assert_eq!(mode.distance_ids_in(pt(90.0, 10.0), pt(110.0, 20.0)), vec![1]);
        assert_eq!(mode.distance_ids_in(pt(0.0, 0.0), pt(90.0, 60.0)), vec![2, 3]);
        assert!(mode.distance_ids_in(pt(90.0, 10.0), pt(95.0, 12.0)).is_empty());

        // 当たり判定: 内側 = 移動、辺 = 変形、角 = 両軸の変形、外 = なし。
        let sel = RangeSelection {
            ids: vec![1],
            min: pt(0.0, 0.0),
            max: pt(20.0, 10.0),
        };
        assert!(matches!(MeasureMode::range_hit(&sel, pt(10.0, 5.0), 1.0), RangeHit::Move));
        assert!(matches!(
            MeasureMode::range_hit(&sel, pt(20.0, 5.0), 1.0),
            RangeHit::Resize { max_x: true, min_x: false, min_y: false, max_y: false }
        ));
        assert!(matches!(
            MeasureMode::range_hit(&sel, pt(0.0, 0.0), 1.0),
            RangeHit::Resize { min_x: true, max_x: false, min_y: true, max_y: false }
        ));
        assert!(matches!(MeasureMode::range_hit(&sel, pt(50.0, 50.0), 1.0), RangeHit::None));
        // 辺の延長線上（y が枠から外れる）は辺扱いしない。
        assert!(matches!(MeasureMode::range_hit(&sel, pt(20.0, 50.0), 1.0), RangeHit::None));
    }

    /// リサイズ時の線形写像。反対側の辺が基準になり、測長の p1/p2 も
    /// 枠と同じ倍率で動く。幅 0 の軸は平行移動になる。
    #[test]
    fn resize_map_scales_about_opposite_edge() {
        let (omin, omax) = (pt(0.0, 0.0), pt(10.0, 10.0));
        // 右辺だけ右へ 10 px: x 方向のみ 2 倍（左辺基準）。
        let (nmin, nmax) = (pt(0.0, 0.0), pt(20.0, 10.0));
        let p = resize_map(omin, omax, nmin, nmax, pt(5.0, 10.0));
        assert!((p.x - 10.0).abs() < 1e-9);
        assert!((p.y - 10.0).abs() < 1e-9, "動かさない軸はそのまま");
        // 両辺を動かす（左上が原点基準）と、枠内の点はその比率で移る。
        let (nmin, nmax) = (pt(5.0, 5.0), pt(15.0, 15.0));
        let p = resize_map(omin, omax, nmin, nmax, pt(0.0, 0.0));
        assert!((p.x - 5.0).abs() < 1e-9);
        assert!((p.y - 5.0).abs() < 1e-9);
        // 幅 0 の軸（すべての中心が同一 x）は平行移動だけになる。
        let p = resize_map(pt(10.0, 0.0), pt(10.0, 10.0), pt(15.0, 0.0), pt(16.0, 10.0), pt(10.0, 5.0));
        assert!((p.x - 15.0).abs() < 1e-9);
        assert!((p.y - 5.0).abs() < 1e-9);
    }

    #[test]
    fn sort_action_via_mutate_updates_mode_and_doc() {
        let (data, g) = data_with_group();
        let computed = computed_of(&data);
        let mut doc = Document::new("t");
        doc.push_command(Command::Measure { data: data.clone() });

        let mut mode = MeasureMode::default();
        mode.index = 0;
        mode.data = data;
        // results_ui のアクション処理と同じ経路（mutate → self.data 変更 → doc 反映）。
        mode.mutate(&mut doc, |d| sort_group(d, &computed, g, SortKey::X));
        assert_eq!(mode.data.group_tools(g), vec![2, 3, 1]);
        let Command::Measure { data: d } = &doc.commands.get(0).expect("Measure のみ").command
        else {
            panic!("Measure のはず");
        };
        assert_eq!(d.group_tools(g), vec![2, 3, 1], "doc 側のコマンドにも反映される");
    }

    #[test]
    fn sort_group_by_x_and_y() {
        let (mut data, g) = data_with_group();
        let computed = computed_of(&data);
        sort_group(&mut data, &computed, g, SortKey::X);
        assert_eq!(data.group_tools(g), vec![2, 3, 1], "x: 20, 55, 100 の順");
        // move_tool_in_group と同じく、ツール本体が動く（ID は変わらない）。
        let ids: Vec<u64> = data.tools.iter().map(|t| t.id()).collect();
        assert_eq!(ids, vec![2, 3, 1], "data.tools の並び自体が入れ替わる");
        let MeasureTool::Distance { p1, .. } = data.tools[0] else {
            panic!("先頭は二点間測長");
        };
        assert_eq!(p1, pt(10.0, 50.0), "先頭は元 id2 のツール本体");
        sort_group(&mut data, &computed, g, SortKey::Y);
        assert_eq!(data.group_tools(g), vec![3, 1, 2], "y: 5, 15, 50 の順");
    }

    #[test]
    fn move_tool_in_group_swaps_order() {
        let (mut data, g) = data_with_group();
        move_tool_in_group(&mut data, g, 1, 1);
        assert_eq!(data.group_tools(g), vec![2, 1, 3]);
        move_tool_in_group(&mut data, g, 3, -1);
        assert_eq!(data.group_tools(g), vec![2, 3, 1]);
        // 端のツールは動かない。
        move_tool_in_group(&mut data, g, 2, -1);
        assert_eq!(data.group_tools(g), vec![2, 3, 1]);
        move_tool_in_group(&mut data, g, 1, 1);
        assert_eq!(data.group_tools(g), vec![2, 3, 1]);
    }

    #[test]
    fn save_measure_json_writes_filename_first() {
        let dir = std::env::temp_dir().join(format!("tem_measure_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("sample.tif");
        image::GrayImage::from_raw(4, 4, vec![0u8; 16])
            .unwrap()
            .save(&img_path)
            .unwrap();

        let mut doc = Document::new("sample.tif");
        doc.push_command(Command::InsertImage {
            path: img_path.clone(),
        });
        let (mut data, _g) = data_with_group();
        data.output_path = "{dir}/{filename}_result.json".to_owned();
        doc.push_command(Command::Measure { data });
        doc.recompute(&mut SourceCache::new());

        let path = save_measure_json(&doc, 1).unwrap().expect("保存される");
        assert_eq!(path, dir.join("sample_result.json"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("{\n  \"filename\": \"sample.tif\""),
            "先頭にファイル名: {text}"
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["groups"][0]["values"].as_array().unwrap().len(), 3);

        // 出力先が空なら保存しない。
        let mut empty = MeasureData::default();
        empty.output_path = String::new();
        doc.commands
            .get_mut(1)
            .expect("Measure コマンド")
            .command = Command::Measure { data: empty };
        assert!(save_measure_json(&doc, 1).unwrap().is_none());

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&img_path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
