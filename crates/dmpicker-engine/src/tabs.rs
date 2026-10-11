//! 곡 목록 위 탭 줄, 지금 탭은 왼쪽 밝은 상자 안에 있고 다음 탭들은 오른쪽에 글자로만 이어짐
//! ALL TRACK 글자만 알아보고 탭 순서나 개수는 안 봄 (DLC 팩이 추가될 때마다 바뀜)
//!
//! 두 모양 다 같은 크기의 같은 글꼴, 지금 탭은 밝은 상자에 어두운 글자, 탭 줄은 어두운 바탕에 밝은 글자
//! 그래서 템플릿 하나로 둘 다 맞춤, 상관계수가 지금 탭이면 +1 근처, 탭 줄에선 -1 근처

use std::sync::OnceLock;

use crate::geom::ListGeom;
use crate::img::{resample, Rect, Rgb, RgbView};
use crate::state::{nominal_lists, ScreenKind};

/// 지금 탭에 보이는 ALL TRACK 글자, 1440p 화면에서 아래 상자 위치대로 잘라 둠
static LABEL_PNG: &[u8] = include_bytes!("../assets/all_track_label.png");

/// 1440p 기준 글자 상자, 왼쪽 끝은 첫 재킷 왼쪽 끝 기준, 위쪽 끝은 0번 줄 기준
const BOX_DX: f32 = -3.0;
const BOX_DY: f32 = -60.0;
const BOX_W: f32 = 140.0;
const BOX_H: f32 = 34.0;
/// 탭 줄 글자, 첫 글자가 지금 탭 글자에서 떨어진 거리와 그다음 간격
const STRIP_DX: f32 = 241.0;
const STRIP_PITCH: f32 = 213.0;
/// 획이 가늘어서 매칭이 예민함, 목록 위치가 1픽셀만 반올림돼도(1080p 등) 점수가 크게 떨어짐
/// 그래서 글자마다 기본 상자 주변을 찾아봄 (1440p 단위)
const SLACK_X: f32 = 4.5;
const SLACK_Y: f32 = 3.0;
const SLACK_STEP: f32 = 1.5;
/// 탭 줄 글자는 기본 간격에서 몇 픽셀 더 어긋남
const STRIP_SLACK_X: f32 = 12.0;
/// 지금 탭 오른쪽으로 확인하는 탭 줄 글자 수
pub const STRIP_SLOTS: usize = 6;

const FW: usize = 72;
const FH: usize = 16;
const CURRENT_MIN: f32 = 0.8;
const STRIP_MIN: f32 = 0.7;

/// 글자 상자의 luma 썸네일, 평균 0·길이 1로 맞추고 살짝 블러 (최고점이 덜 뾰족하게)
fn feature(v: RgbView) -> Vec<f32> {
    let rgb = resample(v, FW, FH, 0.75 * v.width as f32 / FW as f32);
    let mut f: Vec<f32> = rgb.chunks_exact(3).map(|p| (p[0] + p[1] + p[2]) / 3.0).collect();
    let mean = f.iter().sum::<f32>() / f.len() as f32;
    f.iter_mut().for_each(|v| *v -= mean);
    let norm = f.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        f.iter_mut().for_each(|v| *v /= norm);
    }
    f
}

fn template() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        let im = Rgb::decode(LABEL_PNG).expect("built-in ALL TRACK label");
        feature(im.view())
    })
}

/// 지금 탭 글자 상자에서 (`dx`, `dy`)(1440p 단위)만큼 옮긴 상자의 썸네일
/// 상자가 프레임 밖으로 나가면 `None`
fn label(img: &Rgb, geom: &ListGeom, dx: f32, dy: f32) -> Option<Vec<f32>> {
    let s = geom.scale();
    let left = geom.jacket_box(0).x as f32;
    let r = Rect::new(
        (left + (BOX_DX + dx) * s).round() as i32,
        (geom.row0_top + (BOX_DY + dy) * s).round() as i32,
        (BOX_W * s).round() as i32,
        (BOX_H * s).round() as i32,
    );
    let r = r.intersect(&img.rect()).filter(|c| *c == r)?;
    Some(feature(img.sub(r)))
}

/// (`dx`, 0) 주변 양쪽으로 `slack_x`까지 옮겨 가며 잰 템플릿 상관계수
fn scores<'a>(img: &'a Rgb, geom: &'a ListGeom, dx: f32, slack_x: f32) -> impl Iterator<Item = f32> + 'a {
    let steps = |slack: f32| (slack / SLACK_STEP) as i32;
    let (sx, sy) = (steps(slack_x), steps(SLACK_Y));
    (-sx..=sx).flat_map(move |i| (-sy..=sy).map(move |j| (i, j))).filter_map(move |(i, j)| {
        let f = label(img, geom, dx + SLACK_STEP * i as f32, SLACK_STEP * j as f32)?;
        Some(f.iter().zip(template()).map(|(a, b)| a * b).sum())
    })
}

/// 지금 탭 글자의 썸네일, 탭 전환 애니메이션이 끝났는지 볼 때 씀
pub fn current_label(img: &Rgb, geom: &ListGeom) -> Option<Vec<f32>> {
    label(img, geom, 0.0, 0.0)
}

/// ALL TRACK 위치, `Some(0)` = 지금 탭, `Some(k)` = 오른쪽으로 `k`번째 탭
/// (탭 줄 앞쪽 `slots`개 안에서), `None` = 안 보임
pub fn all_track_offset(img: &Rgb, geom: &ListGeom, slots: usize) -> Option<usize> {
    if scores(img, geom, 0.0, SLACK_X).any(|c| c >= CURRENT_MIN) {
        return Some(0);
    }
    (1..=slots.min(STRIP_SLOTS)).find(|&k| {
        let nominal = STRIP_DX + STRIP_PITCH * (k - 1) as f32;
        scores(img, geom, nominal, STRIP_SLACK_X).any(|c| c <= -STRIP_MIN)
    })
}

/// 탭 줄에서 지금 탭 뒤로 보이는 이름 수, 온라인 방은 두 개만 보임
pub fn strip_slots(kind: ScreenKind) -> usize {
    if kind == ScreenKind::OnlineList {
        2
    } else {
        5
    }
}

/// 목록이 없는 선곡 화면 (빈 FAVORITE 탭): 디자인 배치의 목록 자리로 탭 줄을 읽음
/// ALL TRACK이 오른쪽 `k`번째 탭으로 보이면 (선곡 화면이라는 증거) 그 배치의 화면 종류와 `k`
pub fn empty_tab(img: &Rgb) -> Option<(ScreenKind, usize)> {
    nominal_lists(img.width as u32, img.height as u32)
        .into_iter()
        .find_map(|(kind, g)| all_track_offset(img, &g, strip_slots(kind)).filter(|&k| k > 0).map(|k| (kind, k)))
}
