//! 화면 판별 (프리스타일 곡 목록, 온라인 방 곡 목록, 그 밖)
//!
//! 곡 목록만 보고 판단함 (난이도 칸 스킨은 팩마다 달라서 판별 조건으로 안 씀)
//! * [`find_list`]가 줄 격자에서 9줄 이상 찾으면 곡 목록 화면
//! * 프리스타일/온라인은 목록의 가로 위치로 가림
//!   2560x1440 디자인 배치에서 색 막대 x = 1026(프리스타일), 965(온라인, 가운데 정렬 목록)
//!   전부 클라이언트 높이에 맞춰 커짐
//!   가운데 정렬 캔버스와 왼쪽 고정 두 모델을 다 대 봐서 16:9면 크기와 상관없이 정확함
//!   어느 후보에도 안 맞으면(모르는 배치) 곡 목록 화면으로 안 봄, 다른 모드의 목록을 쓰거나 저장하지 않게

use crate::geom::{at_rest, find_list, ListGeom};
use crate::img::{Rect, Rgb};
use crate::panel::{slot_states, slots_for, SlotState};
use crate::REF_H;

/// 프레임이 보여주는 화면
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenKind {
    FreestyleList,
    OnlineList,
    Other,
}

/// [`analyze`]가 찾은 것 전부
#[derive(Clone, Copy, Debug)]
pub struct ScreenInfo {
    pub kind: ScreenKind,
    pub list: Option<ListGeom>,
    /// 찾은 배치의 난이도 칸 위치 (목록 화면이 아니면 `None`)
    pub slots: Option<[Rect; 4]>,
    /// 난이도 칸 상태 (목록 화면이 아니면 `None`)
    pub slot_states: Option<[SlotState; 4]>,
    /// 목록 스크롤 애니메이션이 끝났는지 (목록이 없으면 false)
    pub at_rest: bool,
}

/// 2560x1440 디자인 배치에서 프리스타일 목록 막대 x
pub const FREESTYLE_BAR_X: f32 = 1026.0;
/// 2560x1440 디자인 배치에서 온라인 목록 막대 x
pub const ONLINE_BAR_X: f32 = 965.0;
const DESIGN_W: f32 = 2560.0;

/// 곡 목록으로 치려면 격자에 있어야 하는 최소 줄 수
pub const MIN_ROWS: u32 = 9;

/// 프레임을 판별하고 그 과정에서 찾은 위치도 돌려줌
/// 모르는 배치의 목록은 목록이 없는 화면과 같게 돌려줌
pub fn analyze(img: &Rgb) -> ScreenInfo {
    let Some((g, kind)) = find_list(img)
        .filter(|g| g.rows_seen >= MIN_ROWS)
        .map(|g| (g, layout_kind(&g)))
        .filter(|&(_, kind)| kind != ScreenKind::Other)
    else {
        return ScreenInfo {
            kind: ScreenKind::Other,
            list: None,
            slots: None,
            slot_states: None,
            at_rest: false,
        };
    };
    let slots = slots_for(kind, &g);
    ScreenInfo {
        kind,
        list: Some(g),
        slots,
        slot_states: slots.map(|s| slot_states(img, &s)),
        at_rest: at_rest(img, &g),
    }
}

/// 프레임 판별
pub fn classify(img: &Rgb) -> ScreenKind {
    find_list(img)
        .filter(|g| g.rows_seen >= MIN_ROWS)
        .map_or(ScreenKind::Other, |g| layout_kind(&g))
}

/// `width` x `height` 클라이언트에서 디자인 배치별 목록 막대 x
/// 프리스타일과 온라인 각각 가운데 정렬 캔버스, 왼쪽 고정
fn design_bars(width: u32, height: u32) -> [(ScreenKind, f32); 4] {
    let s = height as f32 / REF_H;
    let center = |design_x: f32| width as f32 / 2.0 + (design_x - DESIGN_W / 2.0) * s;
    [
        (ScreenKind::FreestyleList, center(FREESTYLE_BAR_X)),
        (ScreenKind::OnlineList, center(ONLINE_BAR_X)),
        (ScreenKind::FreestyleList, FREESTYLE_BAR_X * s),
        (ScreenKind::OnlineList, ONLINE_BAR_X * s),
    ]
}

/// 디자인 배치별 목록([`ListGeom::nominal`]), 목록이 안 보이는 화면(빈 탭)에서 씀
pub fn nominal_lists(width: u32, height: u32) -> [(ScreenKind, ListGeom); 4] {
    design_bars(width, height).map(|(k, x)| (k, ListGeom::nominal(x, width, height)))
}

/// 곡 목록이 `geom`인 프레임이 프리스타일인지 온라인인지
/// 막대가 디자인 배치 후보 넷 모두에서 20 px(1440p) 넘게 떨어져 있으면 모르는 배치라 `Other`
pub fn layout_kind(geom: &ListGeom) -> ScreenKind {
    let s = geom.scale();
    let bx = geom.bar_x as f32;
    let (kind, dist) = design_bars(geom.width, geom.height)
        .iter()
        .map(|&(k, x)| (k, (bx - x).abs()))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    if dist <= 20.0 * s {
        kind
    } else {
        ScreenKind::Other
    }
}
