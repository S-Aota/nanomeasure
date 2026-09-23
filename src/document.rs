//! タブ 1 枚分の状態。コマンド履歴・中間結果・表示状態を画像単位で持つ。

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::command::{Command, CommandCategory, CommandItem};
use crate::frame::{Frame, Scale};
use crate::gray::Gray16;
use crate::measure::{ComputedMeasure, MeasureData};
use crate::view::ImageView;

pub type SourceCache = HashMap<PathBuf, Arc<Gray16>>;

/// どのタブからも参照されなくなったデコード済み画像を捨てる。
/// 画像は各タブの段（stages）が `Arc` で握っているので、参照が
/// キャッシュ自身だけになったものが不要な画像。
pub fn prune_source_cache(cache: &mut SourceCache) {
    cache.retain(|_, img| Arc::strong_count(img) > 1);
}

/// 1 コマンドを適用し終えた結果。
#[derive(Clone)]
struct Stage {
    frame: Frame,
    /// 有効な測長コマンドの段だけが持つ、再計算時点の測長データと計算結果。
    measure: Option<MeasureStage>,
}

#[derive(Clone)]
struct MeasureStage {
    data: MeasureData,
    /// フィッティング込みの計算結果。初めて参照されたときに一度だけ計算する
    /// （測長モードで編集中のコマンドは参照されないので、ドラッグ中に
    /// 毎フレームフィットし直すことはない）。
    computed: OnceCell<Arc<ComputedMeasure>>,
}

impl MeasureStage {
    /// 前回の段と測長データ・入力画像・スケールが同じなら計算結果を引き継ぎ、
    /// 変わっていれば未計算の段を作る。
    fn reuse_or_new(previous: Option<&Stage>, data: &MeasureData, frame: &Frame) -> Self {
        if let Some(prev) = previous
            && let Some(m) = &prev.measure
            && m.data == *data
            && Arc::ptr_eq(&prev.frame.image, &frame.image)
            && prev.frame.scale == frame.scale
        {
            return m.clone();
        }
        Self {
            data: data.clone(),
            computed: OnceCell::new(),
        }
    }
}

/// 画像に重ねる 1 コマンド分の測長結果。
pub struct MeasureOverlay<'a> {
    /// 再計算時点の測長データ（グループ名・並び順の参照用）。
    pub data: &'a MeasureData,
    pub computed: Arc<ComputedMeasure>,
    pub scale: Option<Scale>,
}

/// カテゴリごとのコマンド列。処理はカテゴリ順（入力 → 前処理 → 解析 → 出力）
/// に実行され、追加したコマンドはそれぞれのカテゴリの末尾に入る。
/// グローバル添字はカテゴリ順に通しで振った番号で、stages や選択と対応する。
#[derive(Clone, Debug, Default)]
pub struct CommandLists {
    input: Vec<CommandItem>,
    preprocess: Vec<CommandItem>,
    analysis: Vec<CommandItem>,
    output: Vec<CommandItem>,
}

impl CommandLists {
    /// カテゴリのコマンド列。
    pub fn list(&self, cat: CommandCategory) -> &[CommandItem] {
        match cat {
            CommandCategory::Input => &self.input,
            CommandCategory::Preprocess => &self.preprocess,
            CommandCategory::Analysis => &self.analysis,
            CommandCategory::Output => &self.output,
        }
    }

    pub fn list_mut(&mut self, cat: CommandCategory) -> &mut Vec<CommandItem> {
        match cat {
            CommandCategory::Input => &mut self.input,
            CommandCategory::Preprocess => &mut self.preprocess,
            CommandCategory::Analysis => &mut self.analysis,
            CommandCategory::Output => &mut self.output,
        }
    }

