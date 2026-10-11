//! 곡 목록 위치, 줄 격자 자동 보정과 스크롤 멈춤 감지
//!
//! 목록의 줄마다 80x80 재킷 오른쪽 끝에 세로 색 막대가 있음
//! 줄 높이만큼 밝은 세로 구간이 일정 간격으로 많이 나오는 열을 찾으면 어떤 배치든 막대 열이 잡힘
//! 프리스타일은 막대 x ~ 1026/1440*H, 온라인은 가운데 정렬 목록이라 2560x1440에서 막대 x = 965

use crate::img::{Rect, Rgb, RgbView};
use crate::{NROWS, REF_H};

/// 클라이언트 높이 대비 줄 간격
pub const PITCH_OVER_H: f32 = 93.4 / REF_H;
/// 클라이언트 높이 대비 줄(재킷) 높이
pub const ROWH_OVER_H: f32 = 80.0 / REF_H;
/// 클라이언트 높이 대비 0번 줄 위쪽 끝 기본 위치 (프리스타일·온라인, 16:9)
pub const ROW0_OVER_H: f32 = 244.0 / REF_H;
/// 가장 밝은 채널이 이 값을 넘으면 밝은 픽셀
const BRIGHT: u8 = 150;
/// 재킷 오른쪽 끝은 막대 오른쪽 끝보다 이만큼 왼쪽 (1440p)
const JACKET_UNDER_BAR: f32 = 5.0;

/// 보정된 목록 위치, 클라이언트 픽셀 단위
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListGeom {
    /// 줄 색 막대의 가장 왼쪽 열
    pub bar_x: i32,
    /// 색 막대 폭 (1440p에서 ~6-7 px)
    pub bar_w: i32,
    /// 보이는 0번 줄 위쪽 끝, 격자에 맞춘 값
    pub row0_top: f32,
    /// 줄 간격
    pub pitch: f32,
    /// 줄 높이 (= 재킷 크기)
    pub row_h: f32,
    /// 격자 위에서 막대가 잡힌 줄 수 (11개 중)
    pub rows_seen: u32,
    /// 잴 때의 클라이언트 크기
    pub width: u32,
    pub height: u32,
}

impl ListGeom {
    /// 디자인 배치대로 놓인 목록, 색 막대는 `bar_x`, 클라이언트는 `width` x `height` (잡힌 줄 없음)
    /// 잴 목록이 안 보이는 화면에서 씀
    pub fn nominal(bar_x: f32, width: u32, height: u32) -> ListGeom {
        let h = height as f32;
        ListGeom {
            bar_x: bar_x.round() as i32,
            bar_w: (6.0 * h / REF_H).round().max(1.0) as i32,
            row0_top: ROW0_OVER_H * h,
            pitch: PITCH_OVER_H * h,
            row_h: ROWH_OVER_H * h,
            rows_seen: 0,
            width,
            height,
        }
    }

    /// `H / 1440`
    pub fn scale(&self) -> f32 {
        self.height as f32 / REF_H
    }

    /// 보이는 `k`번 줄 위쪽 끝 (0..11, 선택 = 5)
    pub fn row_top(&self, k: usize) -> f32 {
        self.row0_top + k as f32 * self.pitch
    }

