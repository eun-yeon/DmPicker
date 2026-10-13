//! 온라인 방에서 방장인지 봄
//! 방장만 목록을 움직일 수 있고 목록 위 정렬 줄과 Shift·F4 키 표시도 방장에게만 보임
//! 키 표시는 불투명이라 곡 배경과 상관없이 같은 모양, 그중 F4 하나만 봄
//! 오른쪽 PLAYERS 칸의 YOU 표시는 이모티콘에 가려지고 다른 자리 이모티콘의 분홍과도 섞여서 안 씀

use std::sync::OnceLock;

use crate::geom::find_list;
use crate::img::{luma_feature, Rect, Rgb};
use crate::state::{layout_kind, ScreenKind, MIN_ROWS, ONLINE_BAR_X};

/// F4 키 표시 안쪽, 1440p 방장 화면에서 아래 상자 위치대로 잘라 둠
static F4_PNG: &[u8] = include_bytes!("../assets/f4_key.png");

/// F4 키 표시 (2560x1440 디자인 배치), 키 표시는 x 1518..=1548, y 124..=155
/// 줄여 그릴 때 테두리가 배경과 섞여서 안쪽만 봄
const F4_X: f32 = 1520.0;
const F4_Y: f32 = 126.0;
const F4_W: f32 = 27.0;
const F4_H: f32 = 28.0;
/// 글자와 줄여 그린 경계가 섞여도 키 바탕을 알아보도록 절반을 기준으로 둠
const MIN_LIGHT: f32 = 0.5;
/// 손님 화면 배경이 밝은 회색일 수 있어서 F4 글자 모양까지 맞춰 봄
/// 가장 작은 해상도에선 칸이 몇 픽셀이라 방장도 점수가 낮아져서 넉넉히 둠
const MIN_MATCH: f32 = 0.5;
const THUMB: usize = 16;

/// 키 표시 바탕의 밝은 회색
fn light(p: [u8; 3]) -> bool {
    let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
    lo >= 170 && hi - lo <= 30
}

fn template() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        let im = Rgb::decode(F4_PNG).expect("built-in F4 key");
        luma_feature(im.view(), THUMB, THUMB)
    })
}

/// F4 칸의 밝은 회색 비율과 견본 상관계수, 온라인 방 목록이 아니면 None
fn f4_scores(img: &Rgb) -> Option<(f32, f32)> {
    let geom = find_list(img).filter(|g| g.rows_seen >= MIN_ROWS)?;
    if layout_kind(&geom) != ScreenKind::OnlineList {
        return None;
    }
    // 목록 막대에서 디자인 배치의 왼쪽 끝을 구함, 21:9처럼 판이 가운데로 밀려도 맞음
    let s = geom.scale();
    let left = geom.bar_x as f32 - ONLINE_BAR_X * s;
    let r = Rect::new(
        (left + F4_X * s).round() as i32,
        (F4_Y * s).round() as i32,
        (F4_W * s).round().max(1.0) as i32,
        (F4_H * s).round().max(1.0) as i32,
    );
    let r = r.intersect(&img.rect()).filter(|c| *c == r)?;
    let v = img.sub(r);
    let lit = (0..v.height).flat_map(|y| v.row(y).chunks_exact(3)).filter(|p| light([p[0], p[1], p[2]])).count();
    let lit = lit as f32 / (v.width * v.height) as f32;
    let m = luma_feature(v, THUMB, THUMB).iter().zip(template()).map(|(a, b)| a * b).sum();
    Some((lit, m))
}

/// 온라인 방인데 F4 키 표시가 안 보임 (방장 아님)
/// 온라인 방이 아니면 false
pub fn not_host(img: &Rgb) -> bool {
    f4_scores(img).is_some_and(|(lit, m)| lit < MIN_LIGHT || m < MIN_MATCH)
}
