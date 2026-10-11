//! 왼쪽 패널의 난이도 칸 (NORMAL / HARD / MAXIMUM / SC)
//!
//! 위치는 칸 테두리로 찾지 않음
//! 팩마다 스킨이 다름 (두 색 테두리, 줄무늬, 그라데이션, 난이도별 색, 대비 낮은 어두운 빨강)
//! 대신 모드별 배치에서 가져옴, 클라이언트 높이에 맞춰 커짐 (1440p 기준)
//! 칸 폭 152 px, 간격 160 px
//! 프리스타일 x0 = 128, y0 = 648, 높이 41 / 온라인 x0 = 111, y0 = 621, 높이 42
//! 배치는 곡 목록이 밀린 만큼 같이 가로로 밀림 ([`slots_for`])
//!
//! 칸 상태는 스킨과 무관한 값 두 개로 판단 ([`slot_metrics`])
//! * `fill`: 위·아래 테두리 바로 안쪽 글자 없는 띠와 칸 바로 바깥 배경의 색 차이
//!   채워진(선택된) 칸은 배경을 가리고 테두리만 있는 칸은 배경이 비침
//!   선택된 칸만 크게 나옴 ([`FILL_SELECTED`], [`FILL_MIN`])
//! * `border`: 위·아래 테두리 선과 바로 바깥 배경의 대비
//!   칸이 있으면 크게 나옴 ([`BORDER_MIN`])
//!
//! [`slot_states`]는 한 프레임의 네 칸을 같이 보고 판단
//! `fill`이 압도적으로 높은 칸 하나가 선택, 나머지는 테두리가 보이면 선택 가능, 아니면 없음
//!
//! [`button_mode`]는 패널 위 N BUTTON TUNES 배너 색으로 버튼 모드를 읽음, 위치는 칸처럼 잡음

use crate::geom::{find_list, ListGeom};
use crate::img::{Rect, Rgb, RgbView};
use crate::state::{layout_kind, ScreenKind, FREESTYLE_BAR_X, MIN_ROWS, ONLINE_BAR_X};
use crate::REF_H;

/// 칸 하나만 볼 때 선택으로 치는 `fill` 점수 ([`slot_state`])
pub const FILL_SELECTED: f32 = 110.0;
/// 프레임에서 `fill`이 가장 높은 칸을 선택으로 치려면 넘어야 하는 점수 ([`slot_states`])
pub const FILL_MIN: f32 = 80.0;
/// 칸이 있다고 치는 테두리 대비
pub const BORDER_MIN: f32 = 60.0;

/// 난이도, 칸 순서대로
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Difficulty {
    Normal,
    Hard,
    Maximum,
    SC,
}

impl Difficulty {
    pub const ALL: [Difficulty; 4] = [
        Difficulty::Normal,
        Difficulty::Hard,
        Difficulty::Maximum,
        Difficulty::SC,
    ];

