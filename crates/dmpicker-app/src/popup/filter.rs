//! 기록 조건과 필터 시트의 입력·표시 연결

use super::*;
use record_filter::{Bound, Control, Direction, Sort};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FilterEmpty {
    pub text: &'static str,
    pub button: &'static str,
    pub control: Control,
}

impl Popup {
    fn row_pattern(&self, row: Row) -> Option<record_filter::PatternRef<'_>> {
        match row {
            Row::Song(i) => Some((self.catalog.songs.get(i)?, self.mode_choice, self.diff_choice)),
            Row::Chart(i, mode, diff) => Some((self.catalog.songs.get(i)?, mode, diff)),
            Row::Unavailable(i) => {
                let pattern = self.playlist_data()?.entries.get(i)?.pattern;
                Some((self.unavailable_song(i)?, pattern.mode, pattern.diff))
            }
            _ => None,
        }
    }

    fn record_allows(&self, row: Row) -> bool {
        self.row_pattern(row).is_some_and(|pattern| self.record_filter.allows(pattern, &self.records))
    }

    pub(super) fn filtered_results(&self, query: &str) -> (Vec<Row>, bool) {
        let rows = self.find_in(query, self.owned.as_ref());
        if self.catalog.parse_filter(query).is_none() {
            return (rows, false);
        }
        let had_results = picks(&rows).next().is_some();
        let mut rows: Vec<_> = picks(&rows).filter(|&row| self.record_allows(row)).collect();
        let rate_ready = rows.iter().all(|&row| self.row_pattern(row).is_some_and(|(_, mode, _)| self.records.known(mode)));
        let (sort, direction) = self.record_filter.effective_sort(&self.records);
        let title_order = sort == Sort::Title && direction == Direction::Ascending;
        // 받은 모드만으로 정렬하면 못 받은 모드가 기록 없는 줄처럼 뒤로 밀림
        if !title_order && (sort != Sort::Rate || rate_ready) {
            rows.sort_by(|&a, &b| self.record_filter.compare(self.row_pattern(a).unwrap(), self.row_pattern(b).unwrap(), &self.records));
        }
        let empty = had_results && rows.is_empty();
        if !rows.is_empty() {
            rows.insert(0, Row::Random);
        }
        (rows, empty)
    }

    pub(super) fn main_results(&mut self, query: &str) -> Vec<Row> {
        self.result_query = query.to_owned();
        let (rows, empty) = self.filtered_results(query);
        self.filter_empty = empty;
        rows
    }

    /// 기록 필터는 즐겨찾기 탭 후보만 여기서 거름, 조건 검색 결과는 찾을 때 이미 거름
    fn random_allows(&self, sheet: Sheet) -> impl Fn(&Row) -> bool + '_ {
        let favorites = sheet == Sheet::History && self.history_tab == HistoryTab::Favorites;
        move |&row| !favorites || self.record_allows(row)
    }

    pub(super) fn random_candidates(&self, sheet: Sheet) -> Vec<Row> {
        picks(self.rows_of(sheet)).filter(self.random_allows(sheet)).collect()
    }

    pub(super) fn random_scope(&self, sheet: Sheet) -> Vec<Row> {
        let allows = self.random_allows(sheet);
        self.rows_of(sheet).iter().copied().filter(|row| row.song().is_none() || allows(row)).collect()
    }

    pub(super) fn random_note(&self, sheet: Sheet) -> String {
        self.rolls.note(&self.random_candidates(sheet))
    }

    pub fn records_reply(&mut self, reply: varchive::Reply) -> bool {
        if !self.records.apply(reply) {
            return false;
        }
        self.refresh_record_rows();
        true
    }

    pub(super) fn refresh_record_rows(&mut self) {
        let rows = if self.view == View::Filter {
            &self.results
        } else {
            self.rows()
        };
        let selected = rows.get(self.selected).copied();
        let scroll = self.scroll;
        if self.view == View::History && self.history_tab == HistoryTab::Presets {
            let before = self.history.clone();
            self.fill_presets();
            // 모드·난이도로 저장 줄이 생기거나 빠지면 판이 바뀐 것, 접히던 줄 모션은 줄 수가 안 맞아 버림
            // 사라지던 삭제 버튼도 버림, 새 층에선 한 줄 밀린 자리에 그려져 옛 층 것과 두 개로 보임
            if self.history != before {
                self.list_swap.swap();
                self.preset_change = None;
                self.drop_preset_menu();
            }
        }
        if self.playlist.is_none() && self.catalog.parse_filter(&self.result_query).is_some() {
            let query = self.result_query.clone();
            self.results = self.main_results(&query);
        }
        if let Some(from) = self.roll_edit.as_ref().map(|r| r.from) {
            let sheet = if from == View::History {
                Sheet::History
            } else {
                Sheet::Results
            };
            let scope = self.random_scope(sheet);
            self.roll_edit.as_mut().unwrap().scope = scope;
            self.fill_rolls();
        }
        if self.view == View::Filter {
            self.selected = selected.and_then(|row| self.results.iter().position(|r| *r == row)).unwrap_or(0);
            self.scroll = scroll.min(self.selected);
        } else if self.listing() {
            self.reset_rows();
            if let Some(row) = selected {
                self.restore_row(row, scroll);
            }
        }
        self.retarget_panel();
    }

    pub(super) fn set_diff(&mut self, diff: Diff) {
        self.diff_choice = diff;
        self.refresh_record_rows();
    }

    pub(super) fn toggle_filter(&mut self) {
        if self.view == View::Filter {
            self.set_view(View::Results);
        } else {
            self.finish_input();
            self.search_now();
            self.set_view(View::Filter);
            self.filter_sheet.focus = None;
        }
    }

    pub(super) fn filter_changed(&mut self, changed: bool) -> Action {
        if changed {
            self.filter_dirty = true;
            self.refresh_record_rows();
            Action::Choice
        } else {
            Action::None
        }
    }

    pub fn has_filter_changes(&self) -> bool {
        self.filter_dirty
    }

    pub fn take_filter_changed(&mut self) -> bool {
        std::mem::take(&mut self.filter_dirty)
    }

    pub(super) fn commit_rate(&mut self) -> bool {
        let changed = self.filter_sheet.commit_rate(&mut self.record_filter);
        self.filter_dirty |= changed;
        changed
    }

    pub(super) fn finish_filter_input(&mut self) {
        let changed = self.commit_rate();
        self.cancel_nickname();
        self.filter_changed(changed);
    }

    pub(super) fn cancel_nickname(&mut self) {
        let Some(saved) = self.nickname_edit.take() else {
            return;
        };
        self.preedit.clear();
        self.notify_composition(CPS_CANCEL);
        self.restore_saved_query(saved);
        // 치는 동안 켜진 키 포커스 표시를 풂, 안 풀면 입력을 끝낸 뒤에도 버튼이 눌린 채로 보임
        self.filter_keyed = false;
    }

    fn start_nickname(&mut self) {
        if self.nickname_edit.is_some() {
            return;
        }
        self.finish_input();
        self.nickname_edit = Some(SavedQuery {
            edit: std::mem::take(&mut self.edit),
            searched: std::mem::take(&mut self.searched),
            // 결과 목록은 그대로 두고 입력 칸만 빌려서 되돌릴 줄이 없음
            row: None,
        });
        self.clear_input();
        self.edit.insert(&self.records.nickname);
        self.edit.select_all();
        // 바가 입력 칸으로 바뀐 걸 알게 검색창 열 때의 빛을 한 바퀴 더 돌림
        if let Some(Err(e)) = self.beams.as_ref().map(|b| b.open.once(SPIN_SECS)) {
            eprintln!("beams: {e:?}");
        }
    }

    fn save_nickname(&mut self) -> Action {
        self.complete_composition();
        let nickname = self.query().trim().to_owned();
        self.cancel_nickname();
        self.records.set_nickname(&nickname);
        self.refresh_record_rows();
        Action::Nickname
    }

    pub(super) fn filter_control(&mut self, control: Control) -> Action {
        if !record_filter::FilterInput::enabled(control, &self.record_filter, &self.records) {
            return Action::None;
        }
        if control != Control::Nickname {
            self.cancel_nickname();
        }
        let committed = self.commit_rate();
        self.wheel_acc = 0;
        self.filter_sheet.focus = Some(control);
        let changed = match control {
            Control::Reset => self.record_filter.reset(),
            Control::Sort(sort) => {
                let (current, direction) = self.record_filter.effective_sort(&self.records);
                self.record_filter.sort = current;
                self.record_filter.direction = direction;
                self.record_filter.select_sort(sort);
                true
            }
            // 입력 중에 다시 누르면 취소
            Control::Nickname if self.nickname_edit.is_some() => {
                self.cancel_nickname();
                false
            }
            Control::Nickname => {
                self.start_nickname();
                false
            }
            Control::Refresh => {
                self.filter_changed(committed);
                return Action::RefreshRecords;
            }
            Control::Rate(bound) => {
                self.filter_sheet.begin_rate(bound, &self.record_filter);
                false
            }
            Control::Play(play) => {
                let changed = self.record_filter.play != play;
                self.record_filter.play = play;
                changed
            }
        };
        self.filter_changed(committed || changed)
    }

    pub(super) fn filter_char(&mut self, ch: char) {
        if let Some(Control::Rate(bound)) = self.filter_sheet.focus {
            if self.filter_sheet.rate_edit.is_none() {
                self.filter_sheet.begin_rate(bound, &self.record_filter);
            }
            self.filter_sheet.insert_rate(ch.encode_utf8(&mut [0; 4]));
        }
    }

    fn rate_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> Option<Action> {
        let field = self.filter_sheet.rate_edit.as_mut()?;
        if vk == VK_TAB {
            return None;
        }
        if ctrl && !shift {
            match vk.0 as u8 {
                b'A' => {
                    field.edit.select_all();
                    return Some(Action::None);
                }
                b'C' | b'X' => {
                    let editor = &mut field.edit;
                    if !editor.selected().is_empty() && sys::set_clipboard_text(self.hwnd, editor.selected()) && vk.0 as u8 == b'X' {
                        editor.delete_selection();
                    }
                    return Some(Action::None);
                }
                b'V' => {
                    if let Some(text) = sys::clipboard_text(self.hwnd) {
                        self.filter_sheet.insert_rate(text.trim());
                    }
                    return Some(Action::None);
                }
                _ => {}
            }
        }
        let action = match vk {
            VK_ESCAPE => {
                self.filter_sheet.rate_edit = None;
                Action::None
            }
            VK_RETURN => {
                let changed = self.commit_rate();
                self.filter_changed(changed)
            }
            VK_UP | VK_DOWN => {
                let bound = field.bound;
                let delta = if vk == VK_UP { 1 } else { -1 };
                let changed = self.filter_sheet.step_rate(bound, delta, &mut self.record_filter);
                self.filter_changed(changed)
            }
            _ => {
                match vk {
                    VK_LEFT => field.edit.left(ctrl, shift),
                    VK_RIGHT => field.edit.right(ctrl, shift),
                    VK_HOME => field.edit.home(shift),
                    VK_END => field.edit.end(shift),
                    VK_BACK => {
                        field.edit.backspace(ctrl);
                    }
                    VK_DELETE => {
                        field.edit.delete(ctrl);
                    }
                    _ => {}
                }
                Action::None
            }
        };
        Some(action)
    }

    pub(super) fn filter_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, alt: bool, shift: bool) -> Option<Action> {
        self.wheel_acc = 0;
        if self.nickname_edit.is_some() {
            return match vk {
                VK_ESCAPE => {
                    self.cancel_nickname();
                    Some(Action::None)
                }
                VK_RETURN => Some(self.save_nickname()),
                VK_TAB | VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT => Some(Action::None),
                _ if alt || matches!(vk, VK_F2 | VK_APPS | VK_F10) => Some(Action::None),
                _ => None,
            };
        }
        if alt {
            let changed = self.commit_rate();
            self.filter_changed(changed);
            return None;
        }
        if let Some(action) = self.rate_key(vk, ctrl, shift) {
            return Some(action);
        }
        let changed = self.commit_rate();
        let action = match vk {
            VK_ESCAPE => {
                self.set_view(View::Results);
                Action::None
            }
            VK_UP | VK_DOWN => {
                let delta = if vk == VK_UP { -1 } else { 1 };
                self.filter_sheet.move_row(delta, &self.record_filter, &self.records);
                Action::None
            }
            VK_TAB => {
                let direction = if shift { -1 } else { 1 };
                let next = match (self.filter_sheet.focus, shift) {
                    (Some(Control::Rate(Bound::Lower)), false) => Some(Control::Rate(Bound::Upper)),
                    (Some(Control::Rate(Bound::Upper)), true) => Some(Control::Rate(Bound::Lower)),
                    _ => None,
                };
                if let Some(next) = next {
                    self.filter_sheet.focus = Some(next);
                } else {
                    self.filter_sheet.move_row(direction, &self.record_filter, &self.records);
                }
                Action::None
            }
            VK_LEFT | VK_RIGHT => {
                let delta = if vk == VK_LEFT {
                    -1
                } else {
                    1
                };
                if let Some(control) = self.filter_sheet.adjacent(delta, &self.record_filter, &self.records) {
                    self.filter_sheet.focus = Some(control);
                    let (sort, _) = self.record_filter.effective_sort(&self.records);
                    match control {
                        Control::Sort(next) if next != sort => return Some(self.filter_control(control)),
                        Control::Play(play) if play != self.record_filter.play => return Some(self.filter_control(control)),
                        _ => {}
                    }
                }
                Action::None
            }
            VK_RETURN | VK_SPACE => {
                if let Some(control) = self.filter_sheet.focus {
                    return Some(self.filter_control(control));
                }
                Action::None
            }
            _ => Action::None,
        };
        if changed {
            Some(self.filter_changed(true))
        } else {
            Some(action)
        }
    }

    pub(super) fn filter_wheel(&mut self, delta: i32, x: f32, y: f32) -> Action {
        if self.nickname_edit.is_some() {
            return Action::None;
        }
        let Hit::FilterControl(Control::Rate(bound)) = self.hit(x, y) else {
            self.wheel_acc = 0;
            return Action::None;
        };
        let moved = self.filter_sheet.focus != Some(Control::Rate(bound));
        let committed = if moved {
            self.wheel_acc = 0;
            let changed = self.commit_rate();
            self.filter_sheet.focus = Some(Control::Rate(bound));
            self.filter_keyed = false;
            changed
        } else {
            false
        };
        let steps = self.wheel_steps(delta);
        let changed = steps != 0 && self.filter_sheet.step_rate(bound, steps, &mut self.record_filter);
        self.filter_changed(committed || changed)
    }
}

