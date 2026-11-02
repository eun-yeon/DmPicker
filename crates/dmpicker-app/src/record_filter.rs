//! 기록 조건·정렬과 필터 시트의 입력 상태

use std::cmp::Ordering;

use dmpicker_search::{Diff, Mode, Song};
use serde_json::{json, Value};

use crate::edit::Editor;
use crate::varchive::{valid_rate, Cache};

const RATE_STEPS: [f64; 9] = [90.0, 95.0, 97.0, 98.0, 99.0, 99.5, 99.8, 99.9, 100.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Title,
    Level,
    Floor,
    Bpm,
    Rate,
}

impl Sort {
    pub const ALL: [Self; 5] = [Self::Title, Self::Level, Self::Floor, Self::Bpm, Self::Rate];

    pub fn label(self) -> &'static str {
        ["제목", "레벨", "서열", "BPM", "레이트"][self as usize]
    }

    fn key(self) -> &'static str {
        ["title", "level", "floor", "bpm", "rate"][self as usize]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Ascending,
    Descending,
}

impl Direction {
    fn apply(self, order: Ordering) -> Ordering {
        if self == Self::Descending { order.reverse() } else { order }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Play {
    #[default]
    All,
    Missing,
    NoMaxCombo,
    NoPerfect,
}

impl Play {
    pub const ALL: [Self; 4] = [Self::All, Self::Missing, Self::NoMaxCombo, Self::NoPerfect];

    pub fn label(self) -> &'static str {
        ["전체", "기록 없음", "MAX COMBO 아님", "PERFECT 아님"][self as usize]
    }

    fn key(self) -> &'static str {
        ["all", "missing", "no_max_combo", "no_perfect"][self as usize]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Options {
    pub sort: Sort,
    pub direction: Direction,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub play: Play,
}

pub type PatternRef<'a> = (&'a Song, Mode, Diff);

impl Options {
    pub fn from_json(value: &Value) -> Self {
        let mut options = Self {
            sort: Sort::ALL.into_iter().find(|s| value["sort"].as_str() == Some(s.key())).unwrap_or_default(),
            direction: if value["direction"].as_str() == Some("descending") {
                Direction::Descending
            } else {
                Direction::Ascending
            },
            lower: value["lower"].as_f64().filter(|s| valid_rate(*s)),
            upper: value["upper"].as_f64().filter(|s| valid_rate(*s)),
            play: Play::ALL.into_iter().find(|s| value["play"].as_str() == Some(s.key())).unwrap_or_default(),
        };
        options.normalize();
        options
    }

    pub fn json(&self) -> Value {
        let direction = if self.direction == Direction::Ascending {
            "ascending"
        } else {
            "descending"
        };
        json!({
            "sort": self.sort.key(), "direction": direction,
            "lower": self.lower, "upper": self.upper, "play": self.play.key(),
        })
    }

    pub fn active(&self) -> bool {
        self.lower.is_some() || self.upper.is_some() || self.play != Play::All
    }

    pub fn reset(&mut self) -> bool {
        let changed = self.active();
        self.lower = None;
        self.upper = None;
        self.play = Play::All;
        changed
    }

    pub fn effective_sort(&self, cache: &Cache) -> (Sort, Direction) {
        if self.sort == Sort::Rate && !cache.connected() {
            (Sort::Title, Direction::Ascending)
        } else {
            (self.sort, self.direction)
        }
    }

    pub fn select_sort(&mut self, sort: Sort) {
        self.direction = if self.sort == sort && self.direction == Direction::Ascending {
            Direction::Descending
        } else {
            Direction::Ascending
        };
        self.sort = sort;
    }

    pub fn bound(&self, bound: Bound) -> Option<f64> {
        match bound {
            Bound::Lower => self.lower,
            Bound::Upper => self.upper,
        }
    }

    pub fn set_bound(&mut self, bound: Bound, value: Option<f64>) -> bool {
        let before = *self;
        let value = value.filter(|s| valid_rate(*s));
        match bound {
            Bound::Lower => self.lower = value,
            Bound::Upper => self.upper = value,
        }
        self.normalize();
        before != *self
    }

    pub fn step_rate(&mut self, bound: Bound, delta: i32) -> bool {
        let before = *self;
        for _ in 0..delta.unsigned_abs().min(RATE_STEPS.len() as u32) {
            let value = self.bound(bound);
            let next = if delta > 0 {
                RATE_STEPS.into_iter().find(|s| value.is_none_or(|v| *s > v)).unwrap_or(100.0)
            } else {
                RATE_STEPS.into_iter().rev().find(|s| value.is_none_or(|v| *s < v)).unwrap_or(90.0)
            };
            self.set_bound(bound, Some(next));
        }
        before != *self
    }

