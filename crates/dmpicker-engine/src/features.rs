//! 재킷 특징과 기준 세트
//!
//! 특징 만드는 법
//! 크롭 가장자리 6%씩 잘라냄, sigma = 2.0 * crop_width / 70 가우시안 블러, 24x24 bilinear 축소(안티앨리어싱)
//! 여기서 sigma = 10.0 * crop_width / 70으로 블러한 같은 썸네일을 뺌
//! 채널별 표준화 `(x - mean_c) / (std_c + 4)`, L2 정규화
//! djmax.gg 128 px 재킷과 코사인 유사도(= 내적)로 비교
//! 채널별 표준화로 전체에 낀 색조가 상쇄됨
//! 넓은 블러를 빼면 목록 배경의 크로스페이드·테마가 재킷 위에 까는 고르지 않은 음영도 상쇄됨
//! 이 음영은 배경 애니메이션에 따라 계속 바뀜
//! 작은 블러는 고정된 대각선 줄무늬 오버레이를 눌러 줌
//! 게임이 일부 재킷에 붙이는 표시(즐겨찾기, CLEAR PASS+) 아래 칸은 뺌
//!
//! 회색조 크롭용으로 같은 방법의 단일 채널(luma) 버전도 같이 둠 (온라인 방에선 잠긴 곡이 회색으로 나옴)

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::img::{apply_plane, apply_rgb, Op1d, Rgb, RgbView};

/// 썸네일 한 변 길이
pub const FEAT_SIZE: usize = 24;
/// 컬러 특징 길이 (24 x 24 x RGB, 픽셀마다 RGB가 붙어 있음)
pub const FEAT_LEN: usize = FEAT_SIZE * FEAT_SIZE * 3;
/// 회색조 특징 길이 (24 x 24)
pub const GRAY_LEN: usize = FEAT_SIZE * FEAT_SIZE;
/// 만드는 법이 바뀌면 올림 (특징 캐시 무효화)
pub const FEATURE_VERSION: u32 = 4;

const INSET: f32 = 0.06;
const BLUR_AT_70: f32 = 2.0;
/// 썸네일에서 빼는 넓은 블러 (sigma 기준은 위와 같음)
/// 재킷 모양은 남기고 위에 깔린 완만한 음영만 지움
const WIDE_AT_70: f32 = 10.0;
const STD_EPS: f32 = 4.0;
/// 즐겨찾기 곡은 재킷 왼쪽 위 모서리에 노란 삼각형이 붙음 (변 길이는 재킷의 1/4)
/// 썸네일에서 `x + y`가 이 값보다 작은 칸
const CORNER_CELLS: usize = 8;
/// CLEAR PASS+ 곡(며칠마다 바뀜)은 왼쪽 아래에 주황색 CP+ 띠가 붙음, 폭은 재킷 절반, 높이는 1/5
/// `y` 18부터 아래, `x` 14 왼쪽 칸 (띠는 썸네일의 0..13 x 19..23, 블러 몫으로 한 칸 더)
const BAND_CELLS: (usize, usize) = (14, 18);

/// 썸네일 칸 (`x`, `y`)를 특징에 쓰는지
/// 게임이 일부 재킷에 붙이는 표시는 기준 재킷까지 포함해 모든 특징에서 뺌
pub fn kept_cell(x: usize, y: usize) -> bool {
    x + y >= CORNER_CELLS && !(x < BAND_CELLS.0 && y >= BAND_CELLS.1)
}

/// L2 정규화된 컬러 특징
pub type Feature = [f32; FEAT_LEN];
/// L2 정규화된 회색조 특징
pub type GrayFeature = [f32; GRAY_LEN];
/// djmax.gg 곡 id (`djmaxgg_songs.json`의 `song.title`, 재킷 파일 이름과 같음)
pub type SongId = u32;
/// 프리스타일 RANDOM 항목의 가짜 곡 id (? 재킷, 내장 템플릿)
pub const RANDOM_ID: SongId = u32::MAX;

