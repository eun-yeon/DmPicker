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
use std::io;
use std::path::{Path, PathBuf};

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

    /// [`RefSet::load_dir`]와 같지만 `cache_file`의 바이너리 특징 캐시가 폴더 목록과 맞으면 재사용
    /// id, 파일 크기, mtime, 특징 버전을 비교, 안 맞으면 다시 계산해서 덮어씀 (쓰기 실패는 무시)
    pub fn load_dir_cached(
        dir: impl AsRef<Path>,
        cache_file: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let files = list_jackets(dir.as_ref())?;
        let sig = signature(&files);
        let mut set = match read_cache(cache_file.as_ref(), sig) {
            Some(s) => s,
            None => {
                let s = compute_refs(&files);
                let _ = write_cache(cache_file.as_ref(), sig, &s);
                s
            }
        };
        set.add_builtin_templates();
        Ok(set)
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
    size: u64,
    mtime_ns: u128,
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
        let md = e.metadata()?;
        let mtime_ns = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        out.push(JacketFile {
            id,
            path,
            size: md.len(),
            mtime_ns,
        });
    }
    out.sort_by_key(|f| f.id);
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

/// 캐시 형식이 바뀌면 올림 (02: 기준마다 회색 그림 표시 추가)
const CACHE_MAGIC: &[u8; 8] = b"PKREFS02";

fn signature(files: &[JacketFile]) -> u64 {
    // 모든 파일의 (버전, id, 크기, mtime)을 FNV-1a로 해시
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(&FEATURE_VERSION.to_le_bytes());
    eat(&(FEAT_SIZE as u32).to_le_bytes());
    for f in files {
        eat(&f.id.to_le_bytes());
        eat(&f.size.to_le_bytes());
        eat(&f.mtime_ns.to_le_bytes());
    }
    h
}

fn write_cache(path: &Path, sig: u64, set: &RefSet) -> io::Result<()> {
    let n = set.len();
    let mut buf = Vec::with_capacity(24 + n * (5 + 4 * (FEAT_LEN + GRAY_LEN)));
    buf.extend_from_slice(CACHE_MAGIC);
    buf.extend_from_slice(&FEATURE_VERSION.to_le_bytes());
    buf.extend_from_slice(&sig.to_le_bytes());
    buf.extend_from_slice(&(n as u32).to_le_bytes());
    for i in 0..n {
        buf.extend_from_slice(&set.ids[i].to_le_bytes());
        buf.push(set.grey_art(set.ids[i]) as u8);
        for v in &set.color[i * FEAT_LEN..(i + 1) * FEAT_LEN] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        for v in &set.gray[i * GRAY_LEN..(i + 1) * GRAY_LEN] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &buf)?;
    fs::rename(&tmp, path)
}

fn read_cache(path: &Path, sig: u64) -> Option<RefSet> {
    let b = fs::read(path).ok()?;
    if b.len() < 24 || &b[0..8] != CACHE_MAGIC {
        return None;
    }
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    if u32_at(8) != FEATURE_VERSION || u64::from_le_bytes(b[12..20].try_into().unwrap()) != sig {
        return None;
    }
    let n = u32_at(20) as usize;
    let rec = 5 + 4 * (FEAT_LEN + GRAY_LEN);
    if b.len() != 24 + n * rec {
        return None;
    }
    let mut set = RefSet::new();
    let mut c = Box::new([0f32; FEAT_LEN]);
    let mut g = Box::new([0f32; GRAY_LEN]);
    for i in 0..n {
        let o = 24 + i * rec;
        let id = u32_at(o);
        let fl = |k: usize| f32::from_le_bytes(b[k..k + 4].try_into().unwrap());
        for (j, v) in c.iter_mut().enumerate() {
            *v = fl(o + 5 + 4 * j);
        }
        for (j, v) in g.iter_mut().enumerate() {
            *v = fl(o + 5 + 4 * FEAT_LEN + 4 * j);
        }
        set.add_features(id, &c, &g);
        if b[o + 4] != 0 {
            set.grey_art.insert(id);
        }
    }
    Some(set)
}
