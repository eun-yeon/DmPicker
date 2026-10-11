//! 난이도·버튼 모드 도우미

use crate::keys::{ButtonMode, Key};

/// 난이도 커서를 NM에서 `target` 칸으로 옮기는 키 (0=NM, 1=HD, 2=MX, 3=SC)
/// 곡·모드를 바꾸면 항상 NM, Left/Right는 순환하고 없는 칸은 건너뜀
/// 짧은 쪽으로 감 (같으면 Right), `target`이 NM이거나 없는 칸이면 빈 목록
pub fn diff_keys(available: [bool; 4], target: usize) -> Vec<Key> {
    diff_keys_from(available, 0, target).unwrap_or_default()
}

/// 일반형: `current` 칸에서 `target` 칸으로
/// 둘 중 하나라도 없는 칸이거나 범위 밖이면 `None`
pub fn diff_keys_from(available: [bool; 4], current: usize, target: usize) -> Option<Vec<Key>> {
    if current >= 4 || target >= 4 || !available[current] || !available[target] {
        return None;
    }
    let slots: Vec<usize> = (0..4).filter(|&i| available[i]).collect();
    let m = slots.len();
    let ci = slots.iter().position(|&s| s == current)?;
    let ti = slots.iter().position(|&s| s == target)?;
    let right = (ti + m - ci) % m;
    let left = (m - right) % m;
    Some(if right <= left { vec![Key::Right; right] } else { vec![Key::Left; left] })
}

/// 버튼 모드를 바로 바꾸는 Ctrl+Numpad{4,5,6,8} 조합 키 표시
pub fn mode_key(mode: ButtonMode) -> Key {
    Key::Mode(mode)
}