static RANDOM_JACKET_PNG: &[u8] = include_bytes!("../assets/random_jacket.png");

/// 매칭 후보 하나
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub id: SongId,
    /// 코사인 유사도, [-1, 1]
    pub score: f32,
}

pub(crate) fn inset_view(v: RgbView) -> RgbView {
    // m = floor(w * 0.06), 두 축에 똑같이 적용
    let m = (v.width as f32 * INSET) as i32;
    v.sub(crate::img::Rect::new(
        m,
        m,
        v.width as i32 - 2 * m,
        v.height as i32 - 2 * m,
    ))
}

fn ops_for(v: RgbView, blur_at_70: f32) -> (Op1d, Op1d) {
    let sigma = blur_at_70 * v.width as f32 / 70.0;
    (
        Op1d::blur_resize(v.width, FEAT_SIZE, sigma),
        Op1d::blur_resize(v.height, FEAT_SIZE, sigma),
    )
}

/// 채널별 표준화 후 L2 정규화, 제자리에서 처리 (`ch`개 채널이 번갈아 들어 있음)
/// 표시 칸([`kept_cell`])은 계산에서 빠지고 0이 됨
fn standardize(px: &mut [f32], ch: usize) {
    let keep: Vec<bool> = (0..px.len() / ch).map(|i| kept_cell(i % FEAT_SIZE, i / FEAT_SIZE)).collect();
    let n = keep.iter().filter(|&&k| k).count() as f32;
    for c in 0..ch {
        let kept = || px.iter().skip(c).step_by(ch).zip(&keep).filter(|(_, k)| **k).map(|(x, _)| *x);
        let mean = kept().sum::<f32>() / n;
        let var = kept().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
        let d = var.sqrt() + STD_EPS;
        px.iter_mut()
            .skip(c)
            .step_by(ch)
            .zip(&keep)
            .for_each(|(x, k)| *x = if *k { (*x - mean) / d } else { 0.0 });
    }
    let norm = px.iter().map(|x| x * x).sum::<f32>().sqrt() + 1e-6;
    px.iter_mut().for_each(|x| *x /= norm);
}

/// 재킷 크롭의 컬러 특징 (크기 무관, 게임 크롭은 ~80x80/1440*H, 기준 재킷은 128x128)
pub fn jacket_feature(v: RgbView) -> Feature {
    let mut out = [0f32; FEAT_LEN];
    let c = inset_view(v);
    if c.is_empty() {
        return out;
    }
    let thumb = |blur| {
        let (ox, oy) = ops_for(c, blur);
        apply_rgb(c, &ox, &oy)
    };
    let mut px = thumb(BLUR_AT_70);
    px.iter_mut().zip(thumb(WIDE_AT_70)).for_each(|(x, w)| *x -= w);
    standardize(&mut px, 3);
    out.copy_from_slice(&px);
    out
}

/// 재킷 크롭의 회색조(luma) 특징, 같은 방법을 채널 하나에 적용
pub fn jacket_feature_gray(v: RgbView) -> GrayFeature {
    let mut out = [0f32; GRAY_LEN];
    let c = inset_view(v);
    if c.is_empty() {
        return out;
    }
    let mut luma = Vec::with_capacity(c.width * c.height);
    for y in 0..c.height {
        for p in c.row(y).chunks_exact(3) {
            luma.push(0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32);
        }
    }
    let thumb = |blur| {
        let (ox, oy) = ops_for(c, blur);
        apply_plane(&luma, c.width, c.height, &ox, &oy)
    };
    let mut px = thumb(BLUR_AT_70);
    px.iter_mut().zip(thumb(WIDE_AT_70)).for_each(|(x, w)| *x -= w);
    standardize(&mut px, 1);
    out.copy_from_slice(&px);
    out
}

/// 색이 (거의) 없는 크롭인지, 기준은 [`crate::read::read_crop`]과 같음
pub(crate) fn is_grey(v: RgbView) -> bool {
    inset_view(v).mean_chroma() < crate::read::GREY_CHROMA
}

