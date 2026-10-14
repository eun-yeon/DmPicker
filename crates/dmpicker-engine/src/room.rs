//! 온라인 방에서 방장인지, 채팅 입력창이 열렸는지 봄
//! 방장만 목록을 움직일 수 있고 목록 위 정렬 줄과 Shift·F4 키 표시도 방장에게만 보임
//! 키 표시는 불투명이라 곡 배경과 상관없이 같은 모양, 그중 F4 하나만 봄
//! 오른쪽 PLAYERS 칸의 YOU 표시는 이모티콘에 가려지고 다른 자리 이모티콘의 분홍과도 섞여서 안 씀

use std::sync::OnceLock;

use crate::features::dot;
use crate::geom::{find_list, ListGeom};
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
    let m = dot(&luma_feature(v, THUMB, THUMB), template());
    Some((lit, m))
}

/// 채팅 입력창 (2560x1440 디자인 배치), 열면 두께 10 px 불투명 테두리가 생김
/// 왼쪽 노랑 (255,190,0)에서 오른쪽 주황 (255,141,72)으로 바뀜, 닫히면 테두리 없는 어두운 상자
const CHAT_X: (f32, f32) = (70.0, 783.0);
const CHAT_Y: (f32, f32) = (1188.0, 1264.0);
/// 테두리 띠 중 이만큼이 노랑·주황이면 열린 것 (열린 화면은 거의 전부, 닫힌 화면은 0)
const CHAT_MIN: f32 = 0.7;

fn chat_border(p: [u8; 3]) -> bool {
    let [r, g, b] = p.map(i32::from);
    r >= 200 && (100..=215).contains(&g) && b <= 110 && r - b >= 120
}

/// 온라인 방 채팅 입력창이 열려 있음, 그러면 방향키는 글자 칸을 움직이고 글자 키는 입력됨
/// `geom`은 이 프레임의 목록, 온라인 방이 아니면 false
pub fn chat_open_at(img: &Rgb, geom: &ListGeom) -> bool {
    if layout_kind(geom) != ScreenKind::OnlineList {
        return false;
    }
    let s = geom.scale();
    let left = geom.bar_x as f32 - ONLINE_BAR_X * s;
    let px = |x: f32| (left + x * s).round() as i32;
    let py = |y: f32| (y * s).round() as i32;
    // 위 테두리와 왼쪽 테두리의 가운데 줄, 모서리는 뺌
    let (y, x) = (py(CHAT_Y.0 + 5.0), px(CHAT_X.0 + 5.0));
    let top: Vec<(i32, i32)> = (px(CHAT_X.0 + 20.0)..px(CHAT_X.1 - 20.0)).map(|x| (x, y)).collect();
    let side: Vec<(i32, i32)> = (py(CHAT_Y.0 + 15.0)..py(CHAT_Y.1 - 15.0)).map(|y| (x, y)).collect();
    let share = |pts: &[(i32, i32)]| {
        let inside = |&&(x, y): &&(i32, i32)| x >= 0 && y >= 0 && (x as usize) < img.width && (y as usize) < img.height;
        let hits = pts.iter().filter(inside).filter(|&&(x, y)| chat_border(img.pixel(x as usize, y as usize))).count();
        hits as f32 / pts.len().max(1) as f32
    };
    share(&top) >= CHAT_MIN && share(&side) >= CHAT_MIN
}

/// [`chat_open_at`], 목록을 찾아서 봄
pub fn chat_open(img: &Rgb) -> bool {
    find_list(img).filter(|g| g.rows_seen >= MIN_ROWS).is_some_and(|g| chat_open_at(img, &g))
}

/// 온라인 방인데 F4 키 표시가 안 보임 (방장 아님)
/// 온라인 방이 아니면 false
pub fn not_host(img: &Rgb) -> bool {
    f4_scores(img).is_some_and(|(lit, m)| lit < MIN_LIGHT || m < MIN_MATCH)
}
