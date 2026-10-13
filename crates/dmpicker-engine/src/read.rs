//! 보이는 목록 11줄 읽기, 재킷 식별과 RANDOM / 회색조(잠김) 표시 확인

use crate::features::{
    inset_view, jacket_feature, jacket_feature_gray, Candidate, Feature, RefSet, RANDOM_ID,
};
use crate::geom::ListGeom;
use crate::img::{Rgb, RgbView};
use crate::NROWS;

/// `sure` 조건, 1위 유사도 하한
pub const SURE_SCORE: f32 = 0.7;
/// `sure` 조건, 1위와 2위 차이 하한
pub const SURE_MARGIN: f32 = 0.12;
/// 재킷을 회색조로 치는 평균 채도 기준 (채널 최댓값 - 최솟값, 8비트 단위)
/// 온라인에서 잠긴 곡은 회색으로 나옴, 원래 회색조인 재킷도 있음 (3:33, Carrot Carrot 등)
pub const GREY_CHROMA: f32 = 6.0;
/// 줄마다 남기는 후보 수
pub const TOPK: usize = 3;

/// 보이는 줄 하나의 결과
#[derive(Clone, Debug)]
pub struct RowRead {
    /// 보이는 줄 번호 (0..11, 선택 = [`crate::SEL_ROW`])
    pub row: usize,
    /// 서로 다른 후보 최대 3개, 높은 순
    pub cands: Vec<Candidate>,
    /// `top1 >= 0.7 && top1 - top2 >= 0.12`
    pub sure: bool,
    /// 1위가 내장 RANDOM(?) 템플릿
    pub is_random: bool,
    /// 재킷이 (거의) 회색조, 온라인의 LOCKED 곡이거나 그냥 회색 그림
    pub is_greyscale: bool,
    /// (가장자리 잘라낸) 크롭의 평균 채도
    pub chroma: f32,
    /// 크롭의 원래 컬러 특징, 학습한 목록의 줄과 맞춰 볼 때 씀
    pub feature: Box<Feature>,
}

impl RowRead {
    /// 1위 후보
    pub fn top(&self) -> Option<Candidate> {
        self.cands.first().copied()
    }

    /// 1위 후보의 id
    pub fn top_id(&self) -> Option<crate::SongId> {
        self.top().map(|c| c.id)
    }

    /// `top1 - top2` (2위가 없으면 `top1`)
    pub fn margin(&self) -> f32 {
        match self.cands.as_slice() {
            [a, b, ..] => a.score - b.score,
            [a] => a.score,
            [] => 0.0,
        }
    }
}

/// 재킷 크롭 하나 식별
pub fn read_crop(crop: RgbView, refs: &RefSet) -> RowRead {
    let feature = Box::new(jacket_feature(crop));
    let chroma = inset_view(crop).mean_chroma();
    let is_greyscale = chroma < GREY_CHROMA;
    let mut cands = refs.topk(&feature, TOPK);
    if is_greyscale {
        let g = refs.topk_gray(&jacket_feature_gray(crop), TOPK);
        if g.first().map(|c| c.score) > cands.first().map(|c| c.score) {
            cands = g;
        }
    }
    let mut r = RowRead {
        row: 0,
        cands,
        sure: false,
        is_random: false,
        is_greyscale,
        chroma,
        feature,
    };
    let top = r.top().map_or(0.0, |c| c.score);
    r.sure = top >= SURE_SCORE && r.margin() >= SURE_MARGIN;
    r.is_random = r.top_id() == Some(RANDOM_ID) && top >= 0.6;
    r
}

/// 보이는 `k`번 줄 식별
pub fn read_row(img: &Rgb, geom: &ListGeom, refs: &RefSet, k: usize) -> RowRead {
    let mut r = read_crop(img.sub(geom.jacket_box(k)), refs);
    r.row = k;
    r
}

/// 보이는 11줄 전부 식별 (5번 = 선택)
pub fn read_window(img: &Rgb, geom: &ListGeom, refs: &RefSet) -> [RowRead; NROWS] {
    std::array::from_fn(|k| read_row(img, geom, refs, k))
}