/// 내적 (정규화된 특징이면 코사인 유사도), 자동 벡터화되게 8개씩 나눠 누적
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let (ac, ar) = a.as_chunks::<8>();
    let (bc, br) = b.as_chunks::<8>();
    let mut acc = [0f32; 8];
    for (x, y) in ac.iter().zip(bc) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let tail: f32 = ar.iter().zip(br).map(|(x, y)| x * y).sum();
    acc.iter().sum::<f32>() + tail
}

/// 기준 재킷 (곡 id마다 특징 벡터 하나 이상)
#[derive(Clone, Default)]
pub struct RefSet {
    ids: Vec<SongId>,
    color: Vec<f32>,
    gray: Vec<f32>,
    by_id: HashMap<SongId, Vec<u32>>,
    /// 재킷 그림이 원래 회색인 곡 (이런 곡은 크롭이 회색이어도 LOCKED 표시가 아님)
    grey_art: HashSet<SongId>,
    skipped: Vec<PathBuf>,
}

impl std::fmt::Debug for RefSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RefSet({} refs, {} ids)",
            self.ids.len(),
            self.by_id.len()
        )
    }
}

impl RefSet {
    /// 빈 세트 (내장 템플릿 없음)
    pub fn new() -> Self {
        Self::default()
    }

    /// 기준 벡터 개수
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// 서로 다른 곡 id 개수
    pub fn song_count(&self) -> usize {
        self.by_id.len()
    }

    /// `id`의 기준이 하나라도 있는지
    pub fn contains(&self, id: SongId) -> bool {
        self.by_id.contains_key(&id)
    }

    /// 마지막 폴더 읽기에서 디코딩 못 한 파일
    pub fn skipped(&self) -> &[PathBuf] {
        &self.skipped
    }

    /// 미리 계산한 특징으로 기준 추가 (게임에서 학습한 크롭 등, [`crate::read::RowRead::feature`])
    /// 학습한 크롭이 djmax.gg 그림보다 잘 맞음
    pub fn add_features(&mut self, id: SongId, color: &Feature, gray: &GrayFeature) {
        self.by_id
            .entry(id)
            .or_default()
            .push(self.ids.len() as u32);
        self.ids.push(id);
        self.color.extend_from_slice(color);
        self.gray.extend_from_slice(gray);
    }

    /// 이미지로 기준 추가 (재킷 그림이나 게임 크롭)
    pub fn add_image(&mut self, id: SongId, v: RgbView) {
        self.add_features(id, &jacket_feature(v), &jacket_feature_gray(v));
        if is_grey(v) {
            self.grey_art.insert(id);
        }
    }

    /// 곡의 재킷 그림이 원래 회색인지 (화면 크롭과 같은 기준, [`crate::read::GREY_CHROMA`])
    pub fn grey_art(&self, id: SongId) -> bool {
        self.grey_art.contains(&id)
    }

    /// 내장 RANDOM(?) 재킷 템플릿을 [`RANDOM_ID`]로 추가 (이미 있으면 그대로 둠)
    pub fn add_builtin_templates(&mut self) {
        if self.contains(RANDOM_ID) {
            return;
        }
        let im = Rgb::decode(RANDOM_JACKET_PNG).expect("embedded RANDOM template decodes");
        self.add_image(RANDOM_ID, im.view());
    }

    /// `dir`의 `<song_id>.webp`(또는 `.png`)를 전부 읽고 내장 템플릿도 추가
    /// 디코딩은 모든 코어에서 돌림
    pub fn load_dir(dir: impl AsRef<Path>) -> io::Result<Self> {
        let files = list_jackets(dir.as_ref())?;
        let mut set = compute_refs(&files);
        set.add_builtin_templates();
        Ok(set)
    }

