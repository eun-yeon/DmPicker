//! 키와 곡 목록 커서에 미치는 효과

use crate::model::{Group, ListModel};

/// 버튼 모드 (4B/5B/6B/8B)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ButtonMode {
    B4,
    B5,
    B6,
    B8,
}

impl ButtonMode {
    pub fn buttons(self) -> u8 {
        match self {
            ButtonMode::B4 => 4,
            ButtonMode::B5 => 5,
            ButtonMode::B6 => 6,
            ButtonMode::B8 => 8,
        }
    }
}

/// 이 crate가 입력 계층에 보내라고 하는 키 전부
/// 위험한 키 (Enter, Esc, F5, F7, F10, Space, 숫자, Alt, Tab)는 애초에 없음
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// 위 방향키: 이전 항목 (순환)
    Up,
    /// 아래 방향키: 다음 항목 (순환)
    Down,
    /// Page Up: 이전 그룹의 마지막 항목
    PgUp,
    /// Page Down: 다음 그룹의 첫 항목
    PgDn,
    /// 글자 키 `'A'..='Z'`: 그 그룹으로 점프, 이미 그룹 안이면 다음 항목
    Letter(char),
    /// 왼쪽 방향키: 이전 난이도 (있는 것만, 순환)
    Left,
    /// 오른쪽 방향키: 다음 난이도 (있는 것만, 순환)
    Right,
    /// Ctrl+Numpad{4,5,6,8}: 버튼 모드 바로 설정 (입력 계층에 조합 키라고 알리는 표시)
    Mode(ButtonMode),
}

impl Key {
    /// 커서를 멀리 옮기는 키 (글자, 페이지)
    /// 스크롤 애니메이션이 길고, 누락되면 커서가 크게 어긋남
    pub fn is_jump(self) -> bool {
        matches!(self, Key::Letter(_) | Key::PgUp | Key::PgDn)
    }
    /// 곡 목록에 작용하는 키 (난이도·모드 키 말고)
    pub fn is_list_key(self) -> bool {
        matches!(self, Key::Up | Key::Down | Key::PgUp | Key::PgDn | Key::Letter(_))
    }
}

/// `model`의 커서 위치 `pos`에 키 한 번을 적용 (순수 함수)
///
/// * Up/Down: -1/+1, 순환 (RANDOM, LOCKED 줄도 보통 줄)
/// * `Letter(L)`: L 그룹 밖이면 L의 첫 항목, 안이면 L의 다음 항목, 마지막 다음은 다시 첫 항목
///   L 그룹이 없으면 그대로
/// * PgDn: 다음 그룹의 첫 항목, RANDOM으로는 절대 안 감 (Z 다음은 Kanji)
/// * PgUp: 이전 그룹의 마지막 항목, RANDOM으로는 절대 안 감 (Kanji 이전은 Z의 마지막)
/// * Left/Right/Mode: 목록 커서는 안 움직임
pub fn step(model: &ListModel, pos: usize, key: Key) -> usize {
    let n = model.len();
    debug_assert!(pos < n);
    match key {
        Key::Up => (pos + n - 1) % n,
        Key::Down => (pos + 1) % n,
        Key::Letter(c) => {
            let Some(ri) = model.letter_run_index(c) else { return pos };
            let run = &model.runs()[ri];
            if model.run_index(pos) == ri {
                let off = run.offset_of(pos, n).expect("pos in its own run");
                if off + 1 < run.len {
                    (pos + 1) % n
                } else {
                    run.start
                }
            } else {
                run.start
            }
        }
        Key::PgDn => match neighbour_run(model, pos, true) {
            Some(r) => model.runs()[r].start,
            None => pos,
        },
        Key::PgUp => match neighbour_run(model, pos, false) {
            Some(r) => model.runs()[r].last(n),
            None => pos,
        },
        Key::Left | Key::Right | Key::Mode(_) => pos,
    }
}

/// 순환 순서로 다음/이전 구간, RANDOM은 건너뜀
fn neighbour_run(model: &ListModel, pos: usize, forward: bool) -> Option<usize> {
    let runs = model.runs();
    let k = runs.len();
    let mut r = model.run_index(pos);
    for _ in 0..k {
        r = if forward { (r + 1) % k } else { (r + k - 1) % k };
        if runs[r].group != Group::Random {
            return Some(r);
        }
    }
    None
}
