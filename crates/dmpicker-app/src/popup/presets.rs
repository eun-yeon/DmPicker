//! 프리셋 목록과 저장·삭제·적용 처리, 이름을 붙이고 지우는 프리셋 시트

use super::*;
use crate::presets::Preset;

const SAVE_LABEL: &str = "지금 검색 저장";
const NO_PRESETS: &str = "저장한 프리셋이 없어요";
pub(super) const DELETE_LABEL: &str = "삭제";

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

/// 프리셋 시트 상태와 닫으면 돌아갈 입력·스크롤
pub(super) struct PresetEdit {
    /// `presets` 자리, 시트가 열린 동안엔 프리셋을 넣거나 지우지 않아서 그대로
    index: usize,
    /// 열기 전 입력 (프리셋에서 찾던 글), 시트에선 입력 칸에 이름을 씀
    edit: Editor,
    scroll: usize,
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
            let named = preset.name.as_deref().is_some_and(|name| name_matches(name, &query));
            if !finding || named || name_matches(&preset.query, &query) || name_matches(&preset.summary(), &query) {
                self.history.push(Row::Preset(index));
            }
        }
        if !finding && self.presets.is_empty() {
            self.history.push(Row::PresetEmpty);
        }
        if self.view == View::History {
            self.searched.clone_from(&self.history_query);
        }
        if self.sel_of == Sheet::History {
            self.reset_rows();
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
        self.preset_current.as_ref().map(Preset::query_line).unwrap_or_default()
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

    /// 프리셋 `index`의 시트를 엶, 열기 전 입력 (찾던 글)은 보관하고 입력 칸엔 지금 이름
    /// 접히거나 펼쳐지던 줄 모션은 버림, 닫으면 목록을 다시 채움
    pub(super) fn open_preset_sheet(&mut self, index: usize) {
        let Some(name) = self.presets.get(index).map(|p| p.name.clone().unwrap_or_default()) else { return };
        self.finish_input();
        self.preset_edit = Some(PresetEdit { index, edit: std::mem::take(&mut self.edit), scroll: self.scroll });
        self.clear_input();
        self.edit.insert(&name);
        self.preset_change = None;
        self.view = View::Preset;
        self.hover = Hit::None;
        self.retarget_panel();
    }

    /// 열기 전 입력과 히스토리 보기로 되돌리고 그 프리셋 줄과 스크롤을 돌려줌, 쓰던 이름은 버림
    /// 검색창을 닫거나 다시 열 때도 불림, 쓰던 이름이 찾는 글로 남지 않게
    pub(super) fn restore_preset_query(&mut self) -> Option<(Row, usize)> {
        let p = self.preset_edit.take()?;
        self.clear_input();
        self.edit = p.edit;
        self.seen = self.edit.text().to_owned();
        self.view = View::History;
        Some((Row::Preset(p.index), p.scroll))
    }

    /// 프리셋 탭으로 돌아가 그 프리셋 줄을 고름, 바에 쓰던 이름은 여기서 저장 안 함
    pub(super) fn close_preset_sheet(&mut self) {
        if self.preset_edit.is_none() {
            return;
        }
        // 쓰던 이름의 조합 중인 글자는 찾는 글로 안 넘어가게 버림
        self.preedit.clear();
        self.notify_composition(CPS_CANCEL);
        let Some((row, scroll)) = self.restore_preset_query() else { return };
        self.fill_history();
        self.retarget_panel();
        self.restore_row(row, scroll);
        self.hover = Hit::None;
    }

    /// 바에 쓰는 이름, 조합 중인 글자까지 앞뒤 공백 없이 보고 비면 이름 없음
    fn typed_preset_name(&self) -> Option<String> {
        Some(self.query().trim().to_owned()).filter(|n| !n.is_empty())
    }

    /// Enter·완료: 바의 이름을 저장하고 닫음
    pub(super) fn save_preset_name(&mut self) -> Action {
        let Some(index) = self.preset_edit.as_ref().map(|p| p.index) else { return Action::None };
        self.complete_composition();
        let name = self.typed_preset_name();
        let changed = self.presets.get(index).is_some_and(|p| p.name != name);
        if changed {
            self.presets[index].name = name;
        }
        self.close_preset_sheet();
        if changed { Action::Choice } else { Action::None }
    }

    /// 삭제: 시트를 닫고 목록에서 그 줄을 지움, 다음 줄을 고르고 지운 줄은 접힘
    pub(super) fn delete_sheet_preset(&mut self) -> Action {
        let Some(index) = self.preset_edit.as_ref().map(|p| p.index) else { return Action::None };
        self.close_preset_sheet();
        let Some(row) = self.history.iter().position(|r| *r == Row::Preset(index)) else { return Action::None };
        let action = self.delete_preset_row(row);
        // 시트를 닫으며 판 전체를 겹쳐 바꾸는 중이라 줄 목록은 따로 안 섞음, 섞으면 줄만 탭보다 늦게 나타남
        self.list_swap.reset();
        action
    }

    /// 프리셋 시트의 키, Enter는 이름 저장, Esc·F2는 저장하지 않고 닫음
    /// 입력 편집 키는 아래 공통 처리로 넘기고 모드·난이도 단축키 (Alt)와 줄 이동·다른 여는 키는 막음
    pub(super) fn preset_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, alt: bool, shift: bool) -> Option<Action> {
        if self.view != View::Preset {
            return None;
        }
        if vk == VK_ESCAPE || (!ctrl && !alt && !shift && vk == VK_F2) {
            self.close_preset_sheet();
        } else if vk == VK_RETURN {
            return Some(self.save_preset_name());
        } else if !alt && !matches!(vk, VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_APPS | VK_F10 | VK_F2) {
            return None;
        }
        Some(Action::None)
    }

    fn change_preset(&mut self, change: PresetChange) -> Action {
        match change.kind {
            ChangeKind::Inserted => self.presets.insert(0, change.preset.clone()),
            // 저장 줄이 없을 때도 있어서 줄 번호 대신 같은 프리셋을 찾아 지움 (중복 저장은 안 됨)
            ChangeKind::Removed => self.presets.retain(|p| *p != change.preset),
        }
        let before = self.history.clone();
        let (scroll, scroll_s, sel_y) = (self.scroll, self.scroll_s, self.sel_y);
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
                    BandRow::Row(Row::Preset(i)) => self.presets.get(i).map_or(Ok(()), |p| self.draw_preset_row(y, right, p)),
                    BandRow::Row(Row::PresetEmpty) => self.preset_empty_text().map_or(Ok(()), |text| self.draw_preset_empty(y, panel, text)),
                    BandRow::Row(_) => Ok(()),
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
            Ok(())
        })();
        unsafe { dc.PopAxisAlignedClip() };
        result
    }

    /// 프리셋 줄: 재킷 자리에 돋보기, 윗줄 이름 (없으면 검색어)과 아랫줄 (`Preset::row_text`)
    fn draw_preset_row(&self, y: f32, right: f32, preset: &Preset) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        self.draw_slot_icon(y, Icon::Search)?;
        let (title, sub) = preset.row_text();
        self.draw_row_text(y, right, title, &sub, None, (&g.brush(TEXT)?, &g.brush(SUB)?))?;
        Ok(())
    }

    /// 프리셋 시트 (다 편 시트 자리): 프리셋 줄과 삭제·완료 버튼, 안내 글
    /// 안내 글은 키워드 시트 라벨 자리·글꼴, `l0`·`r0`은 패널 안쪽 16 정렬선
    pub(super) fn draw_preset_sheet(&self, (l0, r0): (f32, f32)) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let pl = self.preset_layout();
        // 줄 제목은 바에 쓰는 이름을 바로 보여 줘서 저장하면 어떻게 보일지 그 자리에서 앎, 선택 강조 없음
        if let Some(preset) = self.preset_edit.as_ref().and_then(|p| self.presets.get(p.index)) {
            let shown = Preset { name: self.typed_preset_name(), ..preset.clone() };
            self.draw_preset_row(self.panel_top() + self.px(lay::PAD), pl.delete.left - self.px(8.0), &shown)?;
        }
        draw_button(g, &f.item, pl.delete, DELETE_LABEL, self.hover == Hit::PresetDelete, self.scale)?;
        draw_button(g, &f.item, pl.done, "완료", self.hover == Hit::DonePreset, self.scale)?;
        self.text_line("비워 두면 검색어가 제목으로 보여요", &f.sub, pl.note, self.px(16.0), &g.brush(FAINT)?, (l0, r0))?;
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