    /// 검증된 자켓 목록을 처음 읽을 때만 최대 16개 스레드로 특징 계산
    /// `decode`는 WebP 형식을 지정해 디코딩한 128×128 이미지를 반환해야 함
    /// 디코딩 못 한 곡은 건너뛰고 취소 오류는 전체 중단
    pub fn load_jackets(
        jackets: &[(SongId, String)],
        decode: impl Fn(SongId) -> io::Result<Rgb> + Sync,
    ) -> io::Result<Self> {
        let jackets = checked_jackets(jackets)?;
        let nthreads = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(1, 16);
        let chunk = jackets.len().div_ceil(nthreads).max(1);
        let decode = &decode;
        let results: Vec<io::Result<Vec<RefRow>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = jackets
                .chunks(chunk)
                .map(|part| {
                    std::thread::Builder::new().spawn_scoped(scope, move || {
                        let mut rows = Vec::with_capacity(part.len());
                        for &(id, _) in part {
                            let image = match decode(id) {
                                Ok(image) => image,
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                                Err(error) => {
                                    eprintln!("jacket {id}: {error}");
                                    continue;
                                }
                            };
                            if image.width != 128
                                || image.height != 128
                                || image.data.len() != 128 * 128 * 3
                            {
                                return Err(invalid_cache("invalid jacket dimensions"));
                            }
                            rows.push((
                                id,
                                Box::new(jacket_feature(image.view())),
                                Box::new(jacket_feature_gray(image.view())),
                                is_grey(image.view()),
                            ));
                        }
                        Ok(rows)
                    })
                })
                .collect();
            // 오류가 나도 남은 스레드까지 모두 회수
            handles
                .into_iter()
                .map(|handle| {
                    handle?
                        .join()
                        .map_err(|_| io::Error::other("feature worker panicked"))?
                })
                .collect()
        });
        let mut set = Self::new();
        for rows in results {
            for (id, color, gray, grey_art) in rows? {
                set.add_features(id, &color, &gray);
                if grey_art {
                    set.grey_art.insert(id);
                }
            }
        }
        set.add_builtin_templates();
        Ok(set)
    }

    /// [`RefSet::load_dir`]와 같지만 `cache_file`의 바이너리 특징 캐시가 폴더 목록과 맞으면 재사용
    /// id, 자켓 내용 해시, 특징 버전을 비교, 안 맞으면 다시 계산해서 덮어씀 (쓰기 실패는 무시)
    pub fn load_dir_cached(
        dir: impl AsRef<Path>,
        cache_file: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let files = list_jackets(dir.as_ref())?;
        let sig = signature(files.iter().map(|f| (f.id, f.hash.as_str())));
        let mut set = match read_cache(cache_file.as_ref(), &sig) {
            Ok(s) => s,
            Err(_) => {
                let s = compute_refs(&files);
                let _ = write_cache(cache_file.as_ref(), &sig, &s);
                s
            }
        };
        set.add_builtin_templates();
        Ok(set)
    }

    /// 검증된 자켓 목록을 순차 갱신, 내용이 같은 곡의 특징은 그대로 복사
    /// `decode`는 WebP 형식을 지정해 디코딩한 128×128 이미지를 반환해야 함
    /// 디코딩 못 한 곡은 건너뛰고 취소 오류는 전체 중단
    pub fn update_jackets(
        jackets: &[(SongId, String)],
        previous: Option<(&Self, &[(SongId, String)])>,
        cancel: &AtomicBool,
        mut decode: impl FnMut(SongId) -> io::Result<Rgb>,
    ) -> io::Result<Self> {
        let jackets = checked_jackets(jackets)?;
        let old_hashes: HashMap<_, _> = match previous {
            Some((_, old)) => checked_jackets(old)?.into_iter().collect(),
            None => HashMap::new(),
        };
        let mut set = Self::new();
        for (id, hash) in jackets {
            check_cancel(cancel)?;
            if let Some((old, _)) = previous {
                if old_hashes.get(&id).copied() == Some(hash) && set.copy_rows(old, id)? {
                    continue;
                }
            }
            let decoded = decode(id);
            check_cancel(cancel)?;
            let image = match decoded {
                Ok(image) => image,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(error) => {
                    eprintln!("jacket {id}: {error}");
                    continue;
                }
            };
            if image.width != 128 || image.height != 128 || image.data.len() != 128 * 128 * 3 {
                return Err(invalid_cache("invalid jacket dimensions"));
            }
            set.add_image(id, image.view());
        }
        check_cancel(cancel)?;
        if !set.contains(RANDOM_ID) {
            let old_builtin =
                previous.filter(|(_, hashes)| !hashes.iter().any(|(id, _)| *id == RANDOM_ID));
            if !matches!(old_builtin, Some((old, _)) if set.copy_rows(old, RANDOM_ID)?) {
                set.add_builtin_templates();
            }
        }
        Ok(set)
    }

    fn copy_rows(&mut self, source: &Self, id: SongId) -> io::Result<bool> {
        let Some(rows) = source.by_id.get(&id) else {
            return Ok(false);
        };
        let count = self
            .len()
            .checked_add(rows.len())
            .ok_or_else(|| invalid_cache("too many feature rows"))?;
        cache_len(count)?;
        for row in rows {
            let row = usize::try_from(*row).map_err(|_| invalid_cache("invalid feature row"))?;
            let c = row
                .checked_mul(FEAT_LEN)
                .ok_or_else(|| invalid_cache("invalid feature row"))?;
            let g = row
                .checked_mul(GRAY_LEN)
                .ok_or_else(|| invalid_cache("invalid feature row"))?;
            let color: &Feature = source
                .color
                .get(
                    c..c.checked_add(FEAT_LEN)
                        .ok_or_else(|| invalid_cache("invalid feature row"))?,
                )
                .and_then(|values| values.try_into().ok())
                .ok_or_else(|| invalid_cache("invalid feature row"))?;
            let gray: &GrayFeature = source
                .gray
                .get(
                    g..g.checked_add(GRAY_LEN)
                        .ok_or_else(|| invalid_cache("invalid feature row"))?,
                )
                .and_then(|values| values.try_into().ok())
                .ok_or_else(|| invalid_cache("invalid feature row"))?;
            self.add_features(id, color, gray);
        }
        if source.grey_art(id) {
            self.grey_art.insert(id);
        }
        Ok(!rows.is_empty())
    }

    /// 특징 버전과 자켓 내용 서명이 맞는 캐시를 읽고 내장 자켓을 추가
    pub fn from_cache_bytes(bytes: &[u8], signature: &str) -> io::Result<Self> {
        validate_signature(signature)?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err(invalid_cache("feature cache is too large"));
        }
        let mut cursor = bytes;
        if &take::<8>(&mut cursor)? != CACHE_MAGIC
            || u32::from_le_bytes(take(&mut cursor)?) != FEATURE_VERSION
            || take::<64>(&mut cursor)?.as_slice() != signature.as_bytes()
        {
            return Err(invalid_cache("feature cache signature mismatch"));
        }
        let count = usize::try_from(u32::from_le_bytes(take(&mut cursor)?))
            .map_err(|_| invalid_cache("invalid feature count"))?;
        if bytes.len() != cache_len(count)? {
            return Err(invalid_cache("invalid feature cache length"));
        }
        let mut set = Self::new();
        let mut color = Box::new([0.0; FEAT_LEN]);
        let mut gray = Box::new([0.0; GRAY_LEN]);
        for _ in 0..count {
            let id = u32::from_le_bytes(take(&mut cursor)?);
            let grey_art = match take::<1>(&mut cursor)? {
                [0] => false,
                [1] => true,
                _ => return Err(invalid_cache("invalid feature flag")),
            };
            for value in color.iter_mut().chain(gray.iter_mut()) {
                *value = f32::from_le_bytes(take(&mut cursor)?);
                if !value.is_finite() {
                    return Err(invalid_cache("invalid feature value"));
                }
            }
            set.add_features(id, &color, &gray);
            if grey_art {
                set.grey_art.insert(id);
            }
        }
        set.add_builtin_templates();
        Ok(set)
    }

    /// 다음 실행에서 재사용할 특징 캐시 바이트
    pub fn to_cache_bytes(&self, signature: &str) -> io::Result<Vec<u8>> {
        validate_signature(signature)?;
        let count = self.len();
        let len = cache_len(count)?;
        if count.checked_mul(FEAT_LEN) != Some(self.color.len())
            || count.checked_mul(GRAY_LEN) != Some(self.gray.len())
        {
            return Err(invalid_cache("invalid feature rows"));
        }
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(CACHE_MAGIC);
        bytes.extend_from_slice(&FEATURE_VERSION.to_le_bytes());
        bytes.extend_from_slice(signature.as_bytes());
        bytes.extend_from_slice(
            &u32::try_from(count)
                .map_err(|_| invalid_cache("too many feature rows"))?
                .to_le_bytes(),
        );
        for ((id, color), gray) in self
            .ids
            .iter()
            .zip(self.color.chunks_exact(FEAT_LEN))
            .zip(self.gray.chunks_exact(GRAY_LEN))
        {
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.push(u8::from(self.grey_art(*id)));
            for value in color.iter().chain(gray) {
                if !value.is_finite() {
                    return Err(invalid_cache("invalid feature value"));
                }
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        Ok(bytes)
    }

    fn top_from(&self, k: usize, scores: impl Iterator<Item = f32>) -> Vec<Candidate> {
        let mut top: Vec<Candidate> = Vec::with_capacity(k + 1);
        if k == 0 {
            return top;
        }
        for (i, s) in scores.enumerate() {
            let id = self.ids[i];
            if let Some(p) = top.iter().position(|c| c.id == id) {
                if s <= top[p].score {
                    continue;
                }
                top.remove(p);
            } else if top.len() == k && s <= top[k - 1].score {
                continue;
            }
            let pos = top.iter().position(|c| s > c.score).unwrap_or(top.len());
            top.insert(pos, Candidate { id, score: s });
            top.truncate(k);
        }
        top
    }

    /// 컬러 특징과 가장 비슷한 서로 다른 곡 id `k`개, 높은 순
    pub fn topk(&self, f: &Feature, k: usize) -> Vec<Candidate> {
        self.top_from(k, self.color.chunks_exact(FEAT_LEN).map(|r| dot(r, f)))
    }

    /// 회색조 특징과 가장 비슷한 서로 다른 곡 id `k`개, 높은 순
    pub fn topk_gray(&self, g: &GrayFeature, k: usize) -> Vec<Candidate> {
        self.top_from(k, self.gray.chunks_exact(GRAY_LEN).map(|r| dot(r, g)))
    }

    /// 컬러 특징과 특정 곡 하나의 최고 유사도 (모르는 id면 `None`)
    pub fn score_id(&self, f: &Feature, id: SongId) -> Option<f32> {
        let idx = self.by_id.get(&id)?;
        idx.iter()
            .map(|&i| {
                dot(
                    &self.color[i as usize * FEAT_LEN..(i as usize + 1) * FEAT_LEN],
                    f,
                )
            })
            .reduce(f32::max)
    }
}

struct JacketFile {
    id: SongId,
    path: PathBuf,
    hash: String,
}

fn list_jackets(dir: &Path) -> io::Result<Vec<JacketFile>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        let ext = path
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("webp") | Some("png")) {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<SongId>().ok())
        else {
            continue;
        };
        let mut file = fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let size = file.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            hasher.update(
                buffer
                    .get(..size)
                    .ok_or_else(|| invalid_cache("invalid jacket read"))?,
            );
        }
        out.push(JacketFile {
            id,
            path,
            hash: format!("{:x}", hasher.finalize()),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id).then(a.path.cmp(&b.path)));
    Ok(out)
}

