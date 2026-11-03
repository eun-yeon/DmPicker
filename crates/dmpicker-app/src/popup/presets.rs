//! 프리셋 목록과 저장·삭제·적용 처리

use super::*;
use crate::presets::Preset;

const SAVE_LABEL: &str = "지금 검색 저장";
const NO_PRESETS: &str = "저장한 프리셋이 없어요";
const PRESET_DELETE: &str = "삭제하기";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChangeKind {
    Inserted,
    Removed,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PresetChange {
    pub kind: ChangeKind,
    /// 저장 줄을 포함한 변경 전 지운 줄, 저장 줄 자리에 새로 저장하면 None
    pub row: Option<usize>,
    pub preset: Preset,
}

impl Popup {
    pub fn load_presets(&mut self, value: &serde_json::Value) {
        self.presets.clear();
        for value in value.as_array().into_iter().flatten() {
            if let Some(preset) = Preset::from_json(value) {
                if !self.presets.contains(&preset) {
                    self.presets.push(preset);
                }
            }
        }
    }

    pub fn presets_json(&self) -> serde_json::Value {
        self.presets.iter().map(Preset::json).collect()
    }

    fn current_preset(&self) -> Option<Preset> {
        let query = self.list_edit.as_ref().map_or_else(|| self.query(), |saved| saved.edit.text().to_owned());
        Preset::capture(&self.catalog, &query, self.record_filter, self.mode_choice, self.diff_choice)
    }

    pub(super) fn fill_presets(&mut self) {
        self.history_entries.clear();
        self.history_query = if self.view == View::History {
            self.query()
        } else {
            String::new()
        };
        let finding = self.view == View::History && self.typed();
        let query = dmpicker_search::normalize(&self.history_query);
        self.preset_current = self.current_preset();
        self.history.clear();
        // 검색어가 없거나 이미 저장한 검색이면 저장 줄을 안 둠, 저장한 검색은 목록 제자리에 보임
        if !finding && self.preset_current.as_ref().is_some_and(|p| !self.presets.contains(p)) {
            self.history.push(Row::PresetSave);
        }
        for (index, preset) in self.presets.iter().enumerate() {
            if !finding || name_matches(&preset.query, &query) || name_matches(&preset.summary(), &query) {
                self.history.push(Row::Preset(index));
            }
        }
        if !finding && self.presets.is_empty() {
            self.history.push(Row::PresetEmpty);
        }
        // 한글 조합을 확정하며 다시 채워도 같은 프리셋이 보이면 삭제 버튼은 그대로, 안 그러면 Enter가 적용으로 넘어감
        // 그 줄이 안 보이게 됐으면 버튼도 바로 없앰
        if self.preset_menu.is_some_and(|i| !self.history.contains(&Row::Preset(i))) {
            self.drop_preset_menu();
        }
        if self.view == View::History {
            self.searched.clone_from(&self.history_query);
        }
        if self.sel_of == Sheet::History {
            // 삭제 버튼을 그대로 두면 강조도 그 줄에, 안 그러면 Enter가 화면 밖 버튼의 프리셋을 지움
            // 처음부터 다시 맞추면 강조·스크롤이 맨 위로 튀었다 돌아와서 지금 자리에서 옮김
            match self.preset_menu.and_then(|i| self.history.iter().position(|r| *r == Row::Preset(i))) {
                Some(row) => {
                    self.selected = row;
                    self.fit_scroll();
                    self.retarget_selection(false);
                }
                None => self.reset_rows(),
            }
        }
    }

    pub(super) fn preset_empty_text(&self) -> Option<&'static str> {
        if self.history.iter().any(|row| matches!(row, Row::Preset(_))) {
            None
        } else if self.history_query.is_empty() {
            Some(NO_PRESETS)
        } else {
            Some("프리셋에 맞는 검색이 없어요")
        }
    }

    /// 저장 줄 아랫줄: 지금 검색어와 요약
    fn preset_save_sub(&self) -> String {
        self.preset_current.as_ref().map(|preset| format!("{} · {}", preset.query, preset.summary())).unwrap_or_default()
    }

    /// 지금 검색을 목록 맨 위에 저장, 맨 위에 있던 저장 줄 자리가 그대로 새 프리셋 줄이 됨
    pub(super) fn save_preset(&mut self) -> Action {
        if self.view != View::History || self.history_tab != HistoryTab::Presets || self.typed() {
            return Action::None;
        }
        let Some(preset) = self.current_preset().filter(|p| !self.presets.contains(p)) else {
            return Action::None;
        };
        self.change_preset(PresetChange { kind: ChangeKind::Inserted, row: None, preset })
    }

    /// 고른 줄에서 Delete, 찾는 중엔 Delete가 입력 칸 글을 지워서 안 씀
    pub(super) fn delete_selected_preset(&mut self) -> Action {
        if self.typed() {
            return Action::None;
        }
        self.delete_preset_row(self.selected)
    }

    fn delete_preset_row(&mut self, row: usize) -> Action {
        if self.view != View::History || self.history_tab != HistoryTab::Presets {
            return Action::None;
        }
        let Some(Row::Preset(index)) = self.history.get(row).copied() else {
            return Action::None;
        };
        self.selected = row;
        let change = PresetChange {
            kind: ChangeKind::Removed,
            row: Some(row),
            preset: self.presets[index].clone(),
        };
        self.change_preset(change)
    }

    /// `row` 프리셋 줄 끝에 삭제 버튼을 띄우고 강조를 그 줄로, 저장 줄·안내 줄에선 안 띄움
    pub(super) fn open_preset_menu(&mut self, row: usize) -> bool {
        let Some(Row::Preset(index)) = self.history.get(row).copied() else {
            return false;
        };
        if self.preset_menu == Some(index) {
            return true;
        }
        // 줄이 접히고 펼쳐지는 동안엔 버튼 자리 (고정 줄 높이)가 그린 줄과 어긋남
        if self.preset_rows_moving() {
            return false;
        }
        self.selected = row;
        self.fit_scroll();
        self.retarget_selection(false);
        // 흐려지던 같은 버튼이면 지금 진하기에서 다시 나타나고, 다른 줄 버튼은 흐려지며 사라짐
        if self.preset_menu_drawn != Some(index) {
            self.preset_menu_leaving.retain(|(_, t)| !t.done() || t.value() > 0.0);
            let back = self.preset_menu_leaving.iter().position(|(i, _)| *i == index).map(|at| self.preset_menu_leaving.remove(at).1);
            if let Some(old) = self.preset_menu_drawn {
                let mut fade = self.preset_menu_a;
                fade.go(0.0, QUICK_SECS, QUICK_EASE);
                self.preset_menu_leaving.push((old, fade));
            }
            self.preset_menu_a = back.unwrap_or(Tween::new(0.0));
        }
        self.preset_menu = Some(index);
        self.preset_menu_drawn = Some(index);
        self.preset_menu_a.go(1.0, QUICK_SECS, QUICK_EASE);
        true
    }

    /// 삭제 버튼을 닫음, 버튼은 흐려지며 사라지고 그 줄 글 끝도 같이 돌아옴
    pub(super) fn close_preset_menu(&mut self) {
        if self.preset_menu.take().is_some() {
            self.preset_menu_a.go(0.0, QUICK_SECS, QUICK_EASE);
        }
    }

    /// 삭제 버튼을 바로 없앰, 줄이 지워지거나 안 보이게 될 때
    pub(super) fn drop_preset_menu(&mut self) {
        self.preset_menu = None;
        self.preset_menu_drawn = None;
        self.preset_menu_a = Tween::new(0.0);
        self.preset_menu_leaving.clear();
    }

    /// 누를 수 있는 삭제 버튼 자리, 띄운 프리셋이 목록에 있을 때만
    pub(super) fn preset_delete_rect(&self) -> Option<D2D_RECT_F> {
        self.preset_button_rect(self.preset_menu?)
    }

    /// 프리셋 `index` 줄 끝 삭제 버튼 자리, 프리셋 탭에서 그 줄이 목록에 있을 때만
    fn preset_button_rect(&self, index: usize) -> Option<D2D_RECT_F> {
        if self.view != View::History || self.history_tab != HistoryTab::Presets {
            return None;
        }
        let row = self.history.iter().position(|r| *r == Row::Preset(index))?;
        let y = self.panel_top() + self.list_top(Sheet::History) + self.px(lay::ROW_H) * row as f32 - self.scroll_s.value;
        Some(self.row_button(y, PRESET_DELETE))
    }

    pub(super) fn delete_menu_preset(&mut self) -> Action {
        let Some(index) = self.preset_menu else {
            return Action::None;
        };
        let gone = self.presets.get(index).cloned().map(|p| (p, self.preset_menu_a.value()));
        self.drop_preset_menu();
        let action = match self.history.iter().position(|r| *r == Row::Preset(index)) {
            Some(row) => self.delete_preset_row(row),
            None => Action::None,
        };
        // 버튼은 지워져 접히는 줄 띠 안에 그려서 줄과 같이 흐려지고 접힘, 제자리에서 겹쳐 바뀌면 옛 그림에 같이 들어 있음
        self.preset_menu_gone = gone.filter(|_| self.preset_rows_moving());
        action
    }

    fn change_preset(&mut self, change: PresetChange) -> Action {
        match change.kind {
            ChangeKind::Inserted => self.presets.insert(0, change.preset.clone()),
            // 저장 줄이 없을 때도 있어서 줄 번호 대신 같은 프리셋을 찾아 지움 (중복 저장은 안 됨)
            ChangeKind::Removed => self.presets.retain(|p| *p != change.preset),
        }
        let before = self.history.clone();
        let (scroll, scroll_s, sel_y) = (self.scroll, self.scroll_s, self.sel_y);
        // 프리셋 자리가 바뀌어서 삭제 버튼은 바로 없앰
        self.drop_preset_menu();
        self.fill_presets();
        // 맨 위 줄이 저장 줄과 서로 바뀌거나 (저장, 맨 위 지금 검색 프리셋 지우기) 찾던 마지막 줄이 안내 문구가 되면 줄 자리는 그대로 두고 글만 겹쳐 바꿈
        // 접고 펼치면 지운 줄이 새 저장 줄에 밀려 내려가며 접혀 보임
        let in_place = change.kind == ChangeKind::Inserted
            || (change.row == Some(0) && self.history.first() == Some(&Row::PresetSave))
            || self.history.is_empty();
        // 아래쪽 지금 검색 프리셋을 지우면 맨 위에 저장 줄이 생겨 한 줄씩 밀림
        let pushed = !in_place && self.history.first() == Some(&Row::PresetSave) && before.first() != Some(&Row::PresetSave);
        // 저장 줄을 눌렀으면 강조는 그 자리 새 프리셋 줄에 그대로, 지웠으면 다음 줄 (없으면 이전 줄)
        self.selected = change.row.map_or(0, |row| {
            let row = row + pushed as usize;
            (row..self.history.len()).find(|&i| self.history[i].selectable())
                .or_else(|| (0..row.min(self.history.len())).rev().find(|&i| self.history[i].selectable())).unwrap_or(0)
        });
        self.scroll = scroll;
        // 다시 채우며 맨 위로 맞춘 스크롤·강조를 되돌림, 지금 자리에서 새 줄로 미끄러짐
        (self.scroll_s, self.sel_y) = (scroll_s, sel_y);
        self.fit_scroll();
        self.retarget_selection(false);
        // 다시 채운 목록에 옛 줄 번호 호버가 남지 않게, 마우스를 움직이면 다시 정함
        self.hover = Hit::Inside;
        self.start_preset_motion(&before, &change, in_place);
        if in_place {
            self.list_swap.swap();
        }
        self.preset_change = Some(change);
        self.retarget_panel();
        Action::Choice
    }

    pub(super) fn apply_preset(&mut self, index: usize) -> Action {
        let Some(preset) = self.presets.get(index).cloned() else {
            return Action::None;
        };
        self.complete_composition();
        self.list_edit = None;
        self.clear_input();
        self.edit.insert(&preset.query);
        self.seen = self.query();
        self.record_filter = preset.filter;
        if let Some(mode) = preset.mode {
            self.mode_choice = mode;
        }
        if let Some(diff) = preset.diff {
            self.diff_choice = diff;
        }
        self.view = View::Results;
        self.kb_focus = None;
        self.preset_change = None;
        self.close_preset_menu();
        if !self.in_zone && self.leave_since.is_none() {
            self.leave_since = Some(Instant::now());
        }
        // 같은 검색어라도 기록 조건·정렬·채보 설정이 달라질 수 있음
        self.searched.clear();
        self.search_now();
        Action::Choice
    }
}

