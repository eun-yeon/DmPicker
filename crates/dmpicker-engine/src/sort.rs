//! 정렬 창 (곡 목록에서 F4), 정렬 순서 8개가 4줄 2열 격자로 있음
//! 열리면 지금 순서에 강조, 방향키로 옮김(끝에서 멈추고 안 넘어감), Enter로 적용, Esc는 안 바꾸고 닫음
//!
//! | 줄 | 왼쪽             | 오른쪽             |
//! |----|------------------|--------------------|
//! | 0  | 제목 A부터 Z     | 제목 Z부터 A       |
//! | 1  | BPM 느린 순      | BPM 빠른 순        |
//! | 2  | 레벨 1부터 15    | 레벨 15부터 1      |
//! | 3  | 업데이트 최신 순 | 업데이트 오래된 순 |
//!
//! 정렬 창은 클라이언트 가운데에 있고 다른 UI처럼 높이에 맞춰 커짐
//! 그래서 칸 위치는 클라이언트 중심 기준으로 잡음 (아래 숫자는 1440p, 폭이 달라도 그대로 맞음)
//! 강조된 칸은 바로 바깥에 폭 ~6 px 진한 노란 테두리 (255, 180, 0), 나머지 칸은 테두리 없음
//! 테두리만 읽고 글자는 안 읽음 (글자는 게임 언어 따라 바뀜)
//! 정렬 창이 떠 있으면 뒤 목록이 흐리고 어두워져서 [`crate::find_list`]가 목록을 못 찾음

use crate::img::Rgb;
use crate::REF_H;

/// 격자의 칸 하나
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
}

/// 제목 A부터 Z, 학습한 목록이 따르는 순서
pub const TITLE_ASC: Cell = Cell { row: 0, col: 0 };
pub const ROWS: usize = 4;
pub const COLS: usize = 2;

/// 열마다 칸의 왼쪽 끝과 폭, 클라이언트 중심 기준 (1440p)
const CELL_X: [f32; COLS] = [-433.0, 7.0];
const CELL_W: f32 = 426.0;
/// 줄마다 칸의 위쪽 끝과 높이, 클라이언트 중심 기준 (1440p)
const CELL_Y: [f32; ROWS] = [-187.0, -120.0, -53.0, 13.0];
const CELL_H: f32 = 53.0;
/// 칸 둘레를 샘플링하는 위치, 칸 끝에서 바깥으로 px (1440p)
/// 테두리 가운데, 칸 안쪽(글자는 피함), 테두리 너머
/// 칸 사이가 14 px라 이 칸 테두리는 가까운 절반, 이웃 칸 테두리는 먼 절반을 채움, 테두리는 늘 한 칸에만 있음
const ON: f32 = 4.0;
const INSIDE: f32 = -4.0;
const OUTSIDE: f32 = 10.0;
/// 격자 전체 둘레, 격자 바깥으로 px (1440p)
/// 위쪽 제목, 아래쪽 안내, 양옆 패널 테두리 사이라 패널 바탕만 보임
const PANEL: f32 = 24.0;
/// 칸 둘레와 격자 둘레의 가로·세로 변마다 샘플 수
const ALONG_W: usize = 48;
const ALONG_H: usize = 8;
const PANEL_W: usize = 96;
const PANEL_H: usize = 24;
/// 둘레 샘플 중 테두리 노란색 비율, 테두리로 치는 하한과 테두리 없음으로 치는 상한
const FRAMED: f32 = 0.9;
const UNFRAMED: f32 = 0.1;
/// 테두리 없는 칸의 테두리 자리, 격자 둘레에서 필요한 패널 회색 비율
const PLAIN: f32 = 0.8;
const PANEL_MIN: f32 = 0.95;

/// 테두리 노란색, 안티앨리어싱된 끝과 축소 때문에 조금 탁해짐
fn is_frame(p: [u8; 3]) -> bool {
    let [r, g, b] = p.map(i32::from);
    r >= 180 && (110..=220).contains(&g) && b <= 90 && r - b >= 150
}

/// 정렬 창 패널의 어두운 회색 (약 (40, 39, 38)), 테두리 없는 칸 둘레도 이 색
/// 들어오는 중인 프레임은 해당 없음 (F4 직후 첫 프레임은 모든 칸이 흐린 주황)
/// 정렬 창이 닫혀 있을 때 뒤 목록 줄도 대부분 해당 없음
fn is_panel(p: [u8; 3]) -> bool {
    let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
    hi <= 80 && hi - lo <= 24
}