type RefRow = (SongId, Box<Feature>, Box<GrayFeature>, bool);

fn compute_refs(files: &[JacketFile]) -> RefSet {
    let nthreads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 16);
    let chunk = files.len().div_ceil(nthreads).max(1);
    let results: Vec<(Vec<RefRow>, Vec<PathBuf>)> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    let mut ok = Vec::with_capacity(part.len());
                    let mut bad = Vec::new();
                    for f in part {
                        match fs::read(&f.path).ok().and_then(|b| Rgb::decode(&b).ok()) {
                            Some(im) => ok.push((
                                f.id,
                                Box::new(jacket_feature(im.view())),
                                Box::new(jacket_feature_gray(im.view())),
                                is_grey(im.view()),
                            )),
                            None => bad.push(f.path.clone()),
                        }
                    }
                    (ok, bad)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("feature worker panicked"))
            .collect()
    });
    let mut set = RefSet::new();
    for (ok, bad) in results {
        for (id, c, g, grey) in ok {
            set.add_features(id, &c, &g);
            if grey {
                set.grey_art.insert(id);
            }
        }
        set.skipped.extend(bad);
    }
    set
}

/// 자켓 4096곡과 내장 자켓의 특징이 들어가는 상한
pub const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_MAGIC: &[u8; 8] = b"PKREFS03";
const CACHE_HEADER_BYTES: usize = 80;
const CACHE_ROW_BYTES: usize = 5 + 4 * (FEAT_LEN + GRAY_LEN);