/// 넣고 지우는 동안의 줄 자리, `Row`는 `history` 줄을 차례로 가리킴
/// 지운 줄은 접힐 때까지 그 자리에 `Gone`으로 남음
pub(super) enum Slot {
    /// 넣는 중이면 펼치는 모션
    Row(Option<RowFold>),
    Gone(Gone, RowFold),
}

/// 지워져 접히는 동안 그리는 내용
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Gone {
    Preset(Preset),
    /// 첫 프리셋을 저장해 빠지는 빈 목록 안내
    Empty,
}

/// 그릴 줄 하나, 위쪽은 목록 맨 위에서 스크롤 전 px
struct Band<'a> {
    row: BandRow<'a>,
    top: f32,
    height: f32,
    alpha: f32,
    /// 펼쳐지거나 접히는 중, 제 높이 안으로 잘라 그림
    moving: bool,
}

enum BandRow<'a> {
    Row(Row),
    Gone(&'a Gone),
}

impl Popup {
    /// 저장·지우기로 바뀐 줄의 모션을 시작함, `before`는 바뀌기 전 줄
    /// 모션 도중 또 바뀌어도 지금 보이는 자리에서 이어감, `in_place`면 맨 위 줄은 접고 펼치지 않음
    fn start_preset_motion(&mut self, before: &[Row], change: &PresetChange, in_place: bool) {
        let mut slots = std::mem::take(&mut self.preset_slots);
        // 지난 변경이 취소된 뒤라면 남은 자리는 버림
        if self.preset_change.is_none() || live_rows(&slots) != before.len() {
            slots = before.iter().map(|_| Slot::Row(None)).collect();
        }
        if !in_place {
            if let Some(row) = change.row {
                if let Some(at) = live_slot(&slots, row) {
                    slots[at] = Slot::Gone(Gone::Preset(change.preset.clone()), fold_from(&slots[at], false));
                }
            }
            // 아래쪽 지금 검색 프리셋을 지우면 맨 위에 저장 줄이 다시 펼쳐짐
            if self.history.first() == Some(&Row::PresetSave) && before.first() != Some(&Row::PresetSave) {
                slots.insert(0, Slot::Row(Some(RowFold::new(true))));
            }
        }
        // 빈 목록 안내 띠도 한 줄처럼 접히고 펼쳐짐, 늘 맨 끝
        let had = before.last() == Some(&Row::PresetEmpty);
        let has = self.history.last() == Some(&Row::PresetEmpty);
        if had && !has {
            if let Some(at) = slots.iter().rposition(|s| matches!(s, Slot::Row(_))) {
                slots[at] = Slot::Gone(Gone::Empty, fold_from(&slots[at], false));
            }
        } else if has && !had {
            let fold = match slots.iter().position(|s| matches!(s, Slot::Gone(Gone::Empty, _))) {
                Some(i) => fold_from(&slots.remove(i), true),
                None => RowFold::new(true),
            };
            slots.push(Slot::Row(Some(fold)));
        }
        if slots.iter().any(|s| !matches!(s, Slot::Row(None))) {
            self.preset_slots = slots;
        }
    }

