//! タブ 1 枚分の状態。コマンド履歴・中間結果・表示状態を画像単位で持つ。

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use crate::command::{Command, CommandItem};
use crate::frame::{Frame, Scale};
use crate::gray::Gray16;
use crate::view::ImageView;

pub type SourceCache = HashMap<PathBuf, Arc<Gray16>>;

pub struct Document {
    pub title: String,
    pub commands: Vec<CommandItem>,
    /// `stages[i]` = コマンド i を適用し終えた結果。無効な行は直前の結果をそのまま持つ。
    stages: Vec<Option<Frame>>,
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
            commands: Vec::new(),
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
        self.stages.iter().rev().find_map(|s| s.as_ref())
    }

    pub fn image(&self) -> Option<&Arc<Gray16>> {
        self.result().map(|f| &f.image)
    }

    /// 現在有効なスケール。スケール設定コマンドが無ければ `None`。
    pub fn scale(&self) -> Option<Scale> {
        self.result().and_then(|f| f.scale)
    }

    /// コマンド `index` に入力される結果（= 直前まで適用したもの）。
    pub fn input_to(&self, index: usize) -> Option<&Frame> {
        self.stages[..index.min(self.stages.len())]
            .iter()
            .rev()
            .find_map(|s| s.as_ref())
    }

    pub fn push_command(&mut self, command: Command) -> usize {
        let index = self.commands.len();
        self.commands.push(CommandItem::new(command));
        self.stages.push(None);
        self.invalidate_from(index);
        index
    }

    pub fn extend_commands(&mut self, items: impl IntoIterator<Item = CommandItem>) {
        let start = self.commands.len();
        for item in items {
            self.commands.push(item);
            self.stages.push(None);
        }
        if self.commands.len() > start {
            self.invalidate_from(start);
        }
    }

    /// `at` の位置に割り込ませる形で挿入する。挿入した範囲を返す。
    pub fn insert_commands(
        &mut self,
        at: usize,
        items: Vec<CommandItem>,
    ) -> std::ops::Range<usize> {
        let at = at.min(self.commands.len());
        let count = items.len();
        if count == 0 {
            return at..at;
        }
        self.commands.splice(at..at, items);
        self.stages.splice(at..at, std::iter::repeat_n(None, count));
        self.shift_selection(at, count as isize);
        self.invalidate_from(at);
        at..at + count
    }

    pub fn replace_commands(&mut self, items: Vec<CommandItem>) {
        self.stages = vec![None; items.len()];
        self.commands = items;
        self.clear_selection();
        self.invalidate_from(0);
    }

    pub fn remove_command(&mut self, index: usize) {
        if index >= self.commands.len() {
            return;
        }
        self.commands.remove(index);
        self.stages.remove(index);
        self.selection.remove(&index);
        self.shift_selection(index, -1);
        self.anchor = None;
        self.invalidate_from(index);
    }

    pub fn move_command(&mut self, index: usize, delta: isize) {
        let target = index as isize + delta;
        if target < 0 || target as usize >= self.commands.len() {
            return;
        }
        let target = target as usize;
        self.commands.swap(index, target);
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

    /// 貼り付け先。選択があればその先頭、無ければ末尾。
    pub fn paste_position(&self) -> usize {
        self.selection
            .iter()
            .next()
            .copied()
            .unwrap_or(self.commands.len())
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
        let mut current: Option<Frame> = if start == 0 {
            None
        } else {
            self.stages[..start].iter().rev().find_map(|s| s.clone())
        };

        for i in start..self.commands.len() {
            let item = &self.commands[i];
            if !item.enabled {
                // 無効な行は素通し。直前の結果をそのまま次段へ渡す。
                self.stages[i] = current.clone();
                continue;
            }
            match item.command.apply(current.as_ref(), cache) {
                Ok(out) => {
                    current = Some(out.clone());
                    self.stages[i] = Some(out);
                }
                Err(e) => {
                    self.error = Some(format!("{}: {e}", item.command.label()));
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

    #[test]
    fn paste_position_is_selection_start_or_end() {
        let mut doc = doc_with(3);
        assert_eq!(doc.paste_position(), 3, "未選択なら末尾");
        doc.select_only(1);
        assert_eq!(doc.paste_position(), 1);
    }

    #[test]
    fn insert_shifts_selection_and_keeps_order() {
        let mut doc = doc_with(3);
        doc.select_only(1);
        let range = doc.insert_commands(
            1,
            vec![CommandItem::new(Command::Rotate { angle_deg: 9.0 })],
        );
        assert_eq!(range, 1..2);
        assert_eq!(angles(&doc), vec![0.0, 9.0, 1.0, 2.0]);
        // もとの 1 行目は 2 行目へずれる。
        assert!(doc.is_selected(2));
        assert!(!doc.is_selected(1));
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
