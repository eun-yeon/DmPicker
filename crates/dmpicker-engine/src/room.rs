//! 온라인 방 오른쪽 PLAYERS 칸
//! 방장만 목록을 움직일 수 있고 방장 자리는 늘 맨 위로 올라감
//! 그래서 내 자리 왼쪽에 붙는 분홍 YOU 표시가 맨 위인지만 봄 (레디해도 표시는 그대로)
//! 다른 자리 이모티콘의 분홍이 섞이면 평균 높이가 내려가서 분홍이 표시 하나로 모여 있을 때만 판단함

use crate::geom::find_list;
use crate::img::Rgb;
use crate::state::{layout_kind, ScreenKind, MIN_ROWS, ONLINE_BAR_X};

/// YOU 표시를 찾는 세로 띠 (2560x1440 디자인 배치), 표시는 x 1748..1794에 있음
const TAG_X0: f32 = 1735.0;
const TAG_X1: f32 = 1810.0;
const SEATS_Y0: f32 = 280.0;
const SEATS_Y1: f32 = 1100.0;
/// 표시 가운데 y가 맨 위 자리 333, 두 번째 자리 467
const TOP_SEAT_MAX_Y: f32 = 400.0;
/// 표시 안 분홍 픽셀이 1440p에서 ~800개, 4분의 1도 안 보이면 못 찾은 걸로 봄
const MIN_PINK: f32 = 200.0;
/// 표시 높이 27 (1440p), 이모티콘이 덮기 시작할 때 걸친 분홍까지 38
/// 이모티콘 하나는 ~125라 이보다 넓게 퍼지면 표시 하나로 안 봄
const TAG_SPAN: f32 = 45.0;

/// YOU 표시 바탕의 분홍, 평균 (251, 7, 86)
fn pink(p: [u8; 3]) -> bool {
    p[0] >= 190 && p[1] <= 100 && (40..=170).contains(&p[2])
}

/// 온라인 방에서 내 자리가 맨 위가 아님 (방장 아님)
/// 온라인 방이 아니거나 YOU 표시 하나만 또렷이 안 보이면 false, 못 읽었다고 막지는 않음
pub fn guest_seat(img: &Rgb) -> bool {
    let Some(geom) = find_list(img).filter(|g| g.rows_seen >= MIN_ROWS) else { return false };
    if layout_kind(&geom) != ScreenKind::OnlineList {
        return false;
    }
    // 목록 막대에서 디자인 배치의 왼쪽 끝을 구함, 21:9처럼 판이 가운데로 밀려도 맞음
    let s = geom.scale();
    let left = geom.bar_x as f32 - ONLINE_BAR_X * s;
    let px = |v: f32| (left + v * s).round().max(0.0) as usize;
    let py = |v: f32| (v * s).round().max(0.0) as usize;
    let (x0, x1) = (px(TAG_X0), px(TAG_X1).min(img.width));
    let (y0, y1) = (py(SEATS_Y0), py(SEATS_Y1).min(img.height));
    let (mut n, mut sum_y, mut top, mut bottom) = (0usize, 0usize, usize::MAX, 0usize);
    for y in y0..y1 {
        for x in x0..x1 {
            if pink(img.pixel(x, y)) {
                n += 1;
                sum_y += y;
                top = top.min(y);
                bottom = y;
            }
        }
    }
    if (n as f32) < MIN_PINK * s * s || (bottom - top) as f32 > TAG_SPAN * s {
        return false;
    }
    sum_y as f32 / n as f32 / s > TOP_SEAT_MAX_Y
}