impl Popup {
    pub(super) fn filter_rows(&self) -> Vec<record_filter::SheetRow> {
        self.filter_sheet.rows(&self.record_filter, &self.records)
    }

    pub(super) fn filter_status(&self) -> String {
        self.records.status(self.mode_choice, varchive::now())
    }

    pub(super) fn filter_marked(&self) -> bool {
        self.records.connected() && self.record_filter.active()
    }

    pub(super) fn filter_empty_state(&self, sheet: Sheet) -> Option<FilterEmpty> {
        let empty = match sheet {
            Sheet::Results => self.filter_empty && self.results.is_empty() && self.playlist.is_none(),
            Sheet::History if self.history_tab == HistoryTab::Favorites => self.history.contains(&Row::Random)
                && picks(&self.history).next().is_some() && self.random_candidates(sheet).is_empty(),
            _ => false,
        };
        empty.then_some(FilterEmpty { text: "필터에 맞는 곡이 없어요", button: "필터 초기화", control: Control::Reset })
    }

    pub(super) fn row_record(&self, row: Row) -> Option<&varchive::Record> {
        let (song, mode, diff) = self.row_pattern(row)?;
        song.level(mode, diff)?;
        self.records.record(song.id, mode, diff)
    }

    pub(super) fn row_badge(&self, row: Row) -> Option<varchive::Badge> {
        self.row_record(row)?.badge()
    }

    pub(super) fn record_tail(&self, row: Row, bpm: bool) -> Option<(String, usize)> {
        let (song, _, _) = self.row_pattern(row)?;
        let mut tail = self.sub_tail(song, bpm).unwrap_or_default();
        let rate = self.row_record(row).map(|r| r.rate_text()).unwrap_or_default();
        if !rate.is_empty() {
            tail.push_str("  ·  ");
            tail.push_str(&rate);
        }
        (!tail.is_empty()).then_some((tail, rate.len()))
    }
}