    fn normalize(&mut self) {
        if let (Some(lower), Some(upper)) = (self.lower, self.upper) {
            if lower > upper {
                self.upper = Some(lower);
            }
        }
    }

    pub fn allows(&self, (song, mode, diff): PatternRef<'_>, cache: &Cache) -> bool {
        // 아직 못 받은 모드는 기록 없음으로 판정하지 않음
        if !self.active() || !cache.known(mode) {
            return true;
        }
        if song.level(mode, diff).is_none() {
            return false;
        }
        let record = cache.record(song.id, mode, diff);
        if (self.lower.is_some() || self.upper.is_some()) && !record.is_some_and(|r| {
            self.lower.is_none_or(|lo| r.score >= lo) && self.upper.is_none_or(|hi| r.score <= hi)
        }) {
            return false;
        }
        match self.play {
            Play::All => true,
            Play::Missing => record.is_none(),
            Play::NoMaxCombo => record.is_none_or(|r| !r.max_combo),
            Play::NoPerfect => record.is_none_or(|r| r.score < 100.0),
        }
    }

    pub fn compare(&self, a: PatternRef<'_>, b: PatternRef<'_>, cache: &Cache) -> Ordering {
        let (sort, direction) = self.effective_sort(cache);
        let title = || title_order(a.0, b.0);
        let number = |pattern: PatternRef<'_>| -> Option<(f64, f64)> {
            let (song, mode, diff) = pattern;
            match sort {
                Sort::Title => None,
                Sort::Level => song.level(mode, diff).filter(|&l| l != 0).map(|l| (l as f64, 0.0)),
                Sort::Floor => song.floor(mode, diff).filter(|f| f.is_finite()).map(|f| (f as f64, 0.0)),
                Sort::Bpm => song.bpm_range.filter(|(lo, hi)| lo.is_finite() && hi.is_finite()).map(|(lo, hi)| (hi as f64, lo as f64)),
                Sort::Rate => song.level(mode, diff).and_then(|_| cache.record(song.id, mode, diff)).map(|r| (r.score, 0.0)),
            }
        };
        let primary = if sort == Sort::Title {
            direction.apply(title())
        } else {
            match (number(a), number(b)) {
                (Some(a), Some(b)) => direction.apply(a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1))),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        };
        primary.then_with(title).then_with(|| a.2.cmp(&b.2))
    }
}

fn title_order(a: &Song, b: &Song) -> Ordering {
    dmpicker_nav::order_key(&a.name).cmp(&dmpicker_nav::order_key(&b.name)).then_with(|| a.name.cmp(&b.name))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    Lower,
    Upper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Reset,
    Sort(Sort),
    Nickname,
    Refresh,
    Rate(Bound),
    Play(Play),
}

impl Control {
    pub fn row(self) -> usize {
        match self {
            Self::Reset => 0,
            Self::Sort(_) => 1,
            Self::Nickname | Self::Refresh => 2,
            Self::Rate(_) => 3,
            Self::Play(_) => 4,
        }
    }
}

pub struct Choice {
    pub control: Control,
    pub text: String,
    enabled: bool,
    selected: bool,
}

pub struct SheetRow {
    pub label: &'static str,
    pub choices: Vec<Choice>,
}

#[derive(Default)]
pub struct FilterInput {
    pub focus: Option<Control>,
    pub rate_edit: Option<RateEdit>,
}

pub struct RateEdit {
    pub bound: Bound,
    pub edit: Editor,
}

impl FilterInput {
    pub fn enabled(control: Control, options: &Options, cache: &Cache) -> bool {
        match control {
            Control::Reset => options.active(),
            Control::Refresh => cache.connected() && !cache.loading,
            Control::Sort(Sort::Rate) | Control::Rate(_) | Control::Play(_) => cache.connected(),
            _ => true,
        }
    }