    /// djmax.gg 패턴 키 (NM, HD, MX, SC)
    pub const fn code(self) -> &'static str {
        match self {
            Difficulty::Normal => "NM",
            Difficulty::Hard => "HD",
            Difficulty::Maximum => "MX",
            Difficulty::SC => "SC",
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// 난이도 칸 하나의 상태
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotState {
    /// 채워진 칸, 지금 난이도
    Selected,
    /// 테두리만 있음, 고를 수 있음
    Available,
    /// 칸 없음, 이 버튼 모드엔 채보 없음
    Missing,
}

/// 2560x1440 기준 모드별 배치 (0번 칸 x, 위쪽 y, 칸 높이, 목록 막대 x)
const FREESTYLE_LAYOUT: (f32, f32, f32, f32) = (128.0, 648.0, 41.0, FREESTYLE_BAR_X);
const ONLINE_LAYOUT: (f32, f32, f32, f32) = (111.0, 621.0, 42.0, ONLINE_BAR_X);

fn layout_slots(h: u32, (x0, y0, bh, _): (f32, f32, f32, f32), dx: f32) -> [Rect; 4] {
    let s = h as f32 / REF_H;
    std::array::from_fn(|i| {
        Rect::new(
            ((x0 + 160.0 * i as f32) * s + dx).round() as i32,
            (y0 * s).round() as i32,
            (152.0 * s).round() as i32,
            (bh * s).round() as i32,
        )
    })
}

/// 높이 `h`인 16:9 클라이언트의 온라인 칸 기본 위치
pub fn online_slots(h: u32) -> [Rect; 4] {
    layout_slots(h, ONLINE_LAYOUT, 0.0)
}

/// 막대가 `design_bar_x`에 있는 배치에 비해 목록이 가로로 밀린 정도 (16:9면 0)
/// 왼쪽 패널도 같이 밀림
fn panel_dx(geom: &ListGeom, design_bar_x: f32) -> f32 {
    let s = geom.scale();
    let dx = geom.bar_x as f32 - design_bar_x * s;
    if dx.abs() <= 4.0 * s { 0.0 } else { dx }
}

/// 해당 종류 목록 화면의 칸 위치, 목록이 기본 위치에서 밀린 만큼 가로로 옮김 (16:9면 0)
/// [`ScreenKind::Other`]면 `None`
pub fn slots_for(kind: ScreenKind, geom: &ListGeom) -> Option<[Rect; 4]> {
    let layout = match kind {
        ScreenKind::FreestyleList => FREESTYLE_LAYOUT,
        ScreenKind::OnlineList => ONLINE_LAYOUT,
        ScreenKind::Other => return None,
    };
    Some(layout_slots(geom.height, layout, panel_dx(geom, layout.3)))
}

/// 2560x1440 기준 왼쪽 패널 위 N BUTTON TUNES 배너 (x, 위쪽 y, 폭, 높이)
const FREESTYLE_BANNER: (f32, f32, f32, f32) = (111.0, 171.0, 684.0, 120.0);
const ONLINE_BANNER: (f32, f32, f32, f32) = (81.0, 171.0, 690.0, 120.0);
/// 배너에서 N BUTTON TUNES 글자와 Tab 배지 사이 그림의 버튼 모드별 평균 색
/// 초록(4B), 하늘색(5B), 주황(6B), 보라(8B)
/// 그림은 모든 팩, 프리스타일·온라인 둘 다 같음
const BANNER_COLORS: [(u8, [f32; 3]); 4] = [
    (4, [28.0, 121.0, 104.0]),
    (5, [54.0, 78.0, 97.0]),
    (6, [148.0, 77.0, 21.0]),
    (8, [52.0, 50.0, 119.0]),
];
/// 가장 가까운 두 색(5B와 8B) 거리의 절반
/// 네 색 모두와 이보다 멀면 배너를 못 읽은 걸로 침
const BANNER_MAX_DIST: f32 = 18.0;

/// 목록 화면의 버튼 모드(4, 5, 6, 8), 왼쪽 패널 위 배너 색으로 판단
/// 배너 색이 넷 중 어느 것도 아니면 `None`
pub fn button_mode(img: &Rgb, kind: ScreenKind, geom: &ListGeom) -> Option<u8> {
    let ((x0, y0, w, h), bar_x) = match kind {
        ScreenKind::FreestyleList => (FREESTYLE_BANNER, FREESTYLE_BAR_X),
        ScreenKind::OnlineList => (ONLINE_BANNER, ONLINE_BAR_X),
        ScreenKind::Other => return None,
    };
    let s = geom.scale();
    let dx = panel_dx(geom, bar_x);
    let band = Rect::new(
        ((x0 + 0.42 * w) * s + dx).round() as i32,
        ((y0 + 0.1 * h) * s).round() as i32,
        (0.46 * w * s).round() as i32,
        (0.8 * h * s).round() as i32,
    );
    if band.intersect(&img.rect()) != Some(band) {
        return None;
    }
    let mut sum = [0f64; 3];
    for y in band.y..band.bottom() {
        for x in band.x..band.right() {
            let p = img.pixel(x as usize, y as usize);
            (0..3).for_each(|c| sum[c] += p[c] as f64);
        }
    }
    let n = (band.w * band.h) as f64;
    let mean = sum.map(|c| (c / n) as f32);
    let dist = |c: &[f32; 3]| (0..3).map(|i| (mean[i] - c[i]).powi(2)).sum::<f32>().sqrt();
    BANNER_COLORS
        .iter()
        .map(|(b, c)| (*b, dist(c)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, d)| *d <= BANNER_MAX_DIST)
        .map(|(b, _)| b)
}

/// 프레임의 칸 위치, 곡 목록을 찾아 프리스타일인지 온라인인지 가린 뒤 그 모드의 배치를 돌려줌
/// 곡 목록이 안 보이거나 모르는 배치면 `None`, 테두리 검출은 안 씀
pub fn find_diff_slots(img: &Rgb) -> Option<[Rect; 4]> {
    let g = find_list(img).filter(|g| g.rows_seen >= MIN_ROWS)?;
    slots_for(layout_kind(&g), &g)
}

/// 칸 하나에서 잰 스킨 무관 값 (뜻은 모듈 주석대로)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SlotMetrics {
    /// 안쪽 글자 없는 띠와 칸 바로 바깥 배경 띠의 L1 색 차이(0..765), 열마다 잰 값의 중앙값
    pub fill: f32,
    /// 위·아래 테두리 줄과 몇 px 바깥 배경의 L1 대비 중앙값, 두 테두리 평균
    /// 앞쪽 몇 줄 중 최댓값을 써서 얇은 테두리와 1-2 px 위치 오차를 견딤
    pub border: f32,
}

#[inline]
fn px(v: &RgbView, x: i32, y: i32) -> [i32; 3] {
    let p = v.pixel(x as usize, y as usize);
    [p[0] as i32, p[1] as i32, p[2] as i32]
}

#[inline]
fn l1(a: [i32; 3], b: [i32; 3]) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

fn median_i32(v: &mut [i32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable(mid).1 as f32
}

/// `x` 열에서 `ys` 줄들의 평균 색
fn col_mean(v: &RgbView, x: i32, ys: std::ops::Range<i32>) -> [i32; 3] {
    let n = ys.len().max(1) as i32;
    let mut acc = [0i32; 3];
    for y in ys {
        let p = px(v, x, y);
        acc[0] += p[0];
        acc[1] += p[1];
        acc[2] += p[2];
    }
    [acc[0] / n, acc[1] / n, acc[2] / n]
}

/// 칸 하나 측정, 칸(작은 여유 포함)이 프레임 밖으로 나가면 0
pub fn slot_metrics(img: &Rgb, r: Rect) -> SlotMetrics {
    let v = img.view();
    let s = img.height as f32 / REF_H;
    let sc = |k: f32| ((k * s).round() as i32).max(1);
    let (bw, bh) = (r.w, r.h);
    if bw < 8 || bh < 8 {
        return SlotMetrics::default();
    }
    let margin = sc(6.0);
    let outer = Rect::new(r.x, r.y - margin, r.w, r.h + 2 * margin);
    if outer.intersect(&img.rect()) != Some(outer) {
        return SlotMetrics::default();
    }

    // `fill`, 안쪽 글자 없는 띠와 바로 바깥 배경 띠를 열마다 비교
    let r0 = (bh as f32 * 0.14).round() as i32;
    let r1 = ((bh as f32 * 0.26).round() as i32).max(r0 + 1);
    let (c0, c1) = (
        r.x + (bw as f32 * 0.1) as i32,
        r.x + (bw as f32 * 0.9) as i32,
    );
    let out_top = r.y - sc(6.0)..r.y - sc(3.0);
    let out_bot = r.bottom() + sc(2.0)..r.bottom() + sc(5.0);
    let in_top = r.y + r0..r.y + r1;
    let in_bot = r.bottom() - r1..r.bottom() - r0;
    let mut diffs = Vec::with_capacity(2 * (c1 - c0).max(0) as usize);
    for x in c0..c1 {
        diffs.push(l1(
            col_mean(&v, x, in_top.clone()),
            col_mean(&v, x, out_top.clone()),
        ));
        diffs.push(l1(
            col_mean(&v, x, in_bot.clone()),
            col_mean(&v, x, out_bot.clone()),
        ));
    }
    let fill = median_i32(&mut diffs);

    // `border`, 테두리 줄과 몇 px 바깥 배경 비교
    let (e0, e1) = (
        r.x + (bw as f32 * 0.05) as i32,
        r.x + (bw as f32 * 0.95) as i32,
    );
    let off = sc(4.0);
    let edge = |row: i32, outside: i32| -> f32 {
        let mut d: Vec<i32> = (e0..e1)
            .map(|x| l1(px(&v, x, row), px(&v, x, outside)))
            .collect();
        median_i32(&mut d)
    };
    let depth = sc(2.0);
    let top = (0..=depth)
        .map(|d| edge(r.y + d, r.y - off))
        .fold(0.0, f32::max);
    let bot = (0..=depth)
        .map(|d| edge(r.bottom() - 1 - d, r.bottom() - 1 + off))
        .fold(0.0, f32::max);
    SlotMetrics {
        fill,
        border: 0.5 * (top + bot),
    }
}

/// 칸 하나만 보고 판단한 상태 (절대 기준)
/// 프레임의 네 칸을 비교하는 [`slot_states`]를 쓰는 게 나음
pub fn slot_state(img: &Rgb, r: Rect) -> SlotState {
    let m = slot_metrics(img, r);
    if m.fill >= FILL_SELECTED {
        SlotState::Selected
    } else if m.border >= BORDER_MIN {
        SlotState::Available
    } else {
        SlotState::Missing
    }
}

/// 미리 잰 값으로 상태 판단, `fill`이 압도적으로 높은 칸이 선택 (튀는 칸이 없으면 절대 기준)
/// 나머지는 테두리가 보이면 선택 가능
pub fn states_from_metrics(m: &[SlotMetrics; 4]) -> [SlotState; 4] {
    let mut order = [0usize, 1, 2, 3];
    order.sort_by(|&a, &b| m[b].fill.total_cmp(&m[a].fill));
    let (f0, f1) = (m[order[0]].fill, m[order[1]].fill);
    let sel = (f0 >= FILL_MIN && f0 >= 2.0 * f1 && f0 - f1 >= 50.0).then_some(order[0]);
    std::array::from_fn(|i| {
        if sel == Some(i) || (sel.is_none() && m[i].fill >= FILL_SELECTED) {
            SlotState::Selected
        } else if m[i].border >= BORDER_MIN {
            SlotState::Available
        } else {
            SlotState::Missing
        }
    })
}

/// 네 칸의 측정값
pub fn slots_metrics(img: &Rgb, slots: &[Rect; 4]) -> [SlotMetrics; 4] {
    std::array::from_fn(|i| slot_metrics(img, slots[i]))
}

/// 네 칸의 상태, 같이 비교해서 판단
pub fn slot_states(img: &Rgb, slots: &[Rect; 4]) -> [SlotState; 4] {
    states_from_metrics(&slots_metrics(img, slots))
}

/// 지금 선택된 난이도, 채워진 칸이 정확히 하나일 때만
pub fn selected_difficulty(states: &[SlotState; 4]) -> Option<Difficulty> {
    let mut sel = states
        .iter()
        .enumerate()
        .filter(|(_, s)| **s == SlotState::Selected);
    match (sel.next(), sel.next()) {
        (Some((i, _)), None) => Some(Difficulty::ALL[i]),
        _ => None,
    }
}

/// 칸 위치 찾고 상태까지 한 번에 읽기
pub fn read_slots(img: &Rgb) -> Option<[SlotState; 4]> {
    find_diff_slots(img).map(|r| slot_states(img, &r))
}