/// 사각형 `x0..=x1` x `y0..=y1`(1440p, 클라이언트 중심 기준) 둘레 샘플 중 `test`를 통과하는 비율
/// 변마다 `along`개씩 샘플, 프레임 밖으로 나가면 `None`
fn loop_share(
    img: &Rgb,
    (x0, x1, y0, y1): (f32, f32, f32, f32),
    (along_w, along_h): (usize, usize),
    test: fn([u8; 3]) -> bool,
) -> Option<f32> {
    let s = img.height as f32 / REF_H;
    let (cx, cy) = (img.width as f32 / 2.0, img.height as f32 / 2.0);
    // 픽셀 `p`(1440p, 중심 기준)는 p..p+1을 덮음, 그 가운데가 떨어지는 클라이언트 픽셀을 씀
    let px = |p: f32| (cx + (p + 0.5) * s).floor() as i64;
    let py = |p: f32| (cy + (p + 0.5) * s).floor() as i64;
    let along =
        |a: f32, b: f32, n: usize| (0..n).map(move |i| a + (b - a) * i as f32 / (n - 1) as f32);
    let mut pts = Vec::with_capacity(2 * (along_w + along_h));
    for x in along(x0, x1, along_w) {
        pts.push((px(x), py(y0)));
        pts.push((px(x), py(y1)));
    }
    for y in along(y0, y1, along_h) {
        pts.push((px(x0), py(y)));
        pts.push((px(x1), py(y)));
    }
    let inside = |&(x, y): &(i64, i64)| {
        x >= 0 && y >= 0 && (x as usize) < img.width && (y as usize) < img.height
    };
    if !pts.iter().all(inside) {
        return None;
    }
    let hits = pts
        .iter()
        .filter(|&&(x, y)| test(img.pixel(x as usize, y as usize)))
        .count();
    Some(hits as f32 / pts.len() as f32)
}

/// `cell` 바깥 `off` px 둘레의 [`loop_share`]
fn share(img: &Rgb, cell: Cell, off: f32, test: fn([u8; 3]) -> bool) -> Option<f32> {
    let (x, y) = (CELL_X[cell.col], CELL_Y[cell.row]);
    let rect = (
        x - off,
        x + CELL_W - 1.0 + off,
        y - off,
        y + CELL_H - 1.0 + off,
    );
    loop_share(img, rect, (ALONG_W, ALONG_H), test)
}

/// 격자 전체 둘레에서 패널 회색 비율
pub fn panel_share(img: &Rgb) -> Option<f32> {
    let (x0, y0) = (CELL_X[0] - PANEL, CELL_Y[0] - PANEL);
    let (x1, y1) = (
        CELL_X[COLS - 1] + CELL_W - 1.0 + PANEL,
        CELL_Y[ROWS - 1] + CELL_H - 1.0 + PANEL,
    );
    loop_share(img, (x0, x1, y0, y1), (PANEL_W, PANEL_H), is_panel)
}

/// 격자의 모든 칸, 줄 순서대로
pub fn cells() -> impl Iterator<Item = Cell> {
    (0..ROWS).flat_map(|row| (0..COLS).map(move |col| Cell { row, col }))
}

/// 정렬 창이 멈춰 있는 프레임이면 강조된 칸, 셋이 동시에 맞아야 함
/// 격자 둘레가 전부 패널 회색, 테두리 칸이 딱 하나(노란 둘레, 안쪽·바깥엔 노란색 없음),
/// 나머지 칸은 확실히 테두리 없음
/// 그 밖의 프레임은 `None`, 정렬 창이 들어오는 첫 프레임도 마찬가지
pub fn highlighted(img: &Rgb) -> Option<Cell> {
    if panel_share(img)? < PANEL_MIN {
        return None;
    }
    let mut lit = None;
    for cell in cells() {
        if share(img, cell, ON, is_frame)? >= FRAMED {
            let thin = share(img, cell, INSIDE, is_frame)? <= UNFRAMED
                && share(img, cell, OUTSIDE, is_frame)? <= UNFRAMED;
            if lit.is_some() || !thin {
                return None;
            }
            lit = Some(cell);
        } else if share(img, cell, ON, is_panel)? < PLAIN {
            return None;
        }
    }
    lit
}