    /// 줄 모션을 `dt`만큼, 판 높이는 줄을 같은 프레임에 따라감 (`follow_rows`)
    pub(super) fn step_preset_motion(&mut self, dt: f32) {
        if self.preset_change.is_none() {
            self.preset_slots.clear();
        }
        if self.preset_slots.is_empty() {
            self.preset_menu_gone = None;
            return;
        }
        let before = self.panel_target();
        for slot in &mut self.preset_slots {
            match slot {
                Slot::Row(Some(fold)) | Slot::Gone(_, fold) => fold.step(dt),
                Slot::Row(None) => {}
            }
        }
        self.preset_slots.retain(|s| !matches!(s, Slot::Gone(_, fold) if fold.done()));
        for slot in &mut self.preset_slots {
            if let Slot::Row(fold) = slot {
                if fold.is_some_and(RowFold::done) {
                    *fold = None;
                }
            }
        }
        if self.preset_slots.iter().all(|s| matches!(s, Slot::Row(None))) {
            self.preset_slots.clear();
        }
        if self.view == View::History && self.sheet == Sheet::History && self.history_tab == HistoryTab::Presets {
            self.follow_rows(before, dt);
        }
    }

    /// 프리셋 줄이 접히거나 펼쳐지는 중
    pub(super) fn preset_rows_moving(&self) -> bool {
        self.view == View::History && self.history_tab == HistoryTab::Presets && self.preset_motion().is_some()
    }

