//! 온라인 방에서 방장인지 봄
//! 방장만 목록을 움직일 수 있고 목록 위 정렬 줄과 Shift·F4 키 표시도 방장에게만 보임
//! 키 표시는 불투명이라 곡 배경과 상관없이 같은 색, 그중 F4 하나만 봄
//! 오른쪽 PLAYERS 칸의 YOU 표시는 이모티콘에 가려지고 다른 자리 이모티콘의 분홍과도 섞여서 안 씀

use crate::geom::find_list;
use crate::img::Rgb;
use crate::state::{layout_kind, ScreenKind, MIN_ROWS, ONLINE_BAR_X};

/// F4 키 표시 (2560x1440 디자인 배치), 키 표시는 x 1518..=1548, y 124..=155
/// 줄여 그릴 때 테두리가 배경과 섞여서 안쪽만 봄
const F4_X0: f32 = 1520.0;
const F4_X1: f32 = 1547.0;
const F4_Y0: f32 = 126.0;
const F4_Y1: f32 = 154.0;
/// 글자와 줄여 그린 경계가 섞여도 키 바탕을 알아보도록 절반을 기준으로 둠
const MIN_LIGHT: f32 = 0.5;

/// 키 표시 바탕의 밝은 회색
fn light(p: [u8; 3]) -> bool {
    let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
    lo >= 170 && hi - lo <= 30
}

/// 온라인 방인데 F4 키 표시가 없음 (방장 아님)
/// 온라인 방이 아니면 false
/// 손님 화면 배경이 그 자리에서 우연히 밝은 회색이면 방장으로 읽음, 그땐 키가 무시될 뿐이라 둠
pub fn not_host(img: &Rgb) -> bool {
    let Some(geom) = find_list(img).filter(|g| g.rows_seen >= MIN_ROWS) else { return false };
    if layout_kind(&geom) != ScreenKind::OnlineList {
        return false;
    }
    // 목록 막대에서 디자인 배치의 왼쪽 끝을 구함, 21:9처럼 판이 가운데로 밀려도 맞음
    let s = geom.scale();
    let left = geom.bar_x as f32 - ONLINE_BAR_X * s;
    let px = |v: f32| (left + v * s).round().max(0.0) as usize;
    let py = |v: f32| (v * s).round().max(0.0) as usize;
    let (x0, x1) = (px(F4_X0), px(F4_X1).min(img.width));
    let (y0, y1) = (py(F4_Y0), py(F4_Y1).min(img.height));
    let total = x1.saturating_sub(x0) * y1.saturating_sub(y0);
    let lit = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| light(img.pixel(x, y))).count();
    (lit as f32) < MIN_LIGHT * total as f32
}