    /// 全カテゴリのコマンド数。
    pub fn len(&self) -> usize {
        CommandCategory::ALL
            .iter()
            .map(|&c| self.list(c).len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// カテゴリ順（= 処理順）に並べた全コマンド。
    pub fn iter(&self) -> impl Iterator<Item = &CommandItem> {
        CommandCategory::ALL
            .into_iter()
            .flat_map(|c| self.list(c).iter())
    }

    /// カテゴリの先頭コマンドのグローバル添字（= それ以前のカテゴリの合計数）。
    fn category_start(&self, cat: CommandCategory) -> usize {
        CommandCategory::ALL
            .iter()
            .take_while(|&&c| c != cat)
            .map(|&c| self.list(c).len())
            .sum()
    }

    /// グローバル添字をカテゴリとカテゴリ内添字へ直す。
    pub fn locate(&self, mut index: usize) -> Option<(CommandCategory, usize)> {
        for cat in CommandCategory::ALL {
            let len = self.list(cat).len();
            if index < len {
                return Some((cat, index));
            }
            index -= len;
        }
        None
    }

    /// グローバル添字のコマンド。
    pub fn get(&self, index: usize) -> Option<&CommandItem> {
        let (cat, local) = self.locate(index)?;
        Some(&self.list(cat)[local])
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut CommandItem> {
        let (cat, local) = self.locate(index)?;
        Some(&mut self.list_mut(cat)[local])
    }

    /// カテゴリの末尾へ追加し、グローバル添字を返す。
    fn push(&mut self, cat: CommandCategory, item: CommandItem) -> usize {
        let index = self.category_start(cat) + self.list(cat).len();
        self.list_mut(cat).push(item);
        index
    }

    /// グローバル添字のコマンドを除く。
    fn remove(&mut self, index: usize) -> Option<CommandItem> {
        let (cat, local) = self.locate(index)?;
        Some(self.list_mut(cat).remove(local))
    }
}

pub struct Document {
    pub title: String,
    pub commands: CommandLists,
    /// `stages[i]` = グローバル添字 i のコマンドを適用し終えた結果。
    /// 無効な行は直前の結果をそのまま持つ。
    stages: Vec<Option<Stage>>,
    /// 再計算が必要な最小のコマンド添字。
    dirty_from: Option<usize>,
    pub error: Option<String>,
    /// 表示テクスチャの作り直し判定に使う、結果画像の世代番号。
    pub generation: u64,
    /// 結果画像の実測レンジ（表示の自動コントラスト用）。
    cached_min_max: Option<(u16, u16)>,
    /// リストで選択されている行。コピー元と貼り付け位置に使う。
    selection: BTreeSet<usize>,
    /// Shift クリックの範囲選択の起点。
    anchor: Option<usize>,
    pub view: ImageView,
}

impl Document {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            commands: CommandLists::default(),
            stages: Vec::new(),
            dirty_from: None,
            error: None,
            generation: 0,
            cached_min_max: None,
            selection: BTreeSet::new(),
            anchor: None,
            view: ImageView::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// パイプラインの最終結果。
    pub fn result(&self) -> Option<&Frame> {
        self.stages
            .iter()
            .rev()
            .find_map(|s| s.as_ref().map(|s| &s.frame))
    }

    pub fn image(&self) -> Option<&Arc<Gray16>> {
        self.result().map(|f| &f.image)
    }

    /// 現在有効なスケール。スケール設定コマンドが無ければ `None`。
    pub fn scale(&self) -> Option<Scale> {
        self.result().and_then(|f| f.scale)
    }

    /// グローバル添字 `index` のコマンドに入力される結果（= 直前まで適用したもの）。
    pub fn input_to(&self, index: usize) -> Option<&Frame> {
        self.stages[..index.min(self.stages.len())]
            .iter()
            .rev()
            .find_map(|s| s.as_ref().map(|s| &s.frame))
    }

    /// `image` を入力とする測長コマンドの結果を処理順に集める（画面の
    /// オーバーレイ・画像出力・結果出力で共通）。対象は最後の再計算で
    /// 有効だった測長コマンドだけ。`skip` の添字（測長モードで編集中の
    /// コマンド）は編集セッション側が描くので除く。
    pub fn measure_overlays(
        &self,
        image: &Arc<Gray16>,
        skip: Option<usize>,
    ) -> Vec<MeasureOverlay<'_>> {
        self.stages
            .iter()
            .enumerate()
            .filter(|&(i, _)| Some(i) != skip)
            .filter_map(|(_, stage)| {
                let stage = stage.as_ref()?;
                let m = stage.measure.as_ref()?;
                if !Arc::ptr_eq(&stage.frame.image, image) {
                    return None;
                }
                let computed = m
                    .computed
                    .get_or_init(|| Arc::new(m.data.compute(&stage.frame.image, stage.frame.scale)))
                    .clone();
                Some(MeasureOverlay {
                    data: &m.data,
                    computed,
                    scale: stage.frame.scale,
                })
            })
            .collect()
    }

    /// コマンドをそのカテゴリの末尾へ追加する。グローバル添字を返す。
    fn append_item(&mut self, item: CommandItem) -> usize {
        let index = self.commands.push(item.command.category(), item);
        self.stages.insert(index, None);
        self.shift_selection(index, 1);
        self.invalidate_from(index);
        index
    }

    pub fn push_command(&mut self, command: Command) -> usize {
        self.append_item(CommandItem::new(command))
    }

    /// 各コマンドをそれぞれのカテゴリの末尾へ追加する。グローバル添字の列を返す。
    pub fn extend_commands(&mut self, items: impl IntoIterator<Item = CommandItem>) -> Vec<usize> {
        items
            .into_iter()
            .map(|item| self.append_item(item))
            .collect()
    }

    /// 全コマンドを入れ替える。処理順（カテゴリ順）になるよう分類し直す。
    pub fn replace_commands(&mut self, items: Vec<CommandItem>) {
        let mut lists = CommandLists::default();
        for item in items {
            lists.push(item.command.category(), item);
        }
        self.commands = lists;
        self.stages = vec![None; self.commands.len()];
        self.clear_selection();
        self.invalidate_from(0);
    }

    pub fn remove_command(&mut self, index: usize) {
        if self.commands.remove(index).is_none() {
            return;
        }
        self.stages.remove(index);
        self.selection.remove(&index);
        self.shift_selection(index, -1);
        self.anchor = None;
        self.invalidate_from(index);
    }

    /// 同じカテゴリ内で前後に入れ替える。カテゴリの境界はまたがない
    /// （処理順がカテゴリで固定されているため）。
    pub fn move_command(&mut self, index: usize, delta: isize) {
        let Some((cat, local)) = self.commands.locate(index) else {
            return;
        };
        let target = index as isize + delta;
        if target < 0 {
            return;
        }
        let target = target as usize;
        let Some((target_cat, target_local)) = self.commands.locate(target) else {
            return;
        };
        if target_cat != cat {
            return;
        }
        self.commands.list_mut(cat).swap(local, target_local);
        self.stages.swap(index, target);
        // 選択は行そのものに付いているので、入れ替えに追従させる。
        let (had_index, had_target) = (
            self.selection.contains(&index),
            self.selection.contains(&target),
        );
        set_membership(&mut self.selection, index, had_target);
        set_membership(&mut self.selection, target, had_index);
        self.anchor = None;
        self.invalidate_from(index.min(target));
    }

    /// `index` のコマンドまでで直近の画像挿入コマンドのパス。
    pub fn image_path_at(&self, index: usize) -> Option<&Path> {
        self.commands
            .iter()
            .take(index + 1)
            .filter_map(|c| match &c.command {
                Command::InsertImage { path } => Some(path.as_path()),
                _ => None,
            })
            .last()
    }

    // ------------------------------------------------------------ 選択

    pub fn is_selected(&self, index: usize) -> bool {
        self.selection.contains(&index)
    }

    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty()
    }

    /// 選択されている行を、リストの並び順で返す。
    pub fn selected_indices(&self) -> Vec<usize> {
        self.selection.iter().copied().collect()
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.anchor = None;
    }

    pub fn select_only(&mut self, index: usize) {
        self.selection.clear();
        self.selection.insert(index);
        self.anchor = Some(index);
    }

    pub fn toggle_selection(&mut self, index: usize) {
        if !self.selection.remove(&index) {
            self.selection.insert(index);
        }
        self.anchor = Some(index);
    }

    /// 起点から `index` までをまとめて選択する（Shift クリック）。
    pub fn select_range_to(&mut self, index: usize) {
        let Some(anchor) = self.anchor else {
            self.select_only(index);
            return;
        };
        let (lo, hi) = (anchor.min(index), anchor.max(index));
        self.selection.clear();
        self.selection.extend(lo..=hi);
    }

    pub fn select_indices(&mut self, indices: impl IntoIterator<Item = usize>) {
        self.selection.clear();
        self.selection.extend(indices);
        self.anchor = self.selection.iter().next().copied();
    }

    /// `from` 以降の選択位置を `delta` 分ずらす（行の挿入・削除に追従させる）。
    fn shift_selection(&mut self, from: usize, delta: isize) {
        self.selection = self
            .selection
            .iter()
            .map(|&i| {
                if i >= from {
                    (i as isize + delta).max(0) as usize
                } else {
                    i
                }
            })
            .collect();
    }

    pub fn invalidate_from(&mut self, index: usize) {
        self.dirty_from = Some(match self.dirty_from {
            Some(existing) => existing.min(index),
            None => index,
        });
    }

    /// 全コマンドを最初から計算し直す（ファイルの再読み込みを含む）。
    pub fn invalidate_all(&mut self) {
        self.invalidate_from(0);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_from.is_some()
    }

    /// 汚れている範囲だけをまとめて実行する。
    pub fn recompute(&mut self, cache: &mut SourceCache) {
        let Some(start) = self.dirty_from.take() else {
            return;
        };
        // 測長のような素通しコマンドの編集では結果画像が変わらない。
        // テクスチャの作り直し（フル画像の縮小）を毎フレーム発生させない
        // ため、再計算前後の結果画像を比較して世代を進めるか決める。
        let before = self.result_image_ptr();
        self.error = None;
        let mut current: Option<Frame> = self.input_to(start).cloned();

        for i in start..self.commands.len() {
            let item = self
                .commands
                .get(i)
                .expect("コマンド数はループ開始時に確定");
            if !item.enabled {
                // 無効な行は素通し。直前の結果をそのまま次段へ渡す。
                self.stages[i] = current.clone().map(|frame| Stage {
                    frame,
                    measure: None,
                });
                continue;
            }
            match item.command.apply(current.as_ref(), cache) {
                Ok(frame) => {
                    let measure = match &item.command {
                        Command::Measure { data } => Some(MeasureStage::reuse_or_new(
                            self.stages[i].as_ref(),
                            data,
                            &frame,
                        )),
                        _ => None,
                    };
                    current = Some(frame.clone());
                    self.stages[i] = Some(Stage { frame, measure });
                }
                Err(e) => {
                    self.error = Some(format!(
                        "{}: {e}",
                        item.command.label(crate::settings::DEFAULT_LENGTH_DIGITS)
                    ));
                    // 失敗行以降は結果なしにして、古い画像が残らないようにする。
                    for s in &mut self.stages[i..] {
                        *s = None;
                    }
                    break;
                }
            }
        }

        let after = self.result_image_ptr();
        if before != after {
            self.generation = self.generation.wrapping_add(1);
            self.cached_min_max = None;
        }
    }

    /// 最終結果の画像の同一性判定用ポインタ。
    fn result_image_ptr(&self) -> Option<*const Gray16> {
        self.result().map(|f| Arc::as_ptr(&f.image))
    }

    /// 表示に使う輝度レンジ。`auto` なら結果画像の実測 min/max に合わせる。
    pub fn display_range(&mut self, auto: bool) -> (u16, u16) {
        if !auto {
            return (0, u16::MAX);
        }
        if self.cached_min_max.is_none() {
            self.cached_min_max = Some(self.image().map_or((0, u16::MAX), |img| img.min_max()));
        }
        let (lo, hi) = self.cached_min_max.unwrap();
        if hi > lo { (lo, hi) } else { (0, u16::MAX) }
    }
}

fn set_membership(set: &mut BTreeSet<usize>, index: usize, member: bool) {
    if member {
        set.insert(index);
    } else {
        set.remove(&index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::measure::MeasureData;

    fn doc_with(n: usize) -> Document {
        let mut doc = Document::new("t");
        for i in 0..n {
            doc.push_command(Command::Rotate {
                angle_deg: i as f32,
            });
        }
        doc
    }

    fn angles(doc: &Document) -> Vec<f32> {
        doc.commands
            .iter()
            .map(|c| match c.command {
                Command::Rotate { angle_deg } => angle_deg,
                _ => f32::NAN,
            })
            .collect()
    }

    fn kinds(doc: &Document) -> Vec<CommandCategory> {
        doc.commands.iter().map(|c| c.command.category()).collect()
    }

    /// 回転コマンドの角度だけを処理順に取り出す（NaN は assert できないため）。
    fn rotations(doc: &Document) -> Vec<f32> {
        doc.commands
            .iter()
            .filter_map(|c| match c.command {
                Command::Rotate { angle_deg } => Some(angle_deg),
                _ => None,
            })
            .collect()
    }

    fn insert_cmd() -> Command {
        Command::InsertImage {
            path: PathBuf::from("a.tif"),
        }
    }

    /// 追加したコマンドはそのカテゴリの末尾（= カテゴリ順の位置）に入ること。
    #[test]
    fn push_appends_to_category_end() {
        let mut doc = Document::new("t");
        doc.push_command(Command::Rotate { angle_deg: 1.0 }); // 前処理
        doc.push_command(Command::Measure {
            data: MeasureData::default(),
        }); // 解析
        let index = doc.push_command(insert_cmd()); // 入力
        assert_eq!(index, 0, "入力カテゴリの末尾 = 全体の先頭");
        doc.push_command(Command::Rotate { angle_deg: 2.0 });
        assert_eq!(
            kinds(&doc),
            vec![
                CommandCategory::Input,
                CommandCategory::Preprocess,
                CommandCategory::Preprocess,
                CommandCategory::Analysis,
            ]
        );
        assert_eq!(rotations(&doc), vec![1.0, 2.0]);
    }

    #[test]
    fn move_never_crosses_category_boundary() {
        let mut doc = Document::new("t");
        doc.push_command(insert_cmd());
        doc.push_command(Command::Rotate { angle_deg: 1.0 });
        doc.move_command(1, -1); // 前処理の行を入力カテゴリへ動かそうとする
        assert_eq!(
            kinds(&doc),
            vec![CommandCategory::Input, CommandCategory::Preprocess]
        );
        // 同じカテゴリ内なら動く。
        doc.push_command(Command::Rotate { angle_deg: 2.0 });
        doc.move_command(1, 1);
        assert_eq!(rotations(&doc), vec![2.0, 1.0]);
    }

    /// 途中への追加で、後続の行の選択がずれて付いていくこと。
    #[test]
    fn push_into_middle_shifts_selection() {
        let mut doc = doc_with(2);
        doc.select_only(0);
        doc.push_command(insert_cmd());
        assert!(doc.is_selected(1), "挿入分だけ後ろへずれる");
        assert!(!doc.is_selected(0));
    }

    /// 履歴の読み込みでは、カテゴリ順になるよう分類し直されること。
    #[test]
    fn replace_commands_regroups_by_category() {
        let mut doc = Document::new("t");
        doc.replace_commands(vec![
            CommandItem::new(Command::Rotate { angle_deg: 1.0 }),
            CommandItem::new(insert_cmd()),
            CommandItem::new(Command::Rotate { angle_deg: 2.0 }),
        ]);
        assert_eq!(
            kinds(&doc),
            vec![
                CommandCategory::Input,
                CommandCategory::Preprocess,
                CommandCategory::Preprocess,
            ]
        );
        assert_eq!(rotations(&doc), vec![1.0, 2.0]);
    }

    #[test]
    fn remove_shifts_selection_down() {
        let mut doc = doc_with(3);
        doc.select_indices([2]);
        doc.remove_command(0);
        assert_eq!(angles(&doc), vec![1.0, 2.0]);
        assert!(doc.is_selected(1));
    }

    #[test]
    fn move_carries_selection_with_the_row() {
        let mut doc = doc_with(3);
        doc.select_only(0);
        doc.move_command(0, 1);
        assert_eq!(angles(&doc), vec![1.0, 0.0, 2.0]);
        assert!(doc.is_selected(1), "選択は動かした行に付いていく");
        assert!(!doc.is_selected(0));
    }

    #[test]
    fn shift_click_selects_a_range() {
        let mut doc = doc_with(5);
        doc.select_only(1);
        doc.select_range_to(3);
        assert_eq!(doc.selected_indices(), vec![1, 2, 3]);
    }

    /// 構造の変更は汚れ印だけを付け、勝手に計算しないこと。
    #[test]
    fn structural_edits_only_mark_dirty() {
        let mut doc = doc_with(3);
        let mut cache = SourceCache::new();
        doc.recompute(&mut cache);
        assert!(!doc.is_dirty());
        doc.move_command(0, 1);
        assert!(doc.is_dirty());
        assert!(doc.result().is_none(), "画像挿入が無いので結果は出ない");
    }
}