fn invalid_cache(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn validate_signature(signature: &str) -> io::Result<()> {
    if signature.len() == 64
        && signature
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        Ok(())
    } else {
        Err(invalid_cache("invalid feature signature"))
    }
}

fn checked_jackets(jackets: &[(SongId, String)]) -> io::Result<Vec<(SongId, &str)>> {
    if jackets.len() > 4096 {
        return Err(invalid_cache("too many jackets"));
    }
    let mut out = Vec::with_capacity(jackets.len());
    let mut seen = HashSet::new();
    for (id, hash) in jackets {
        validate_signature(hash)?;
        if !seen.insert(*id) {
            return Err(invalid_cache("duplicate jacket id"));
        }
        out.push((*id, hash.as_str()));
    }
    out.sort_unstable_by_key(|(id, _)| *id);
    Ok(out)
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "feature update cancelled",
        ))
    } else {
        Ok(())
    }
}

/// 정렬한 자켓 목록의 JSON 해시와 특징 버전을 묶은 서명
pub fn jacket_signature(jackets: &[(SongId, String)]) -> io::Result<String> {
    Ok(signature(checked_jackets(jackets)?))
}

fn signature<'a>(jackets: impl IntoIterator<Item = (SongId, &'a str)>) -> String {
    let mut set = Sha256::new();
    set.update(b"[");
    for (index, (id, hash)) in jackets.into_iter().enumerate() {
        if index != 0 {
            set.update(b",")
        }
        set.update(format!("[{id},\"{hash}\"]").as_bytes());
    }
    set.update(b"]");
    let mut result = Sha256::new();
    result.update(FEATURE_VERSION.to_le_bytes());
    result.update(format!("{:x}", set.finalize()).as_bytes());
    format!("{:x}", result.finalize())
}

fn cache_len(count: usize) -> io::Result<usize> {
    count
        .checked_mul(CACHE_ROW_BYTES)
        .and_then(|size| size.checked_add(CACHE_HEADER_BYTES))
        .filter(|size| *size <= MAX_CACHE_BYTES)
        .ok_or_else(|| invalid_cache("feature cache is too large"))
}

fn take<const N: usize>(cursor: &mut &[u8]) -> io::Result<[u8; N]> {
    let value = cursor
        .get(..N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| invalid_cache("truncated feature cache"))?;
    *cursor = cursor
        .get(N..)
        .ok_or_else(|| invalid_cache("truncated feature cache"))?;
    Ok(value)
}

fn write_cache(path: &Path, sig: &str, set: &RefSet) -> io::Result<()> {
    let buf = set.to_cache_bytes(sig)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &buf)?;
    fs::rename(&tmp, path)
}

fn read_cache(path: &Path, sig: &str) -> io::Result<RefSet> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_CACHE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    RefSet::from_cache_bytes(&bytes, sig)
}