    /// 지금 쓰는 줄 자리, 변경이 취소됐거나 줄 수가 안 맞으면 None
    fn preset_motion(&self) -> Option<&[Slot]> {
        let valid = self.preset_change.is_some() && !self.preset_slots.is_empty() && live_rows(&self.preset_slots) == self.history.len();
        valid.then_some(self.preset_slots.as_slice())
    }

    fn preset_bands(&self) -> Vec<Band<'_>> {
        let row_h = self.px(lay::ROW_H);
        let empty_h = self.px(lay::EXCLUDE_EMPTY_H);
        let mut rows = self.history.iter().copied();
        let mut bands = Vec::new();
        let mut top = 0.0;
        let mut push = |row, full: f32, fold: Option<RowFold>| {
            let height = full * fold.map_or(1.0, RowFold::height);
            let alpha = fold.map_or(1.0, |f| f.alpha);
            bands.push(Band { row, top, height, alpha, moving: fold.is_some() });
            top += height;
        };
        let full = |row: Row| if row == Row::PresetEmpty { empty_h } else { row_h };
        match self.preset_motion() {
            None => rows.for_each(|row| push(BandRow::Row(row), full(row), None)),
            Some(slots) => {
                for slot in slots {
                    match slot {
                        Slot::Row(fold) => {
                            let Some(row) = rows.next() else { break };
                            push(BandRow::Row(row), full(row), *fold);
                        }
                        Slot::Gone(gone, fold) => {
                            let full = if *gone == Gone::Empty { empty_h } else { row_h };
                            push(BandRow::Gone(gone), full, Some(*fold));
                        }
                    }
                }
            }
        }
        bands
    }

    /// 다 편 판에서 줄 목록 높이, 한 화면 줄 수를 넘으면 그만큼 (줄이 없으면 문구 한 줄)
    /// 빈 목록 안내는 검색 제외의 빈 목록과 같은 높이 띠
    pub(super) fn preset_list_h(&self) -> f32 {
        if self.history.is_empty() {
            return self.px(lay::ROW_H);
        }
        let content = self.preset_bands().last().map_or(0.0, |b| b.top + b.height);
        content.min(self.px(lay::ROW_H) * lay::ROWS as f32)
    }

    /// 프리셋 탭 줄 목록: 선택 강조와 저장 줄·프리셋 줄·빈 목록 안내, 넣고 지우는 동안엔 접히고 펼쳐지는 줄
    /// `top`은 첫 줄 자리 위쪽, `panel`은 지금 판
    pub(super) fn draw_preset_list(&self, top: f32, panel: D2D_RECT_F) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        let dc = &g.dc;
        let inset = self.px(lay::PAD);
        if self.history.get(self.selected).is_none_or(|r| r.selectable()) {
            let t = self.panel_top() + self.sel_y.value;
            let hl = rrect(panel.left + inset, t, panel.right - inset, t + self.px(lay::ROW_H), self.px(lay::PANEL_R - lay::PAD));
            unsafe { dc.FillRoundedRectangle(&hl, &g.brush(white(FILL_SELECTED))?) };
        }
        // 레벨 칸이 없어 글은 16 정렬선까지
        let right = panel.right - self.px(lay::INSET);
        let clip = rect(panel.left, top, panel.right, top + self.preset_list_h());
        // 그릴 삭제 버튼마다 (프리셋, 자리, 진하기), 지금 버튼과 다른 줄로 옮겨 사라지는 버튼
        let buttons: Vec<_> = self.preset_menu_drawn.map(|i| (i, self.preset_menu_a))
            .into_iter()
            .chain(self.preset_menu_leaving.iter().copied())
            .filter_map(|(i, t)| Some((i, self.preset_button_rect(i)?, t.value())).filter(|b| b.2 > 0.0))
            .collect();
        unsafe { dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED) };
        let result = (|| -> Result<()> {
            for band in self.preset_bands() {
                let y = top + band.top - self.scroll_s.value;
                if band.alpha <= 0.0 || y + band.height <= clip.top || y >= clip.bottom {
                    continue;
                }
                if band.moving {
                    unsafe {
                        dc.PushAxisAlignedClip(&rect(panel.left, y, panel.right, y + band.height), D2D1_ANTIALIAS_MODE_ALIASED);
                        dc.PushLayer(&gfx::layer(band.alpha, None), None);
                    }
                }
                let drawn = match band.row {
                    BandRow::Row(Row::PresetSave) => self.draw_preset_save(y, right),
                    // 삭제 버튼을 띄운 줄은 글이 버튼 앞 8에서 끝남, 버튼이 나타나고 사라지는 만큼 움직임
                    BandRow::Row(Row::Preset(i)) if buttons.iter().any(|b| b.0 == i) => {
                        let (_, b, a) = buttons.iter().find(|b| b.0 == i).unwrap();
                        let right = right + (b.left - self.px(8.0) - right) * a;
                        self.presets.get(i).map_or(Ok(()), |p| self.draw_preset_row(y, right, p))
                    }
                    BandRow::Row(Row::Preset(i)) => self.presets.get(i).map_or(Ok(()), |p| self.draw_preset_row(y, right, p)),
                    BandRow::Row(Row::PresetEmpty) => self.preset_empty_text().map_or(Ok(()), |text| self.draw_preset_empty(y, panel, text)),
                    BandRow::Row(_) => Ok(()),
                    // 나타나던 중에 지웠으면 그때 진하기 그대로, 줄이 흐려지는 만큼 같이 흐려짐
                    BandRow::Gone(Gone::Preset(p)) if self.preset_menu_gone.as_ref().is_some_and(|(g, _)| g == p) => {
                        let a = self.preset_menu_gone.as_ref().map_or(0.0, |(_, a)| *a);
                        let b = self.row_button(y, PRESET_DELETE);
                        let f = self.fonts.as_ref().unwrap();
                        self.draw_preset_row(y, right + (b.left - self.px(8.0) - right) * a, p).and_then(|_| {
                            unsafe { dc.PushLayer(&gfx::layer(a, None), None) };
                            let drawn = draw_button(g, &f.item, b, PRESET_DELETE, false, self.scale);
                            unsafe { dc.PopLayer() };
                            drawn
                        })
                    }
                    BandRow::Gone(Gone::Preset(p)) => self.draw_preset_row(y, right, p),
                    BandRow::Gone(Gone::Empty) => self.draw_preset_empty(y, panel, NO_PRESETS),
                };
                if band.moving {
                    unsafe {
                        dc.PopLayer();
                        dc.PopAxisAlignedClip();
                    }
                }
                drawn?;
            }
            for &(i, b, a) in &buttons {
                let f = self.fonts.as_ref().unwrap();
                let lit = self.preset_menu == Some(i) && self.hover == Hit::PresetDelete;
                unsafe { dc.PushLayer(&gfx::layer(a, None), None) };
                let drawn = draw_button(g, &f.item, b, PRESET_DELETE, lit, self.scale);
                unsafe { dc.PopLayer() };
                drawn?;
            }
            Ok(())
        })();
        unsafe { dc.PopAxisAlignedClip() };
        result
    }

    /// 프리셋 줄: 재킷 자리에 돋보기, 윗줄 검색어와 아랫줄 요약
    fn draw_preset_row(&self, y: f32, right: f32, preset: &Preset) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        self.draw_slot_icon(y, Icon::Search)?;
        self.draw_row_text(y, right, &preset.query, &preset.summary(), None, (&g.brush(TEXT)?, &g.brush(SUB)?))?;
        Ok(())
    }

    /// 저장 줄: 재킷 자리에 북마크
    fn draw_preset_save(&self, y: f32, right: f32) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        let slot = self.jacket_slot(y);
        unsafe { g.dc.FillRoundedRectangle(&slot, &g.brush(white(FILL_HOVER))?) };
        let (cx, cy) = ((slot.rect.left + slot.rect.right) / 2.0, (slot.rect.top + slot.rect.bottom) / 2.0);
        g.fill_icon(Icon::Bookmark, cx, cy, self.px(lay::ICON), &g.brush(TEXT)?)?;
        self.draw_row_text(y, right, SAVE_LABEL, &self.preset_save_sub(), None, (&g.brush(TEXT)?, &g.brush(SUB)?))?;
        Ok(())
    }

    /// 빈 목록 안내: 검색 제외의 빈 목록처럼 띠 가운데
    fn draw_preset_empty(&self, y: f32, panel: D2D_RECT_F, text: &str) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let h = self.px(lay::EXCLUDE_EMPTY_H);
        let l = g.layout(text, &f.item, panel.right - panel.left, h)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            l.GetMetrics(&mut m)?;
            g.dc.DrawTextLayout(gfx::pt(panel.left, y + ((h - m.height) / 2.0).round()), &l, &g.brush(SUB)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(())
    }
}

fn live_rows(slots: &[Slot]) -> usize {
    slots.iter().filter(|s| matches!(s, Slot::Row(_))).count()
}

/// `history`의 `row`번째 줄 자리
fn live_slot(slots: &[Slot], row: usize) -> Option<usize> {
    slots.iter().enumerate().filter(|(_, s)| matches!(s, Slot::Row(_))).nth(row).map(|(i, _)| i)
}

/// 이 자리가 바뀔 때의 모션, 반대로 움직이던 중이면 거꾸로 돌려 이어감
fn fold_from(slot: &Slot, opening: bool) -> RowFold {
    match slot {
        Slot::Row(Some(fold)) | Slot::Gone(_, fold) if fold.opening != opening => fold.reversed(),
        _ => RowFold::new(opening),
    }
}