    /// 보이는 `k`번 줄의 재킷 사각형, 색 막대가 재킷 오른쪽 끝을 덮음
    /// 재킷은 막대 오른쪽 끝에서 85 px(1440p) 왼쪽에서 시작
    /// 막대 왼쪽 열은 재킷과 안티앨리어싱돼서 프레임에 따라 밝게 잡히기도 해서 오른쪽 끝 기준으로 잼
    pub fn jacket_box(&self, k: usize) -> Rect {
        let x1 = (self.bar_x + self.bar_w) as f32 - JACKET_UNDER_BAR * self.scale();
        let x0 = x1 - self.row_h;
        let y0 = self.row_top(k);
        let y1 = y0 + self.row_h;
        let (x0, y0, x1, y1) = (
            x0.round() as i32,
            y0.round() as i32,
            x1.round() as i32,
            y1.round() as i32,
        );
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// 0번 줄이 기본 위치(244/1440*H)에서 벗어난 정도, 픽셀 단위
    /// 알려진 16:9 배치에서 목록이 멈춰 있으면 0 근처, 크면 스크롤 도중 캡처했거나 모르는 배치
    pub fn grid_offset(&self) -> f32 {
        self.row0_top - ROW0_OVER_H * self.height as f32
    }
}

#[inline]
fn value(p: &[u8]) -> u8 {
    p[0].max(p[1]).max(p[2])
}

/// 곡 목록 찾기, 일정한 격자에 놓인 줄이 8개 미만이면 `None` (목록 아닌 화면, 많이 가려짐)
///
/// 이미지를 `step`번째 열마다 줄 단위로 훑고(캐시 친화적) 잡힌 구간을 하나씩 격자 기준으로 시험함
/// 막대 왼쪽 끝과 폭은 잡힌 줄 전부로 다듬음
/// 첫 구간의 절대 줄 번호는 0번 줄 기본 위치로 정함, 0번 줄 막대가 어두워도 격자가 안 밀림
pub fn find_list(img: &Rgb) -> Option<ListGeom> {
    find_list_view(img.view())
}

/// view를 받는 [`find_list`]
pub fn find_list_view(v: RgbView) -> Option<ListGeom> {
    let (w, h) = (v.width, v.height);
    if w < 16 || h < 64 {
        return None;
    }
    let hf = h as f32;
    let s = hf / REF_H;
    let rowh = ROWH_OVER_H * hf;
    let pitch = PITCH_OVER_H * hf;
    let (min_len, max_len) = (rowh * 0.85, rowh * 1.15);
    let step = ((2.0 * s).round() as usize).max(1);
    let ncols = w.div_ceil(step);

    // 샘플 열에서 줄 단위로 밝은 구간 찾기
    let mut start = vec![u32::MAX; ncols];
    let mut runs: Vec<Vec<u32>> = vec![Vec::new(); ncols];
    for y in 0..h {
        let row = v.row(y);
        for (ci, st) in start.iter_mut().enumerate() {
            let x = ci * step;
            let bright = value(&row[x * 3..x * 3 + 3]) > BRIGHT;
            if bright {
                if *st == u32::MAX {
                    *st = y as u32;
                }
            } else if *st != u32::MAX {
                let len = (y as u32 - *st) as f32;
                if len >= min_len && len <= max_len {
                    runs[ci].push(*st);
                }
                *st = u32::MAX;
            }
        }
    }

    // 일정 간격 구간이 가장 많은 열
    let tol = (3.0 * s).max(2.0);
    // (샘플 열, 격자 위 구간들을 (위쪽 끝, 기준 구간과의 줄 차이)로)
    let mut best: Option<(usize, Vec<(f32, i32)>)> = None;
    for (ci, tops) in runs.iter().enumerate() {
        if tops.len() < 8 {
            continue;
        }
        let mut col_best: Option<Vec<(f32, i32)>> = None;
        for &anchor in tops {
            let a = anchor as f32;
            let good: Vec<(f32, i32)> = tops
                .iter()
                .filter_map(|&t| {
                    let t = t as f32;
                    let k = ((t - a) / pitch).round();
                    ((t - (a + k * pitch)).abs() < tol).then_some((t, k as i32))
                })
                .collect();
            if col_best.as_ref().is_none_or(|b| good.len() > b.len()) {
                col_best = Some(good);
            }
        }
        let good = col_best.unwrap_or_default();
        if good.len() >= 8 && best.as_ref().is_none_or(|b| good.len() > b.1.len()) {
            best = Some((ci, good));
        }
    }
    let (ci, good) = best?;
    let ngood = good.len();
    let kmin = good.iter().map(|g| g.1).min()?;
    let kmax = good.iter().map(|g| g.1).max()?;
    let span = (kmax - kmin + 1) as usize;
    if span > NROWS {
        return None;
    }

    // 막대 범위, 잡힌 줄 안쪽에서 90% 넘게 밝은 열이 막대
    let inset = (5.0 * s).round().max(1.0) as usize;
    let is_bar = |x: usize| -> bool {
        let (mut on, mut tot) = (0usize, 0usize);
        for &(t, _) in &good {
            let y0 = t as usize + inset;
            let y1 = ((t + rowh) as usize).saturating_sub(inset).min(h);
            for y in y0..y1 {
                tot += 1;
                on += (value(&v.row(y)[x * 3..x * 3 + 3]) > BRIGHT) as usize;
            }
        }
        tot > 0 && on * 10 > tot * 9
    };
    let x = ci * step;
    if !is_bar(x) {
        return None;
    }
    let mut x0 = x;
    while x0 > 0 && is_bar(x0 - 1) {
        x0 -= 1;
    }
    let mut x1 = x + 1;
    while x1 < w && is_bar(x1) {
        x1 += 1;
    }

    // 처음 잡힌 줄의 격자 기준 위쪽 끝, 그다음 절대 줄 번호
    let mut offs: Vec<f32> = good
        .iter()
        .map(|&(t, k)| t - (k - kmin) as f32 * pitch)
        .collect();
    offs.sort_by(|a, b| a.total_cmp(b));
    let y_first = median_sorted(&offs);
    let nominal = ROW0_OVER_H * hf;
    let max_shift = (NROWS - span) as i32;
    let shift = (((y_first - nominal) / pitch).round() as i32).clamp(0, max_shift);
    let row0_top = y_first - shift as f32 * pitch;
    if row0_top < -2.0 || row0_top + (NROWS - 1) as f32 * pitch + rowh > hf + 2.0 {
        return None;
    }

    Some(ListGeom {
        bar_x: x0 as i32,
        bar_w: (x1 - x0) as i32,
        row0_top,
        pitch,
        row_h: rowh,
        rows_seen: ngood as u32,
        width: w as u32,
        height: h as u32,
    })
}

fn median_sorted(v: &[f32]) -> f32 {
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// 목록 스크롤이 멈췄으면 true, 막대 열의 밝은 구간 시작·끝이 격자에 맞아야 함 (1440p ±3 px)
/// `geom`은 멈춘 프레임에서 구한 것이어야 함, 한 번 보정해서 계속 쓰면 됨
/// [`ListGeom::grid_offset`]이 0 근처면 제대로 구한 것
pub fn at_rest(img: &Rgb, geom: &ListGeom) -> bool {
    rest_score(img.view(), geom).0
}

/// 멈춤 판단 세부값 `(at_rest, aligned_rows, misaligned_runs)`
pub fn rest_score(v: RgbView, geom: &ListGeom) -> (bool, u32, u32) {
    let h = v.height;
    let s = h as f32 / REF_H;
    let cx = (geom.bar_x + geom.bar_w / 2).max(0) as usize;
    if cx >= v.width || h == 0 {
        return (false, 0, 0);
    }
    let tol = (3.0 * s).max(1.5);
    let y_lo = (geom.row0_top - geom.pitch * 0.5).max(0.0) as usize;
    let y_hi = ((geom.row_top(NROWS - 1) + geom.pitch).min(h as f32)) as usize;
    let (mut aligned, mut misaligned) = (0u32, 0u32);
    let mut st: Option<usize> = None;
    let mut close = |a: usize, b: usize| {
        let len = (b - a) as f32;
        if len < geom.row_h * 0.5 || len > geom.row_h * 1.5 {
            return;
        }
        let k = ((a as f32 - geom.row0_top) / geom.pitch).round();
        let top = geom.row0_top + k * geom.pitch;
        if (a as f32 - top).abs() <= tol && (b as f32 - (top + geom.row_h)).abs() <= tol {
            aligned += 1;
        } else {
            misaligned += 1;
        }
    };
    for y in y_lo..y_hi {
        let bright = v.value(cx, y) > BRIGHT;
        match (bright, st) {
            (true, None) => st = Some(y),
            (false, Some(a)) => {
                close(a, y);
                st = None;
            }
            _ => {}
        }
    }
    if let Some(a) = st {
        close(a, y_hi);
    }
    let need = (geom.rows_seen.min(NROWS as u32)).saturating_sub(2).max(6);
    (aligned >= need && misaligned <= 1, aligned, misaligned)
}