    pub fn rows(&self, options: &Options, cache: &Cache) -> Vec<SheetRow> {
        let choice = |control, text: String, selected| Choice { control, text, selected, enabled: Self::enabled(control, options, cache) };
        let nickname = if cache.connected() {
            cache.nickname.clone()
        } else {
            "기록 연동".into()
        };
        let mut archive = vec![choice(Control::Nickname, nickname, false)];
        if cache.connected() {
            archive.push(choice(Control::Refresh, "새로고침".into(), false));
        }
        let (sort, _) = options.effective_sort(cache);
        vec![
            SheetRow { label: "필터", choices: vec![choice(Control::Reset, "초기화".into(), false)] },
            SheetRow { label: "정렬", choices: Sort::ALL.map(|s| choice(Control::Sort(s), s.label().into(), s == sort)).into() },
            SheetRow { label: "V-Archive 기록", choices: archive },
            SheetRow { label: "레이트", choices: [Bound::Lower, Bound::Upper].map(|b| choice(Control::Rate(b), self.rate_text(b, options), false)).into() },
            SheetRow { label: "플레이", choices: Play::ALL.map(|p| choice(Control::Play(p), p.label().into(), p == options.play)).into() },
        ]
    }

    pub fn rate_text(&self, bound: Bound, options: &Options) -> String {
        if let Some(edit) = self.rate_edit.as_ref().filter(|e| e.bound == bound) {
            edit.edit.text().to_owned()
        } else {
            options.bound(bound).map(|n| n.to_string()).unwrap_or_default()
        }
    }

    pub fn begin_rate(&mut self, bound: Bound, options: &Options) {
        let mut edit = Editor::default();
        edit.insert(&options.bound(bound).map(|n| n.to_string()).unwrap_or_default());
        edit.select_all();
        self.focus = Some(Control::Rate(bound));
        self.rate_edit = Some(RateEdit { bound, edit });
    }

    pub fn commit_rate(&mut self, options: &mut Options) -> bool {
        let Some(edit) = self.rate_edit.take() else {
            return false;
        };
        let text = edit.edit.text().trim();
        if text.is_empty() {
            return options.set_bound(edit.bound, None);
        }
        let Some(value) = text.parse::<f64>().ok().filter(|r| valid_rate(*r)) else {
            return false;
        };
        options.set_bound(edit.bound, Some(value))
    }

    pub fn insert_rate(&mut self, text: &str) {
        if !text.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return;
        }
        let Some(field) = &mut self.rate_edit else {
            return;
        };
        let old = field.edit.text();
        let (a, b) = field.edit.selection().unwrap_or((field.edit.caret(), field.edit.caret()));
        let candidate = format!("{}{text}{}", &old[..a], &old[b..]);
        if rate_draft(&candidate) {
            field.edit.insert(text);
        }
    }

    pub fn step_rate(&mut self, bound: Bound, delta: i32, options: &mut Options) -> bool {
        if let Some(field) = &self.rate_edit {
            let mut draft = *options;
            let text = field.edit.text().trim();
            if text.is_empty() {
                draft.set_bound(bound, None);
            } else if let Some(value) = text.parse::<f64>().ok().filter(|r| valid_rate(*r)) {
                draft.set_bound(bound, Some(value));
            }
            draft.step_rate(bound, delta);
            self.begin_rate(bound, &draft);
            return false;
        }
        options.step_rate(bound, delta)
    }

    pub fn move_row(&mut self, delta: i32, options: &Options, cache: &Cache) {
        let rows = self.rows(options, cache);
        let start = match self.focus {
            Some(control) => control.row() as i32,
            None if delta > 0 => Control::Reset.row() as i32,
            None => rows.len() as i32,
        };
        let mut at = start + delta.signum();
        while (0..rows.len() as i32).contains(&at) {
            let row = &rows[at as usize];
            if let Some(choice) = row.choices.iter().find(|c| c.enabled && c.selected).or_else(|| row.choices.iter().find(|c| c.enabled)) {
                self.focus = Some(choice.control);
                break;
            }
            at += delta.signum();
        }
    }

    pub fn adjacent(&self, delta: i32, options: &Options, cache: &Cache) -> Option<Control> {
        let focus = self.focus?;
        let rows = self.rows(options, cache);
        let choices: Vec<_> = rows[focus.row()].choices.iter().filter(|c| c.enabled).map(|c| c.control).collect();
        let at = choices.iter().position(|&c| c == focus)? as i32;
        choices.get((at + delta).rem_euclid(choices.len() as i32) as usize).copied()
    }
}

/// 치는 중인 레이트 칸 글이 아직 레이트가 될 수 있음, 정수 세 자리·소수 두 자리까지에 100 이하
/// 넘는 글자는 안 받아서 숫자 칸 폭 56 밖으로 안 넘침
fn rate_draft(text: &str) -> bool {
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    int.len() <= 3 && frac.len() <= 2 && !frac.contains('.') && (text.is_empty() || text == "." || text.parse::<f64>().is_ok_and(|v| v <= 100.0))
}
