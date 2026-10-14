//! 화면 인식(dmpicker-engine), 경로 계획·실행(dmpicker-nav), 사용자별 저장 목록을 잇는 부분
//! 작업 스레드에서 돌고 게임 프로세스는 건드리지 않음

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dmpicker_engine::capture::{self, CaptureMethod, Hwnd};
use dmpicker_engine::input::{self, Key as EKey, PopupKey};
use dmpicker_engine::state::{classify, layout_kind};
use dmpicker_engine::{features, read_window, room, sort, tabs, Candidate, Feature, ListGeom, RefSet, RowRead, ScreenKind, FEAT_LEN, RANDOM_ID};
use dmpicker_nav::{order_key, Abort, ButtonMode, Entry, GameIo, Key as NKey, ListModel, NavConfig, NavError, Navigator, Observation, OrderKey, TimingTuner};
use dmpicker_search::Diff;
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};

use crate::hands::Hands;
use crate::hotkey::Hotkey;
use crate::pace::{Pace, SAFE_MS};

/// `eprintln!` 대신 씀, 작업 중이면 실패 기록에 넣을 진단으로도 모임
macro_rules! diagnostic {
    ($($arg:tt)*) => { crate::fail_log::line(format_args!($($arg)*)) };
}

const ALIGN_SIM: f32 = 0.85;
/// 저장된 게임 내 특징이 계산 방식과 안 맞게 되면 `dmpicker_engine::features::FEATURE_VERSION`과 같이 올림
const LEARN_MAGIC: &[u8; 8] = b"PICKLST3";

// ------------------------------------------------------------------ 저장 목록

#[derive(Clone)]
pub struct LEntry {
    pub id: Option<u32>,
    pub random: bool,
    pub locked: bool,
    pub feat: Vec<i8>, // 게임 화면 재킷 특징, 양자화함
}

pub struct Learned {
    pub kind: ScreenKind,
    pub entries: Vec<LEntry>,
}

/// 저장 특징의 배율, 특징 원소는 +-0.25 안에 들어옴
/// 1/24이면 1할 가까이 잘려서 줄이 자기 저장 특징과도 0.6-0.7밖에 안 맞을 수 있음
const QUANT: f32 = 127.0 * 4.0;

fn quant(f: &Feature) -> Vec<i8> {
    f.iter().map(|v| (v * QUANT).round().clamp(-127.0, 127.0) as i8).collect()
}

fn qdot(f: &Feature, q: &[i8]) -> f32 {
    let mut s = 0.0f32;
    for (a, b) in f.iter().zip(q) {
        s += a * (*b as f32);
    }
    s / QUANT
}

pub fn kind_name(k: ScreenKind) -> &'static str {
    match k {
        ScreenKind::OnlineList => "online",
        _ => "freestyle",
    }
}

fn other_kind(k: ScreenKind) -> ScreenKind {
    if k == ScreenKind::OnlineList { ScreenKind::FreestyleList } else { ScreenKind::OnlineList }
}

impl Learned {
    pub fn path(dir: &Path, k: ScreenKind) -> PathBuf {
        dir.join(format!("learned_{}.bin", kind_name(k)))
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let mut b = Vec::with_capacity(16 + self.entries.len() * (FEAT_LEN + 8));
        b.extend_from_slice(LEARN_MAGIC);
        b.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for e in &self.entries {
            b.extend_from_slice(&e.id.map(|v| v as i64).unwrap_or(-1).to_le_bytes());
            b.push(e.random as u8 | (e.locked as u8) << 1);
            b.extend(e.feat.iter().map(|v| *v as u8));
        }
        crate::sys::save_file(&Self::path(dir, self.kind), &b)
    }

    pub fn load(dir: &Path, k: ScreenKind) -> Option<Self> {
        let b = std::fs::read(Self::path(dir, k)).ok()?;
        if b.len() < 12 || &b[..8] != LEARN_MAGIC {
            return None;
        }
        let n = u32::from_le_bytes(b[8..12].try_into().ok()?) as usize;
        let rec = 8 + 1 + FEAT_LEN;
        if b.len() != 12 + n * rec {
            return None;
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let r = &b[12 + i * rec..12 + (i + 1) * rec];
            let id = i64::from_le_bytes(r[..8].try_into().ok()?);
            let fl = r[8];
            let feat: Vec<i8> = r[9..].iter().map(|v| *v as i8).collect();
            entries.push(LEntry { id: (id >= 0).then_some(id as u32), random: fl & 1 != 0, locked: fl & 2 != 0, feat });
        }
        Some(Learned { kind: k, entries })
    }

    pub fn index_of(&self, id: u32) -> Option<usize> {
        self.entries.iter().position(|e| e.id == Some(id))
    }

    /// 다른 모드에서 보이는 같은 목록
    /// 온라인은 RANDOM이 없고, 보유 팩의 아직 안 푼 히든곡(LOCKED)이 프리스타일과 달리 목록에 나옴
    /// 프리스타일 목록으로 알 수 없는 LOCKED 곡은 온라인에서 이동하며 익힘 ([`LiveIo`])
    pub fn derive(&self, title_of: &dyn Fn(u32) -> String) -> Result<Learned, String> {
        if self.kind != ScreenKind::OnlineList {
            let entries = self.entries.iter().filter(|e| !e.random).cloned().collect();
            return Ok(Learned { kind: ScreenKind::OnlineList, entries });
        }
        let entries = self.entries.iter().filter(|e| !e.locked).cloned().collect();
        let mut l = Learned { kind: ScreenKind::FreestyleList, entries };
        // RANDOM은 목록 시작 칸 바로 앞에 있음
        let at = l.model(title_of)?.reset_pos();
        l.entries.insert(at, LEntry { id: None, random: true, locked: false, feat: vec![0; FEAT_LEN] });
        Ok(l)
    }

    /// 새 프리스타일 인식에서 막 만든 온라인 목록에 예전 온라인 목록이 익힌 LOCKED 곡을 유지함
    /// 각 곡은 예전과 같은 곡 뒤에 넣음
    fn carry_locked(&mut self, old: &Learned) {
        let mut prev = None;
        for e in &old.entries {
            let Some(id) = e.id else { continue };
            if e.locked && self.index_of(id).is_none() {
                let Some(at) = prev.and_then(|p| self.index_of(p)) else { continue };
                self.entries.insert(at + 1, e.clone());
            }
            prev = Some(id);
        }
    }

    /// 이 목록으로 다른 모드 목록을 만들어 저장
    fn save_derived(&self, store: &Path, title_of: &dyn Fn(u32) -> String) -> Result<(), String> {
        let mut d = self.derive(title_of)?;
        if d.kind == ScreenKind::OnlineList {
            if let Some(old) = Learned::load(store, ScreenKind::OnlineList) {
                d.carry_locked(&old);
            }
        }
        d.save(store).map_err(|e| e.to_string())?;
        diagnostic!("derived {} list: {} entries", kind_name(d.kind), d.entries.len());
        Ok(())
    }

    pub fn model(&self, title_of: &dyn Fn(u32) -> String) -> Result<ListModel, String> {
        let es: Vec<Entry> = self
            .entries
            .iter()
            .map(|e| Entry {
                song: e.id,
                title: e.id.map(title_of).unwrap_or_default(),
                locked: e.locked || (e.id.is_none() && !e.random),
            })
            .collect();
        ListModel::from_entries(es).map_err(|e| format!("{e:?}"))
    }
}

/// 인식한 그대로의 목록과 칸마다의 다른 후보들 (보정용)
pub struct PendingLearned {
    pub learned: Learned,
    cands: Vec<Vec<Candidate>>,
    /// 두 바퀴에서 서로 다른 곡으로 읽힌 칸
    doubtful: Vec<usize>,
}

impl PendingLearned {
    /// 목록 순서로 오인식을 고침, 제목이 자기 그룹 구간에 안 맞는 칸은 맞는 후보 중 가장 좋은 걸로 바꿈
    /// 중복은 가장 그럴듯한 하나만 남김
    /// 다시 볼 만한 칸(여기서 바뀐 칸, 두 가지로 읽힌 칸, 모르는 칸)도 같이 돌려줌
    pub fn repair(mut self, title_of: &dyn Fn(u32) -> String) -> (Learned, Vec<usize>) {
        // 두 가지로 읽힌 칸(두 곡, 또는 곡과 RANDOM)은 멈춘 화면에서 다시 볼 때까지 모름으로 둠
        // 틀린 곡이 더 나쁨
        for &i in &self.doubtful {
            let e = &mut self.learned.entries[i];
            e.id = None;
            e.random = false;
        }
        let mut changed: std::collections::BTreeSet<usize> = self.doubtful.iter().copied().collect();
        for round in 0..3 {
            let Ok(model) = self.learned.model(title_of) else { break };
            let anomalies: Vec<(usize, dmpicker_nav::Group)> = model.anomalies().iter().map(|a| (a.index, a.assigned)).collect();
            if anomalies.is_empty() {
                break;
            }
            for (i, group) in &anomalies {
                let alt = self.cands[*i].iter().skip(1).find(|c| {
                    c.id != RANDOM_ID && c.score >= 0.45 && dmpicker_nav::Group::of_title(&title_of(c.id)) == Some(*group)
                });
                self.learned.entries[*i].id = alt.map(|c| c.id);
                changed.insert(*i);
            }
            diagnostic!("repair round {round}: {} anomalies", anomalies.len());
        }
        // 중복 id는 가장 잘 읽힌 칸만 남기고 나머지는 모름으로
        let mut best: HashMap<u32, (usize, f32)> = HashMap::new();
        for (i, e) in self.learned.entries.iter().enumerate() {
            if let Some(id) = e.id {
                let sc = self.cands[i].iter().find(|c| c.id == id).map(|c| c.score).unwrap_or(0.0);
                let ent = best.entry(id).or_insert((i, sc));
                if sc > ent.1 {
                    *ent = (i, sc);
                }
            }
        }
        for (i, e) in self.learned.entries.iter_mut().enumerate() {
            if let Some(id) = e.id {
                if best[&id].0 != i {
                    e.id = None;
                }
            }
            if e.id.is_none() && !e.random && !e.locked {
                changed.insert(i);
            }
        }
        (self.learned, changed.into_iter().collect())
    }
}

/// 제목 순서에서 칸의 위치, 앞뒤로 가장 가까운 아는 곡 사이
struct Bounds {
    lo: OrderKey,
    hi: OrderKey,
}

impl Bounds {
    fn fits(&self, title: &str) -> bool {
        order_key(title).is_some_and(|k| self.lo <= k && k <= self.hi)
    }
}

impl Learned {
    /// 칸 `i`의 제목 순서 범위, [`dmpicker_nav::order_key`] 순서를 따르는 영문·한글 그룹만
    /// 한자와 기호는 순서가 따로 있음
    fn bounds(&self, i: usize, titles: &HashMap<u32, String>) -> Option<Bounds> {
        use dmpicker_nav::Group;
        let n = self.entries.len();
        let near = |step: usize| (1..n).map(|d| &self.entries[(i + step * d) % n]).find_map(|e| e.id);
        let (lo, hi) = (order_key(titles.get(&near(n - 1)?)?)?, order_key(titles.get(&near(1)?)?)?);
        [lo.0, hi.0].iter().all(|g| matches!(g, Group::Letter(_) | Group::Hangul)).then_some(Bounds { lo, hi })
    }

    /// 멈춘 화면에서 읽은 칸 `i`의 곡
    /// 제목 순서에 맞고 다른 칸에 없는 후보 중 가장 강하고, 그다음 후보보다 확실히 앞서야 함
    /// 곡 데이터에 없는 곡(새 DLC)은 잘 맞는 게 없어서 모름으로 남음
    fn confirm(&self, i: usize, row: &RowRead, titles: &HashMap<u32, String>) -> Option<u32> {
        let Some(b) = self.bounds(i, titles) else {
            return row.top_id().filter(|&id| row.sure && self.free(i, id));
        };
        let (best, second) = self.in_order(i, row, titles, &b);
        let best = best?;
        (best.score >= 0.6 && best.score - second >= dmpicker_engine::read::SURE_MARGIN).then_some(best.id)
    }

    /// 칸 `i`가 곡 `id`일 수 있음, 다른 칸이 그 곡이 아니고 RANDOM도 아님
    fn free(&self, i: usize, id: u32) -> bool {
        id != RANDOM_ID && !self.entries.iter().enumerate().any(|(j, e)| j != i && e.id == Some(id))
    }

    /// 칸 `i`에 대해 제목 순서 `b`에 맞고 다른 칸에 없는 가장 강한 후보와 그다음 후보의 점수
    fn in_order(&self, i: usize, row: &RowRead, titles: &HashMap<u32, String>, b: &Bounds) -> (Option<Candidate>, f32) {
        let mut fit = row.cands.iter().filter(|c| self.free(i, c.id) && titles.get(&c.id).is_some_and(|t| b.fits(t)));
        (fit.next().copied(), fit.next().map_or(0.0, |c| c.score))
    }

    /// [`Learned::confirm`]이 칸 `i`에서 아무것도 안 받은 이유, 로그용
    /// 칸의 곡, 후보들(순서 밖이거나 다른 칸의 곡), 순서로 걸러진 결과
    fn unconfirmed(&self, i: usize, row: &RowRead, titles: &HashMap<u32, String>) -> String {
        let name = |id: u32| titles.get(&id).cloned().unwrap_or_else(|| format!("#{id}"));
        let b = self.bounds(i, titles);
        let cands: Vec<String> = row
            .cands
            .iter()
            .map(|c| {
                let other = (0..self.entries.len()).find(|&j| j != i && self.entries[j].id == Some(c.id));
                let out = b.as_ref().is_some_and(|b| !titles.get(&c.id).is_some_and(|t| b.fits(t)));
                let note = match (other, out) {
                    (Some(j), _) => format!(" (entry {j}'s)"),
                    (None, true) => " (out of order)".to_string(),
                    _ => String::new(),
                };
                format!("{} {:.3}{note}", name(c.id), c.score)
            })
            .collect();
        let order = match &b {
            Some(b) => {
                let (best, second) = self.in_order(i, row, titles, b);
                format!("order {:?} .. {:?}: best {:.3}, next {:.3}", b.lo, b.hi, best.map_or(0.0, |c| c.score), second)
            }
            None => "no order bounds, a sure read needed".to_string(),
        };
        let has = self.entries[i].id.map_or("none".to_string(), name);
        format!("has {has}; read {}, {}; {order}", cands.join(", "), if row.sure { "sure" } else { "not sure" })
    }

    /// 멈춘 화면에서 읽은 칸 `i`를 반영함, 목록에 RANDOM이 아직 없으면 RANDOM으로
    /// 읽은 1순위가 칸의 기존 곡과 같으면 그대로 둠, 두 번 같게 읽힌 걸 제목 순서로 뒤집지 않음
    /// 순서 범위를 정하는 이웃이 오인식 칸일 수 있음 (제목 순서가 뒤집힌 곳은 양옆을 다시 확인함)
    /// 나머지는 [`Learned::confirm`]이 정함, 반영했는지를 돌려줌
    fn reread(&mut self, i: usize, row: &RowRead, titles: &HashMap<u32, String>) -> bool {
        if row.is_random && !self.entries.iter().any(|e| e.random) {
            diagnostic!("recheck {i}: RANDOM");
            let e = &mut self.entries[i];
            (e.random, e.id, e.locked) = (true, None, false);
            return true;
        }
        let id = match self.entries[i].id {
            Some(have) if row.top_id() == Some(have) => Some(have),
            _ => self.confirm(i, row, titles),
        };
        let Some(id) = id else { return false };
        diagnostic!("recheck {i}: {}", titles.get(&id).map_or("?", String::as_str));
        self.entries[i].id = Some(id);
        self.entries[i].feat = quant(&row.feature);
        true
    }

    /// 회색으로 읽혀 LOCKED로 본 칸이 원래 회색 재킷인 곡이면 LOCKED가 아님
    fn unlock_grey_art(&mut self, refs: &RefSet) {
        for e in &mut self.entries {
            if e.locked && e.id.is_some_and(|id| refs.grey_art(id)) {
                e.locked = false;
            }
        }
    }

    /// `song`이 목록엔 없지만 모르는 칸 중 하나일 수 있음
    pub fn might_hold(&self, song: u32, titles: &HashMap<u32, String>) -> bool {
        let Some(t) = titles.get(&song) else { return false };
        (0..self.entries.len()).any(|i| {
            let e = &self.entries[i];
            e.id.is_none() && !e.random && self.bounds(i, titles).is_some_and(|b| b.fits(t))
        })
    }
}

// ------------------------------------------------------------------ 화면 도우미

pub struct Screen {
    pub game: Hwnd,
}

impl Screen {
    /// 전용 전체 화면에선 PrintWindow가 앞으로 가져온 순간 화면에 멈춰 있어서 화면에서 직접 찍음
    /// 최소화에서 펴지는 중에 모드가 바뀌기도 해서 찍을 때마다 봄
    pub fn grab(&self) -> Result<dmpicker_engine::Rgb, Abort> {
        let method = if full_screen() { CaptureMethod::ScreenBlt } else { CaptureMethod::PrintWindow };
        capture::capture(self.game, method).map_err(|e| Abort::Io(format!("{e:?}")))
    }

    /// 목록이 멈출 때까지 기다린 뒤 그 프레임과 목록 위치를 돌려줌
    /// 줄이 멈춘 자리 격자에 붙은 프레임이 연속 둘이어야 함
    pub fn settled(&self, cancel: &AtomicBool, timeout: Duration) -> Result<Option<(dmpicker_engine::Rgb, ListGeom)>, Abort> {
        self.wait_rest(cancel, timeout, &mut Waits::default(), true)
    }

    /// 인식 본 루프용 [`Screen::settled`], 시간이 어디에 쓰였는지 합산함
    /// 멈춘 자리는 안 봄, 창을 내용으로 이어 붙여서 반 줄 안쪽으로 밀린 창도 그대로 씀
    pub fn settled_timed(&self, cancel: &AtomicBool, timeout: Duration, w: &mut Waits) -> Result<Option<(dmpicker_engine::Rgb, ListGeom)>, Abort> {
        self.wait_rest(cancel, timeout, w, false)
    }

    fn wait_rest(&self, cancel: &AtomicBool, timeout: Duration, w: &mut Waits, on_grid: bool) -> Result<Option<(dmpicker_engine::Rgb, ListGeom)>, Abort> {
        let t0 = Instant::now();
        let mut ok = 0;
        let nap = |w: &mut Waits, ms: u64| {
            let t = Instant::now();
            std::thread::sleep(Duration::from_millis(ms));
            w.sleep += t.elapsed();
        };
        nap(w, 40);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Abort::Cancelled);
            }
            let t = Instant::now();
            let img = self.grab()?;
            let grabbed = Instant::now();
            let info = dmpicker_engine::analyze(&img);
            w.grab += grabbed - t;
            w.judge += grabbed.elapsed();
            w.frames += 1;
            if let Some(geom) = info.list {
                if info.at_rest && (!on_grid || geom.on_grid()) {
                    ok += 1;
                    if ok >= 2 {
                        return Ok(Some((img, geom)));
                    }
                } else {
                    ok = 0;
                }
            } else {
                ok = 0;
            }
            if t0.elapsed() > timeout {
                return Ok(None);
            }
            nap(w, 15);
        }
    }
}

/// [`Screen::settled_timed`]에 든 시간: 프레임 캡처, 목록이 멈췄는지 판정, 프레임 사이 대기
#[derive(Default)]
pub struct Waits {
    pub grab: Duration,
    pub judge: Duration,
    pub sleep: Duration,
    pub frames: u32,
}

fn press_engine(game: Hwnd, k: NKey, hold: u32, gap: u32) -> Result<(), Abort> {
    let (h, g) = (hold as u64, gap as u64);
    let r = match k {
        NKey::Up => input::press(game, EKey::Up, h, g),
        NKey::Down => input::press(game, EKey::Down, h, g),
        NKey::PgUp => input::press(game, EKey::PageUp, h, g),
        NKey::PgDn => input::press(game, EKey::PageDown, h, g),
        NKey::Left => input::press(game, EKey::Left, h, g),
        NKey::Right => input::press(game, EKey::Right, h, g),
        NKey::Letter(c) => match EKey::letter(c) {
            Some(k) => input::press(game, k, h, g),
            None => return Err(Abort::Io(format!("bad letter {c}"))),
        },
        NKey::Mode(m) => {
            let pad = match m {
                ButtonMode::B4 => EKey::Numpad4,
                ButtonMode::B5 => EKey::Numpad5,
                ButtonMode::B6 => EKey::Numpad6,
                ButtonMode::B8 => EKey::Numpad8,
            };
            input::combo(game, &[EKey::LCtrl, pad], h, g)
        }
    };
    r.map_err(|_| Abort::FocusLost)
}

// ------------------------------------------------------------------ 커서 위치

fn row_matches(row: &RowRead, e: &LEntry) -> bool {
    if e.random {
        return row.is_random;
    }
    if let Some(id) = e.id {
        if row.cands.iter().take(3).any(|c| c.id == id && c.score >= 0.6) {
            return true;
        }
    }
    !e.feat.is_empty() && qdot(&row.feature, &e.feat) >= ALIGN_SIM
}

/// `m[k][i]`: 화면 `k`번째 줄이 저장 목록 `i`번 칸을 보여줌
fn match_table(rows: &[RowRead; 11], learned: &Learned) -> Vec<Vec<bool>> {
    rows.iter().map(|r| learned.entries.iter().map(|e| row_matches(r, e)).collect()).collect()
}

/// 커서 찾기, 11칸 창이 화면과 가장 잘 맞는 저장 목록 위치
/// 확실할 때만 그 위치를 주고, 맞는 줄 수도 같이 돌려줌
fn locate(m: &[Vec<bool>]) -> (Option<usize>, usize) {
    let n = m[0].len();
    if n == 0 {
        return (None, 0);
    }
    let mut best = (0usize, 0usize);
    let mut second = 0usize;
    for c in 0..n {
        let agree = (0..m.len()).filter(|&k| m[k][(c + n * 2 + k - dmpicker_engine::SEL_ROW) % n]).count();
        if agree > best.1 {
            second = best.1;
            best = (c, agree);
        } else if agree > second {
            second = agree;
        }
    }
    ((best.1 >= 8 && best.1 >= second + 3).then_some(best.0), best.1)
}

/// 화면 창이 저장 목록(제목 순서를 따름) 안에 자리가 있음
fn placed(rows: &[RowRead; 11], learned: &Learned) -> bool {
    locate(&match_table(rows, learned)).0.is_some()
}

/// `a` 바로 다음의 `b`가 제목 순서상 거꾸로 감
/// 한자와 기호는 순서가 따로 있어서 그룹으로만 비교함
fn goes_back((ga, a): &OrderKey, (gb, b): &OrderKey) -> bool {
    use dmpicker_nav::Group;
    if ga != gb { gb < ga } else { matches!(ga, Group::Letter(_) | Group::Hangul) && b < a }
}

/// 온라인의 LOCKED 곡처럼 재킷이 회색으로 보이는 줄, 컬러 재킷인 곡이 회색으로 잘려 나온 경우
/// 원래 회색인 재킷도 있음
fn greyed(row: &RowRead, refs: &RefSet) -> bool {
    row.is_greyscale && !row.top_id().is_some_and(|id| refs.grey_art(id))
}

/// 온라인 화면에 목록에 없는 LOCKED 곡이 보임 (프리스타일 목록으로는 알 수 없는 안 푼 히든곡)
/// 나머지 줄이 모두 그 줄을 기준으로 한 칸 밀린 목록과 맞을 때 찾음
/// (넣을 위치, 화면 줄)을 돌려줌
fn find_missing_locked(rows: &[RowRead; 11], m: &[Vec<bool>], refs: &RefSet) -> Option<(usize, usize)> {
    let n = m[0].len();
    if n < rows.len() {
        return None;
    }
    let mut found = None;
    for j in 1..rows.len() - 1 {
        // 위아래 줄이 모두 뒷받침해야 함
        if !greyed(&rows[j], refs) {
            continue;
        }
        for at in 0..n {
            // 위쪽 줄은 at-j+k번 칸, 아래쪽 줄은 한 칸씩 밀려 있음
            let ent = |k: usize| (at + n * 2 + k - j - usize::from(k > j)) % n;
            if m[j][at] || m[j][(at + n - 1) % n] {
                continue;
            }
            if (0..rows.len()).filter(|&k| k != j).all(|k| m[k][ent(k)]) {
                if found.is_some() {
                    return None; // 후보가 둘 이상
                }
                found = Some((at, j));
            }
        }
    }
    found
}

fn locked_entry(row: &RowRead, learned: &Learned) -> LEntry {
    let id = row
        .cands
        .first()
        .filter(|c| c.score >= 0.6 && c.id != RANDOM_ID && learned.index_of(c.id).is_none())
        .map(|c| c.id);
    LEntry { id, random: false, locked: true, feat: quant(&row.feature) }
}

// ------------------------------------------------------------------ 이동기용 GameIo

pub struct LiveIo<'a> {
    pub screen: &'a Screen,
    pub refs: &'a RefSet,
    pub learned: &'a Learned,
    pub cancel: &'a AtomicBool,
    /// 멈춘 목록에서 이미 읽은 창, 첫 확인 때 새 프레임 대신 씀
    pub first: Option<[RowRead; 11]>,
    /// 온라인 목록에 없던 LOCKED 칸 (넣을 위치, 칸)
    /// 이게 생기면 `read_pos`가 [`Abort::ListChanged`]로 이동을 멈춤
    pub found: Option<(usize, LEntry)>,
    /// 마지막 키를 누른 시각
    pub pressed: Option<Instant>,
    /// 마지막으로 멈춘 프레임의 목록 위치, 곁눈질할 띠를 정함
    pub geom: Option<ListGeom>,
    /// 마지막으로 멈춘 프레임의 버튼 모드 표시 (4, 5, 6, 8)
    pub mode_seen: Option<u8>,
}

impl LiveIo<'_> {
    /// 멈춘 목록의 11줄, 줄이 멈춘 자리 격자에 붙은 연속 두 프레임이 같은 위치로 읽혀야 함
    /// 스크롤 중에도 줄을 지날 때마다 격자에 잠깐 붙어서 한 프레임으로는 못 믿음
    fn still_rows(&mut self) -> Result<Option<[RowRead; 11]>, Abort> {
        let t0 = Instant::now();
        let mut prev: Option<Option<usize>> = None;
        std::thread::sleep(Duration::from_millis(40));
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(Abort::Cancelled);
            }
            let img = self.screen.grab()?;
            let info = dmpicker_engine::analyze(&img);
            match info.list.filter(|g| info.at_rest && g.on_grid()) {
                Some(g) => {
                    let rows = read_window(&img, &g, self.refs);
                    let pos = locate(&match_table(&rows, self.learned)).0;
                    if prev == Some(pos) {
                        self.geom = Some(g);
                        self.mode_seen = dmpicker_engine::panel::button_mode(&img, info.kind, &g);
                        return Ok(Some(rows));
                    }
                    prev = Some(pos);
                }
                None => prev = None,
            }
            if t0.elapsed() > Duration::from_millis(1500) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

impl GameIo for LiveIo<'_> {
    fn press(&mut self, key: NKey, hold_ms: u32, gap_ms: u32) -> Result<(), Abort> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Abort::Cancelled);
        }
        press_engine(self.screen.game, key, hold_ms, gap_ms)?;
        self.pressed = Some(Instant::now());
        Ok(())
    }

    fn read_pos(&mut self) -> Result<Observation, Abort> {
        if !input::is_foreground(self.screen.game) {
            return Err(Abort::FocusLost);
        }
        let rows = match self.first.take() {
            Some(rows) => rows,
            None => match self.still_rows()? {
                Some(rows) => rows,
                None => return Ok(Observation::NotOnList),
            },
        };
        let m = match_table(&rows, self.learned);
        let (pos, agree) = locate(&m);
        if self.learned.kind == ScreenKind::OnlineList && agree < rows.len() {
            if let Some((at, j)) = find_missing_locked(&rows, &m, self.refs) {
                self.found = Some((at, locked_entry(&rows[j], self.learned)));
                return Err(Abort::ListChanged);
            }
        }
        Ok(match pos {
            Some(p) => Observation::At(p),
            None => Observation::Unknown,
        })
    }

    /// 마지막으로 멈춘 프레임의 목록 막대 둘레만 화면에서 떠서 읽음 (게임 화면 높이 그대로, 1440p에서 폭 약 120 px)
    /// 창 전체를 찍는 것보다 훨씬 빨라 키 사이에 끼워도 연타가 안 끊김
    /// 다른 창이 덮이면 틀리게 읽을 수 있지만 이어 보낼지만 정하고 위치는 멈춘 화면으로 확인함
    fn glance(&mut self) -> Result<Option<usize>, Abort> {
        if !input::is_foreground(self.screen.game) {
            return Err(Abort::FocusLost);
        }
        let Some(g) = self.geom else { return Ok(None) };
        let Some(frame) = capture::frame_rect_screen(self.screen.game) else { return Ok(None) };
        // 창 크기가 바뀌었으면 목록 자리도 모름
        if (frame.w, frame.h) != (g.width as i32, g.height as i32) {
            return Ok(None);
        }
        let s = g.scale();
        let right = g.bar_x + g.bar_w + (8.0 * s).ceil() as i32;
        let left = (g.bar_x + g.bar_w - (100.0 * s).ceil() as i32).max(0);
        let strip = dmpicker_engine::Rect::new(frame.x + left, frame.y, right - left, frame.h);
        let Ok(img) = capture::capture_screen_rect(strip) else { return Ok(None) };
        // 찍는 사이 창이 움직였으면 다른 자리를 찍은 것
        if capture::frame_rect_screen(self.screen.game) != Some(frame) {
            return Ok(None);
        }
        let Some(sg) = dmpicker_engine::find_list(&img).filter(|sg| sg.rows_seen >= dmpicker_engine::state::MIN_ROWS) else {
            return Ok(None);
        };
        // 재킷 칸이 띠 안에 다 있어야 읽음
        let whole = img.rect();
        if (0..dmpicker_engine::NROWS).any(|k| whole.intersect(&sg.jacket_box(k)) != Some(sg.jacket_box(k))) {
            return Ok(None);
        }
        let rows = read_window(&img, &sg, self.refs);
        Ok(locate(&match_table(&rows, self.learned)).0)
    }
}

// ------------------------------------------------------------------ 목록 인식

#[derive(Clone)]
struct Seen {
    cands: Vec<Candidate>,
    sure: bool,
    random: bool,
    /// 회색으로 보임 ([`greyed`])
    grey: bool,
    feat: Box<Feature>,
    row: usize,
}

impl Seen {
    /// 가장자리 줄(0, 10)은 아직 흐려지는 중일 수 있어서 가운데의 확실한 인식을 우선함
    fn quality(&self) -> f32 {
        let top = self.cands.first().map(|c| c.score).unwrap_or(0.0);
        let margin = top - self.cands.get(1).map(|c| c.score).unwrap_or(0.0);
        let edge = if self.row == 0 || self.row == 10 { 0.25 } else { 0.0 };
        top + margin + if self.sure { 0.5 } else { 0.0 } - edge
    }
}

/// 두 창에서 같은 줄을 읽은 두 결과의 비교
#[derive(Clone, Copy, PartialEq)]
enum Pair {
    Same,
    Unclear,
    /// 둘 다 확실한데 서로 다름 (또는 한쪽만 RANDOM), 다른 칸임
    Contradict,
}

/// 확실하지 않은 인식에서 근거로 쳐 주는 최저 후보 점수
const READ_FLOOR: f32 = 0.5;

/// 두 창에서 본 게 같은 칸인지, 인식 결과로 먼저 보고 다음에 특징으로 봄
/// 같은 재킷을 두 줄에서 자르면 `ALIGN_SIM` 위, 다른 재킷끼리는 한참 아래
fn pair(a: &Seen, b: &Seen) -> Pair {
    if a.random || b.random {
        return if a.random && b.random { Pair::Same } else { Pair::Contradict };
    }
    let (ta, tb) = (a.cands.first(), b.cands.first());
    if a.sure && b.sure {
        return if ta.map(|c| c.id) == tb.map(|c| c.id) { Pair::Same } else { Pair::Contradict };
    }
    // 둘 다 같은 곡 쪽으로 기욺
    if let (Some(x), Some(y)) = (ta, tb) {
        if x.id == y.id && x.score >= READ_FLOOR && y.score >= READ_FLOOR {
            return Pair::Same;
        }
    }
    // 한쪽의 확실한 결과가 다른 쪽 상위 후보 안에 있음
    let backs = |sure: &Seen, other: &Seen| {
        sure.sure && sure.cands.first().is_some_and(|t| other.cands.iter().take(3).any(|c| c.id == t.id && c.score >= READ_FLOOR))
    };
    if backs(a, b) || backs(b, a) || features::dot(&a.feat[..], &b.feat[..]) >= ALIGN_SIM {
        return Pair::Same;
    }
    Pair::Unclear
}

/// 목록 끝 판정용으로 더 엄격하게, 둘 다 자신 있게 같은 곡이거나 둘 다 RANDOM
fn firm(a: &Seen, b: &Seen) -> bool {
    if a.random || b.random {
        return a.random && b.random;
    }
    matches!((a.cands.first(), b.cands.first()), (Some(x), Some(y)) if x.id == y.id && x.score >= 0.6 && y.score >= 0.6)
}

/// 인식 진행 단계, 토스트 표시용
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScanStage {
    /// 정렬 창에서 게임 정렬을 확인하고 제목순으로 맞추는 중
    Sorting,
    /// 목록 읽는 중, 짐작한 길이 중 읽은 비율 (0..1), 끝을 찾으면 1
    Reading(f32),
    /// 그 길이를 넘게 읽었는데 아직 끝을 못 찾음
    FindingEnd,
    /// 읽고도 불확실한 칸을 다시 보는 중: 끝낸 수, 전체 수
    Rechecking(usize, usize),
}

/// 끝을 찾기 전 링의 최대 채움, 100 %는 다 읽었을 때만
const FINDING_FILL: f32 = 0.99;

impl ScanStage {
    /// 진행 링의 채움 정도 (0..1), 다시 보기 동안은 다 찬 채로
    pub fn fill(self) -> f32 {
        match self {
            ScanStage::Sorting => 0.0,
            ScanStage::Reading(p) => p.clamp(0.0, 1.0),
            ScanStage::FindingEnd => FINDING_FILL,
            ScanStage::Rechecking(..) => 1.0,
        }
    }
}

/// 인식 시간이 어디에 쓰였는지, 로그용 (동작엔 영향 없음)
#[derive(Default)]
struct ScanTimes {
    keys: Duration,
    /// 창마다 목록이 멈추길 기다린 시간
    settle: Waits,
    read: Duration,
    /// 다시 보기: 흐려짐이 끝나길 더 기다린 시간, 프레임 캡처, 인식
    fade: Duration,
    look_frame: Duration,
    look_read: Duration,
    looks: u32,
    /// 줄의 1순위 후보(또는 RANDOM 여부)를 바꾼 다시 보기 횟수와 바뀐 줄 수
    looks_top_changed: u32,
    rows_top_changed: u32,
    /// 다시 보기로 불확실에서 확실이 된 줄 수
    rows_made_sure: u32,
    fit: Duration,
    merge: Duration,
    wrap: Duration,
    windows: u32,
    /// 한 단계의 마지막 키부터 다음 창 첫 인식까지
    to_read: Duration,
    to_read_max: Duration,
    steps: u32,
    /// 방향키 간격(ms)별 이동 수: 전체, 첫 인식 때 누른 만큼 안 움직인 수, 나중에 입력이 늦게 먹은 걸로 밝혀진 수
    paces: BTreeMap<u32, [u32; 3]>,
    pace_end: u32,
}

impl ScanTimes {
    fn log(&self, total: Duration) {
        let s = |d: Duration| d.as_secs_f32();
        let w = &self.settle;
        let looks = self.fade + self.look_frame + self.look_read;
        let compute = self.fit + self.merge + self.wrap;
        let known = self.keys + w.grab + w.judge + w.sleep + self.read + looks + compute;
        diagnostic!(
            "scan time {:.1}s: keys {:.1}s, settle {:.1}s (capture {:.1}s, rest check {:.1}s, sleep {:.1}s, {} frames), read {:.1}s, second looks {:.1}s (fade wait {:.1}s, frame {:.1}s, read {:.1}s), compute {:.2}s (fit {:.2}s, merge {:.2}s, wrap {:.2}s), other {:.1}s",
            s(total),
            s(self.keys),
            s(w.grab + w.judge + w.sleep),
            s(w.grab),
            s(w.judge),
            s(w.sleep),
            w.frames,
            s(self.read),
            s(looks),
            s(self.fade),
            s(self.look_frame),
            s(self.look_read),
            s(compute),
            s(self.fit),
            s(self.merge),
            s(self.wrap),
            s(total.saturating_sub(known)),
        );
        diagnostic!(
            "scan windows: {} read, {} second looks (best candidate changed in {} looks / {} rows, unsure made sure in {} rows), last key to first read avg {} ms max {} ms",
            self.windows,
            self.looks,
            self.looks_top_changed,
            self.rows_top_changed,
            self.rows_made_sure,
            self.to_read.as_millis() / u128::from(self.steps.max(1)),
            self.to_read_max.as_millis(),
        );
        let Some(&fastest) = self.paces.keys().next() else { return };
        let paces: Vec<String> = self.paces.iter().map(|(ms, [n, short, late])| format!("{ms} ms {n} moves ({short} short, {late} late)")).collect();
        diagnostic!("scan pace: final {} ms, fastest {fastest} ms; {}", self.pace_end, paces.join(", "));
    }
}

/// 인식 창 하나에서 다음 창까지 넘기는 줄 수
/// 맨 위·아래 줄은 잘리거나 아직 흐려서 안쪽 줄만 비교함, 11줄이면 안쪽 4줄이 겹침
const STEP: usize = 5;
/// 새 창은 확실히 맞을 때만 이전 창에 이어 붙임: 겹치는 안쪽 줄 중 이 비율 이상, `MIN_COMMON`줄 이상,
/// 모순 없음, 다른 밀림보다 이만큼 앞서야 함
const MIN_OVERLAP: f32 = 0.75;
const MIN_COMMON: usize = 3;
const MIN_LEAD: f32 = 0.2;
/// 안 맞는 창을 포기하기 전까지 다시 보는 횟수
const REREADS: u32 = 2;
/// 목록이 안 움직인 창(입력 씹힘)이 연속 이만큼을 넘으면 포기, 그때마다 누르는 시간을 늘림
const MAX_STALL: u32 = 6;
/// 목록 끝 판정: `WRAP_SPAN`칸 구간 두 개가 다시 나오고 그중 `WRAP_FIRM`칸이 확실히 맞아야 함
const WRAP_SPAN: i64 = 12;
const WRAP_FIRM: usize = 9;
/// 첫 창의 위쪽 줄, 한 번만 읽힘
const READ_ONCE: i64 = 6;
/// 끝이 확실해질 때까지 끝을 지나 더 읽는 칸 수
const PAST_END: i64 = READ_ONCE + 2 * WRAP_SPAN;

/// 작업이 멈춘 이유
pub enum Stop {
    /// 사용자가 뭔가 건드림 (키, 클릭, 휠, 다른 창), 값은 이유 문구
    Touched(&'static str),
    Cancelled,
    Failed(String),
}

impl From<Abort> for Stop {
    fn from(e: Abort) -> Self {
        match e {
            Abort::Cancelled => Stop::Cancelled,
            Abort::FocusLost => Stop::Touched(FOCUS_LEFT),
            Abort::ListChanged => Stop::Failed(MISREAD.into()),
            Abort::Io(e) => {
                diagnostic!("io: {e}");
                Stop::Failed(NO_FRAME.into())
            }
        }
    }
}

const FOCUS_LEFT: &str = "다른 창으로 바뀌었어요";
const MOVED: &str = "키보드나 마우스 입력이 있었어요";
const NO_FRAME: &str = "게임 화면을 읽지 못했어요";
/// 곡 목록이 없음, 작업 시작 때부터 없거나 도중에 사라짐
const NOT_HERE: &str = "선곡 화면에서 다시 시도해 주세요";
const TAB_ASK: &str = "ALL TRACK 탭으로 바꾼 뒤 다시 시도해 주세요";
/// 온라인 방에선 방장만 목록을 움직임
const NOT_HOST: &str = "방장만 곡을 고를 수 있어요";
/// 앞뒤가 안 맞는 인식 (오인식, 키와 다르게 움직인 목록), 저장 안 함
const MISREAD: &str = "목록을 제대로 읽지 못했어요. 다시 인식해 주세요";

/// 인식 중엔 손을 떼야 함, 키·클릭·휠은 목록을 움직이고 다른 창은 키를 가져감
/// 마우스 이동만은 괜찮음 (게임이 안 따라감)
struct HandsOff {
    game: Hwnd,
    /// 사용자의 키와 마우스 버튼, raw input이 없으면 포커스로만 알 수 있음
    hands: Option<Hands>,
}

impl HandsOff {
    /// 지금부터 이 스레드에서 감시, 작업을 멈추는 `stop` 단축키는 손댄 걸로 안 침
    fn begin(game: Hwnd, stop: Hotkey) -> Self {
        let hands = Hands::watch(stop);
        if hands.is_none() {
            diagnostic!("hands: no raw input, watching the focus only");
        }
        HandsOff { game, hands }
    }

    /// 사용자 키를 감시 중 (raw input 시작됨), 정렬 바꾸기에 필요함
    fn watching(&self) -> bool {
        self.hands.is_some()
    }

    fn check(&self) -> Result<(), Stop> {
        if !input::is_foreground(self.game) {
            return Err(Stop::Touched(FOCUS_LEFT));
        }
        if self.hands.as_ref().is_some_and(Hands::touched) {
            return Err(Stop::Touched(MOVED));
        }
        Ok(())
    }
}

/// 목록이 끝나는 곳, `WRAP_SPAN`칸 구간 두 개가 다시 나오는 가장 짧은 길이
/// 각 구간은 서로 다른 `WRAP_FIRM`곡이 확실히 맞고 모순이 없어야 함
/// 구간은 첫 창 위쪽 절반(한 번만 읽힘)을 건너뛰고 서로 다른 `WRAP_FIRM`곡이 잘 읽힌 곳에 둠
/// 아무 두 구간이면 되니 어느 바퀴가 약하게 읽혀도 끝을 못 찾거나 한 바퀴 늦게 찾지 않음
fn wrap_length(seq: &HashMap<i64, Seen>, lo: i64, hi: i64) -> Option<usize> {
    let well_read = |p: i64| seq.get(&p).filter(|s| !s.random).and_then(|s| s.cands.first()).filter(|c| c.score >= 0.6).map(|c| c.id);
    let clear = |r: i64| (r..r + WRAP_SPAN).filter_map(well_read).collect::<HashSet<_>>().len() >= WRAP_FIRM;
    let mut stretches = Vec::new();
    let mut r = lo + READ_ONCE;
    while r + WRAP_SPAN <= hi {
        if clear(r) {
            stretches.push(r);
            r += WRAP_SPAN;
        } else {
            r += 1;
        }
    }
    let repeats = |from: i64, len: i64| {
        let mut songs = HashSet::new();
        for i in 0..WRAP_SPAN {
            if let (Some(a), Some(b)) = (seq.get(&(from + i)), seq.get(&(from + len + i))) {
                if firm(a, b) {
                    songs.insert(if a.random { None } else { a.cands.first().map(|c| c.id) });
                } else if pair(a, b) == Pair::Contradict {
                    return false;
                }
            }
            if songs.len() + ((WRAP_SPAN - 1 - i) as usize) < WRAP_FIRM {
                return false;
            }
        }
        true
    };
    (12..=hi - stretches.get(1)? - WRAP_SPAN + 1)
        .find(|&len| stretches.iter().filter(|&&r| r + len + WRAP_SPAN <= hi + 1 && repeats(r, len)).take(2).count() == 2)
        .map(|len| len as usize)
}

/// 두 인식이 같은 id여서 맞은 경우 그 곡
fn same_song(a: &Seen, b: &Seen) -> Option<u32> {
    let (x, y) = (a.cands.first()?, b.cands.first()?);
    (!a.random && !b.random && x.id == y.id).then_some(x.id)
}

/// `rows`가 `prev`에 이어지는 밀림 (0..=STEP, STEP번 눌러서 그보다 더 가진 않음)
/// 겹치는 안쪽 줄이 맞고 다른 모든 밀림보다 확실히 앞서야 함
/// 모순 있는 밀림은 비교 전에 빼고, 여러 줄에서 맞은 한 곡은 한 번만 셈
/// Err: 본 내용 (로그용)
fn fit_shift(prev: &[Seen], rows: &[Seen]) -> Result<(i64, f32), String> {
    let inner = |r: usize| r != 0 && r != rows.len() - 1;
    let common = |s: usize| (0..rows.len() - s).filter(move |&k| inner(k) && inner(k + s));
    // (모순 수, 맞은 수, 비교한 수), `faint`면 한쪽이 확실하고 1순위가 같은 줄도 점수와 상관없이 맞은 걸로 셈
    let tally = |s: usize, faint: bool| {
        let (mut songs, mut others, mut contra, mut n) = (HashSet::new(), 0, 0, 0);
        for k in common(s) {
            let (a, b) = (&prev[k + s], &rows[k]);
            n += 1;
            match pair(a, b) {
                Pair::Contradict => contra += 1,
                Pair::Same => match same_song(a, b) {
                    Some(id) => {
                        songs.insert(id);
                    }
                    None => others += 1,
                },
                Pair::Unclear => {
                    if let Some(id) = same_song(a, b).filter(|_| faint && (a.sure || b.sure)) {
                        songs.insert(id);
                    }
                }
            }
        }
        (contra, songs.len() + others, n)
    };
    // (밀림, 모순 수, 맞은 수, 비교한 수, 점수)
    let mut fits: Vec<(usize, usize, usize, usize, f32)> = (0..=STEP)
        .map(|s| {
            let (contra, m, n) = tally(s, false);
            (s, contra, m, n, m as f32 / n as f32 - 0.01 * (s as f32 - STEP as f32).abs())
        })
        .collect();
    fits.sort_by(|a, b| a.1.cmp(&b.1).then(b.4.total_cmp(&a.4)));
    let (shift, contra, mut m, n, score) = fits[0];
    let lead = fits.get(1).filter(|f| f.1 == 0).map_or(f32::INFINITY, |f| score - f.4);
    // 다른 밀림이 모두 모순이면 위치는 이걸로 정해짐
    // 느린 PC에선 새로 올라온 줄이 흐릴 때 읽혀 다시 봐도 점수가 바닥 아래였지만 1순위는 맞았음
    if contra == 0 && lead.is_infinite() {
        m = tally(shift, true).1;
    }
    let share = m as f32 / n as f32;
    if contra == 0 && m >= MIN_COMMON && share >= MIN_OVERLAP && lead >= MIN_LEAD {
        return Ok((shift as i64, share));
    }
    let read = |r: &Seen| r.cands.first().map_or("-".to_string(), |c| format!("{}:{:.2}{}", c.id, c.score, if r.sure { "!" } else { "" }));
    let evidence: Vec<String> = common(shift).map(|k| format!("{}={}", read(&prev[k + shift]), read(&rows[k]))).collect();
    let contradicted: Vec<usize> = fits.iter().filter(|f| f.1 > 0).map(|f| f.0).collect();
    Err(format!("shift {shift} {m}/{n} lead {lead:.2} contradictions {contra}, contradicted {contradicted:?}, rows {}", evidence.join(" ")))
}

/// 키를 누른 뒤 줄이 흐려지는(커서에서 멀수록 어두워짐) 시간, 대략 이 정도
/// 불확실한 줄이 있으면 이때 처음 다시 봄
const FADE: Duration = Duration::from_millis(320);
/// 흐려짐이 끝나는 시간은 PC마다 달라서 점수가 오르는 동안은 이 간격으로 더 봄
/// 느린 PC에선 320 ms에 다시 봐도 1순위만 맞고 점수가 낮았음
const LOOK_GAP: Duration = Duration::from_millis(100);
/// 마지막 키 뒤 이때까지만 다시 봄, 끝내 확실해지지 않는 닮은 재킷에서 오래 안 머묾
const LOOK_UNTIL: Duration = Duration::from_millis(1500);
/// 다시 본 인식이 이만큼 나아져야 흐려지는 중으로 봄, 멈춘 화면끼리도 점수가 0.01쯤 흔들림
const LOOK_GAIN: f32 = 0.03;
/// 누른 만큼 안 움직였을 때 마지막 키 뒤 이만큼 지나면 늦게 먹은 입력까지 목록에 반영됨
const LATE: Duration = Duration::from_millis(400);

/// 누른 만큼 안 움직인 창을 다시 본 것(`again`)으로 목록이 더 간 곳의 `rows`를 바꾸고([`merge_look`])
/// `prev`에 다시 맞춤, 그 밀림을 돌려주고 못 맞추면 `rows`를 되돌리고 None
fn look_late(prev: &[Seen], rows: &mut Vec<Seen>, again: Vec<Seen>) -> Option<i64> {
    let before = rows.clone();
    merge_look(rows, &mut before.clone(), again, |_| false);
    let fit = fit_shift(prev, rows).map(|f| f.0).ok();
    if fit.is_none() {
        *rows = before;
    }
    fit
}

/// 창을 나중에 다시 본 결과, 목록이 확실히 제자리면(밀림 0으로 맞으면) 줄마다 더 나은 인식을 `rows`에 남김
/// 아니면 새로 본 걸로 바꿈
/// 아직 불확실한 줄이 바로 전에 본 것(`last`)과 같은 곡이고 점수도 안 올랐는지 돌려줌
/// 가장 나은 인식과 비교하면 1순위가 한 번 바뀐 줄은 계속 바뀐 걸로 보여서 직전 것과 비교함
fn merge_look(rows: &mut Vec<Seen>, last: &mut Vec<Seen>, again: Vec<Seen>, unsure: impl Fn(&Seen) -> bool) -> bool {
    if again.len() != rows.len() || !matches!(fit_shift(rows, &again), Ok((0, _))) {
        *rows = again.clone();
        *last = again;
        return false;
    }
    let top = |r: &Seen| r.cands.first().map(|c| c.id);
    let held = (0..rows.len()).all(|k| !unsure(&rows[k]) || (top(&last[k]) == top(&again[k]) && again[k].quality() < last[k].quality() + LOOK_GAIN));
    for (a, b) in rows.iter_mut().zip(&again) {
        if b.quality() > a.quality() {
            *a = b.clone();
        }
    }
    *last = again;
    held
}

fn seen(img: &dmpicker_engine::Rgb, geom: &ListGeom, refs: &RefSet) -> Vec<Seen> {
    read_window(img, geom, refs)
        .into_iter()
        .map(|r| Seen { grey: greyed(&r, refs), cands: r.cands, sure: r.sure, random: r.is_random, feat: r.feature, row: r.row })
        .collect()
}

/// 원형 목록을 한 바퀴 돌며 모든 칸을 기록 (id, 게임 화면 재킷 특징)
/// `progress`에는 지금까지 읽은 칸 수가 들어감
fn scan(screen: &Screen, refs: &RefSet, cancel: &AtomicBool, hands: &HandsOff, progress: impl FnMut(usize)) -> Result<PendingLearned, Stop> {
    let started = Instant::now();
    let mut times = ScanTimes::default();
    let r = scan_timed(screen, refs, cancel, hands, progress, &mut times);
    times.log(started.elapsed());
    r
}

fn scan_timed(
    screen: &Screen,
    refs: &RefSet,
    cancel: &AtomicBool,
    hands: &HandsOff,
    mut progress: impl FnMut(usize),
    t: &mut ScanTimes,
) -> Result<PendingLearned, Stop> {
    let mut pace = Pace::new();
    let first = screen.settled_timed(cancel, Duration::from_secs(3), &mut t.settle)?;
    let Some((_, geom0)) = first else { return Err(Stop::Failed(NOT_HERE.into())) };
    let kind = layout_kind(&geom0);
    let mut seq: HashMap<i64, Seen> = HashMap::new();
    let mut pos: i64 = 0;
    let mut prev: Option<Vec<Seen>> = None;
    let started = Instant::now();
    let (mut rereads, mut stall) = (0u32, 0u32);
    let mut moved_at = Instant::now();
    let mut stepped = false;
    let mut agrees: Vec<f32> = Vec::new();
    loop {
        hands.check()?;
        let (img, geom) = screen
            .settled_timed(cancel, Duration::from_secs(3), &mut t.settle)?
            .ok_or(Stop::Failed(NOT_HERE.into()))?;
        if std::mem::take(&mut stepped) {
            let d = moved_at.elapsed();
            t.to_read += d;
            t.to_read_max = t.to_read_max.max(d);
            t.steps += 1;
        }
        t.windows += 1;
        let at = Instant::now();
        let mut rows = seen(&img, &geom, refs);
        t.read += at.elapsed();
        // 목록이 멈춘 뒤에도 잠깐 줄이 흐려짐 (줄이 어두워지고 뒤에 곡 배경이 깔림)
        // 안쪽 줄이 불확실하면 흐려짐이 끝난 뒤 다시 보고 줄마다 더 나은 인식을 남김
        // 불확실한 인식이 1순위도 점수도 그대로일 때까지 반복
        let last = rows.len().saturating_sub(1);
        let unsure = |r: &Seen| !r.sure && !r.random && r.row != 0 && r.row != last;
        let top = |r: &Seen| (r.random, r.cands.first().map(|c| c.id));
        let mut next = FADE;
        let mut last = rows.clone();
        while rows.iter().any(unsure) && moved_at.elapsed() < LOOK_UNTIL {
            if let Some(wait) = next.checked_sub(moved_at.elapsed()) {
                let at = Instant::now();
                std::thread::sleep(wait);
                t.fade += at.elapsed();
            }
            // 목록이 멈춰 있었으니 프레임 하나면 됨, 그래도 움직이고 있으면 멈출 때까지 기다림
            let at = Instant::now();
            let img = screen.grab()?;
            let info = dmpicker_engine::analyze(&img);
            let at_rest = info.at_rest;
            let (img, geom) = match info.list.filter(|_| at_rest) {
                Some(geom) => (img, geom),
                None => {
                    hands.check()?;
                    screen.settled(cancel, Duration::from_secs(3))?.ok_or(Stop::Failed(NOT_HERE.into()))?
                }
            };
            hands.check()?;
            t.look_frame += at.elapsed();
            t.looks += 1;
            let at = Instant::now();
            let again = seen(&img, &geom, refs);
            t.look_read += at.elapsed();
            let before: Vec<_> = rows.iter().map(|r| (top(r), r.sure)).collect();
            let at = Instant::now();
            let held = merge_look(&mut rows, &mut last, again, unsure);
            t.merge += at.elapsed();
            let changed = rows.iter().zip(&before).filter(|(r, b)| top(r) != b.0).count() as u32;
            if changed > 0 {
                t.looks_top_changed += 1;
                t.rows_top_changed += changed;
            }
            t.rows_made_sure += rows.iter().zip(&before).filter(|(r, b)| r.sure && !b.1).count() as u32;
            if held {
                break;
            }
            next = moved_at.elapsed() + LOOK_GAP;
        }
        if let Some(p) = &prev {
            let at = Instant::now();
            let fit = fit_shift(p, &rows);
            t.fit += at.elapsed();
            let (shift, share) = match fit {
                Ok(fit) => fit,
                // 확실히 안 맞으면 다시 봄 (인식이 조금씩 달라짐)
                // 그래도 안 되면 틀린 목록을 잇느니 멈춤
                Err(why) => {
                    diagnostic!("scan: window does not fit ({why}){}", if rereads >= REREADS { ", stopping" } else { "" });
                    if rereads >= REREADS {
                        return Err(Stop::Failed(MISREAD.into()));
                    }
                    rereads += 1;
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
            };
            rereads = 0;
            agrees.push(share);
            // 25 ms보다 빠르면 누른 만큼 안 움직일 수 있음, 게임은 스크롤보다 빨리 온 입력을 차례로 처리함
            // 그래서 입력이 대기 중인데 목록이 칸에 맞춰 멈춰 보일 수 있음
            // 입력이 다 반영된 뒤 다시 봐서 다음 이동이 STEP 안에 들게 함
            // 25 ms에선 입력이 늦게 먹지 않아서 덜 움직여도 그대로 받아들임
            let ms = pace.ms();
            let (fitted, mut shift) = (shift, shift);
            let look = (fitted as usize) < STEP && ms < SAFE_MS;
            if look {
                if let Some(wait) = LATE.checked_sub(moved_at.elapsed()) {
                    std::thread::sleep(wait);
                }
                hands.check()?;
                let (img, geom) = screen.settled(cancel, Duration::from_secs(3))?.ok_or(Stop::Failed(NOT_HERE.into()))?;
                hands.check()?; // 프레임을 찍는 동안 안 건드렸음
                shift = look_late(p, &mut rows, seen(&img, &geom, refs)).unwrap_or(fitted);
            }
            pos += shift;
            // 덜 움직이면 더 느린 간격으로 바꿈, 목록이 아예 안 움직이면 포기
            pace.moved(STEP, fitted as usize, shift as usize);
            let counts = t.paces.entry(ms).or_default();
            counts[0] += 1;
            if (fitted as usize) < STEP {
                counts[1] += 1;
                counts[2] += u32::from(shift > fitted);
                let again = if look { format!(", {shift} when looked at again") } else { String::new() };
                diagnostic!("scan: moved {fitted} of {STEP} at {ms} ms{again}; now {} ms", pace.ms());
            }
            t.pace_end = pace.ms();
            if shift == 0 {
                stall += 1;
                if stall > MAX_STALL {
                    return Err(Stop::Failed(NOT_MOVING.into()));
                }
            } else {
                stall = 0;
            }
        }
        for (k, r) in rows.iter().enumerate() {
            let p = pos - 5 + k as i64;
            let better = seq.get(&p).map(|old| r.quality() > old.quality()).unwrap_or(true);
            if better {
                seq.insert(p, r.clone());
            }
        }
        prev = Some(rows);

        let lo = *seq.keys().min().unwrap();
        let hi = *seq.keys().max().unwrap();
        let at = Instant::now();
        let length = wrap_length(&seq, lo, hi);
        t.wrap += at.elapsed();
        // 끝을 찾은 창은 바로 다시 보기로 넘어감
        if length.is_none() {
            progress((hi - lo + 1) as usize);
        }
        if let Some(l) = length {
            // 시작 근처 칸은 두 바퀴 다 읽혔음, 더 나은 인식을 남김
            // 서로 다른 곡으로 읽힌 칸은 다시 봄
            let mut doubtful = Vec::new();
            let reads: Vec<&Seen> = (0..l as i64)
                .map(|i| {
                    let a = &seq[&(lo + i)];
                    let Some(b) = seq.get(&(lo + l as i64 + i)) else { return a };
                    if pair(a, b) == Pair::Contradict {
                        doubtful.push(i as usize);
                    }
                    if b.quality() > a.quality() { b } else { a }
                })
                .collect();
            let cand_lists: Vec<Vec<Candidate>> = reads.iter().map(|s| s.cands.clone()).collect();
            let entries = reads
                .iter()
                .map(|s| {
                    let top = s.cands.first();
                    let id = if s.random || top.map(|c| c.id == RANDOM_ID).unwrap_or(false) {
                        None
                    } else {
                        top.filter(|c| s.sure || c.score >= 0.55).map(|c| c.id)
                    };
                    LEntry { id, random: s.random, locked: s.grey && kind == ScreenKind::OnlineList && !s.sure, feat: quant(&s.feat) }
                })
                .collect();
            agrees.sort_by(f32::total_cmp);
            diagnostic!(
                "scan: {l} entries in {:.1}s, window overlap agreement min {:.2} / 5th pct {:.2}, {} read two ways, {} second looks",
                started.elapsed().as_secs_f32(),
                agrees.first().copied().unwrap_or(1.0),
                agrees.get(agrees.len() / 20).copied().unwrap_or(1.0),
                doubtful.len(),
                t.looks
            );
            return Ok(PendingLearned { learned: Learned { kind, entries }, cands: cand_lists, doubtful });
        }
        if hi - lo > 2500 {
            return Err(Stop::Failed(MISREAD.into()));
        }
        let at = Instant::now();
        let ms = pace.ms();
        for _ in 0..STEP {
            if cancel.load(Ordering::Relaxed) {
                return Err(Stop::Cancelled);
            }
            press_engine(screen.game, NKey::Down, ms, ms)?;
        }
        moved_at = Instant::now();
        t.keys += moved_at - at;
        stepped = true;
    }
}

// ------------------------------------------------------------------ 이동 작업 하나

pub enum JobResult {
    Arrived { presses: u32, ms: u64, diff_note: Option<String> },
    /// 저장 목록에 없음, 없는 곡이거나 인식 때 모름으로 남은 칸 중 하나
    NotFound,
    NeedScan,
    /// 저장 목록으로 곡에 못 감, 화면이 목록과 끝내 안 맞았거나 이동이 도착하지 못함
    /// 안 맞는 경우는 다른 계정이거나 인식 뒤 곡을 사거나 풀었을 때
    ListMismatch,
    Scanned { entries: usize, unknown: usize, secs: f32 },
    /// 사용자가 뭔가 건드려서 작업이 멈춤 (이유 문구)
    Touched(&'static str),
    Cancelled,
    Failed(String),
}

impl From<Stop> for JobResult {
    fn from(s: Stop) -> Self {
        match s {
            Stop::Touched(why) => JobResult::Touched(why),
            Stop::Cancelled => JobResult::Cancelled,
            Stop::Failed(e) => JobResult::Failed(e),
        }
    }
}

/// 단축키를 누르면 게임을 잠깐 봄: 게임이 앞에 있고 선곡 화면 (프리스타일, 온라인 방)이면 그 모드
/// 지금 화면에 보이는 그대로 읽음 (전용 전체 화면에선 PrintWindow가 지난 프레임을 줄 수 있음)
/// 목록이 움직이거나 (탭 전환) 게임이 막 앞으로 오는 중일 수 있어서 60 ms 간격으로 4번까지 보고, 보이면 바로 돌려줌
/// 목록을 못 찾으면 빈 탭인지도 봄 (빈 FAVORITE도 열고, 고르면 ALL TRACK으로 감)
pub fn peek() -> Option<ScreenKind> {
    let game = capture::find_game_window()?;
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(60));
        }
        if !input::is_foreground(game) {
            continue;
        }
        let Ok(img) = capture::capture(game, CaptureMethod::ScreenBlt) else { continue };
        let kind = classify(&img);
        if kind != ScreenKind::Other {
            return Some(kind);
        }
        if let Some((kind, _)) = tabs::empty_tab(&img) {
            return Some(kind);
        }
    }
    None
}

/// 인식 한 번으로 두 모드를 다 씀, 한 모드 목록만 있거나 다른 쪽이 안 읽히면 있는 쪽으로 만듦
pub fn ensure_both(store: &Path, title_of: &dyn Fn(u32) -> String) {
    for k in [ScreenKind::FreestyleList, ScreenKind::OnlineList] {
        if Learned::load(store, k).is_some() {
            continue;
        }
        let Some(src) = Learned::load(store, other_kind(k)) else { continue };
        if let Err(e) = src.save_derived(store, title_of) {
            diagnostic!("derive {}: {e}", kind_name(k));
        }
    }
}

/// 목록 저장 뒤 검색에 나오는 곡, 목록에 있는 곡(프리스타일처럼 LOCKED 제외)과 모르는 칸일 수 있는 곡
/// 첫 인식 전엔 `None`
pub fn searchable(store: &Path, k: ScreenKind, titles: &HashMap<u32, String>) -> Option<HashSet<u32>> {
    let l = Learned::load(store, k)?;
    let mut ids: HashSet<u32> = l.entries.iter().filter(|e| !e.locked).filter_map(|e| e.id).collect();
    if l.entries.iter().any(|e| e.id.is_none() && !e.random) {
        let maybe: Vec<u32> = titles.keys().copied().filter(|id| !ids.contains(id) && l.might_hold(*id, titles)).collect();
        ids.extend(maybe);
    }
    Some(ids)
}

fn focus_game(game: Hwnd) -> bool {
    if game.is_minimized() {
        input::request_restore(game);
    }
    let t0 = Instant::now();
    while !input::is_foreground(game) {
        if t0.elapsed() > Duration::from_millis(700) {
            return false;
        }
        input::request_foreground(game);
        std::thread::sleep(Duration::from_millis(30));
    }
    true
}

/// 포커스를 받은 게임 창 (호출하는 쪽이 포커스를 넘겨줌)
fn focused_screen() -> Result<Screen, JobResult> {
    let Some(game) = capture::find_game_window() else { return Err(JobResult::Failed("DJMAX가 인식되지 않아요".into())) };
    let minimized = game.is_minimized();
    if !focus_game(game) {
        return Err(JobResult::Failed("게임 창을 앞으로 가져오지 못했어요".into()));
    }
    if minimized {
        wait_restored(game);
    }
    if full_screen() {
        diagnostic!("capture: full screen, reading the screen");
    }
    Ok(Screen { game })
}

/// 전체 화면 게임은 검색창에 포커스를 뺏기면 최소화되고 다시 펴질 때 화면 모드를 바꾸느라 2~3초 걸림
/// 클라이언트 영역이 연달아 같을 때까지 기다림
fn wait_restored(game: Hwnd) {
    let t0 = Instant::now();
    let mut last = None;
    while t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(150));
        let now = if game.is_minimized() { None } else { capture::client_rect_screen(game) };
        if now.is_some() && now == last {
            break;
        }
        last = now;
    }
    diagnostic!("job: waited {} ms for the game to come back", t0.elapsed().as_millis());
}

/// 창 모양으로는 화면을 다 덮는 테두리 없는 창과 구별이 안 돼서 셸 알림 상태로 봄, 한 번에 0.1 ms 안쪽
fn full_screen() -> bool {
    unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN)
}

/// 탭 전환 애니메이션(~350 ms)이 끝나 현재 탭 이름이 멈춘 프레임
fn tab_frame(screen: &Screen, geom: &ListGeom, cancel: &AtomicBool) -> Result<dmpicker_engine::Rgb, Stop> {
    let mut prev: Option<Vec<f32>> = None;
    for _ in 0..30 {
        if cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        let img = screen.grab()?;
        let cur = tabs::current_label(&img, geom);
        if let (Some(a), Some(b)) = (&prev, &cur) {
            if features::dot(a, b) >= 0.98 {
                return Ok(img);
            }
        }
        prev = cur;
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(Stop::Failed(TAB_ASK.into()))
}

/// 저장 목록은 ALL TRACK 기준이라 ALL TRACK이 현재 탭이 될 때까지 탭을 바꿈
/// 아는 건 탭 이름뿐 (탭 순서와 개수는 DLC 팩에 따라 바뀜)
/// 탭 줄에서 `k`칸 오른쪽에 보이면 RShift `k`번, 아니면 탭 줄에 보이는 수 이하씩 LShift
/// 그래야 지나쳐도 다시 보임, ALL TRACK에서 멈춘 목록을 돌려줌
///
/// 빈 탭(아무것도 없는 FAVORITE)은 잴 목록이 없어서 레이아웃상 탭 줄 자리를 읽음
/// 거기서 ALL TRACK이 보여야만(선곡 화면이라는 증거) 키를 누름
fn goto_all_track(screen: &Screen, cancel: &AtomicBool) -> Result<(dmpicker_engine::Rgb, ListGeom), Stop> {
    let not_here = || Stop::Failed(NOT_HERE.into());
    let settled = || screen.settled(cancel, Duration::from_secs(3))?.ok_or_else(not_here);
    let press = |key: EKey, n: usize| {
        for _ in 0..n {
            if cancel.load(Ordering::Relaxed) {
                return Err(Stop::Cancelled);
            }
            input::press(screen.game, key, 40, 40).map_err(|_| Stop::Touched(FOCUS_LEFT))?;
        }
        std::thread::sleep(Duration::from_millis(300));
        Ok(())
    };
    let nominal = |img: &dmpicker_engine::Rgb| {
        let k = tabs::empty_tab(img).map(|(_, k)| k);
        if let Some(k) = k {
            diagnostic!("tabs: no list, ALL TRACK {k} to the right");
        }
        k
    };
    // 첫 프레임에 목록이 없으면 목록을 기다리지 않고 바로 빈 탭인지 봄
    let first = screen.grab()?;
    let empty = if dmpicker_engine::analyze(&first).list.is_none() { nominal(&first) } else { None };
    let (mut img, geom) = match empty {
        Some(k) => {
            press(EKey::RShift, k)?;
            settled()?
        }
        None => match screen.settled(cancel, Duration::from_secs(3))? {
            Some(frame) => frame,
            None => {
                let k = nominal(&screen.grab()?).ok_or_else(not_here)?;
                press(EKey::RShift, k)?;
                settled()?
            }
        },
    };
    let burst = tabs::strip_slots(layout_kind(&geom));
    // 탭 수보다 많이 뒤로 갔으면 인식을 못 하는 것
    let mut back = 0;
    for round in 0.. {
        if round > 0 {
            img = tab_frame(screen, &geom, cancel)?;
        }
        let (key, n) = match tabs::all_track_offset(&img, &geom, burst) {
            Some(0) if round == 0 => return Ok((img, geom)),
            Some(0) => return settled(), // 탭을 바꾸면 목록이 처음으로 돌아감
            // 첫 장은 탭이 바뀌는 중일 수 있음, 키를 누르기 전에 탭 이름이 멈춘 프레임으로 다시 봄
            _ if round == 0 => continue,
            Some(k) if round < 50 => (EKey::RShift, k),
            None if back < 48 => (EKey::LShift, burst),
            _ => break,
        };
        if key == EKey::LShift {
            back += n;
        }
        press(key, n)?;
    }
    Err(Stop::Failed(TAB_ASK.into()))
}

// ------------------------------------------------------------------ 정렬 순서

/// 정렬을 못 바꿨거나 마지막 키 뒤에도 정렬 창이 떠 있음, 사용자가 직접 바꾸게 함
/// 게임이 아직 받을 수 있는 키 뒤엔 다른 키를 안 누름
const SORT_ASK: &str = "정렬을 곡 제목 (A to Z)으로 맞춘 뒤 다시 시도해 주세요";
/// 키가 다른 데로 감, 온라인 채팅 입력창이 열려 있으면 키를 가져감
const NOT_MOVING: &str = "목록이 움직이지 않아요. 채팅 입력 중이면 닫고 다시 시도해 주세요";
/// 정렬 창 키 간격, 탭 전환과 같음 (입력이 씹힐 수 있어서 단계마다 화면으로 확인)
const POPUP_MS: u64 = 40;

/// 정렬 바꾸기에 필요한 게임 쪽 기능
trait SortIo {
    type Frame;
    /// `wait` 안에 멈춘 목록, 즉 정렬 창이 닫힌 상태
    /// `check`는 기다리는 동안과 프레임을 받은 뒤에 돌림 (마지막 키 뒤 사용자가 건드렸을 수 있음)
    fn list(&mut self, wait: Duration, check: &dyn Fn() -> Result<(), Stop>) -> Result<Option<Self::Frame>, Stop>;
    /// `wait` 안에 두 프레임 연속 같게 나온 정렬 창의 강조 칸
    fn popup(&mut self, wait: Duration) -> Result<Option<sort::Cell>, Stop>;
    fn key(&mut self, k: EKey) -> Result<(), Stop>;
    fn popup_key(&mut self, k: PopupKey) -> Result<(), Stop>;
}

struct LiveSort<'a> {
    screen: &'a Screen,
    cancel: &'a AtomicBool,
}

impl LiveSort<'_> {
    fn go_on(&self) -> Result<(), Stop> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        Ok(())
    }
}

impl SortIo for LiveSort<'_> {
    type Frame = (dmpicker_engine::Rgb, ListGeom);

    fn list(&mut self, wait: Duration, check: &dyn Fn() -> Result<(), Stop>) -> Result<Option<Self::Frame>, Stop> {
        // 기다리는 중에 건드리면 멈추도록 짧게 끊어서 기다림
        let t0 = Instant::now();
        loop {
            check()?;
            let left = wait.saturating_sub(t0.elapsed());
            if let Some(frame) = self.screen.settled(self.cancel, left.min(Duration::from_millis(300)))? {
                check()?; // 프레임을 찍는 동안 안 건드렸음
                return Ok(Some(frame));
            }
            if t0.elapsed() >= wait {
                return Ok(None);
            }
        }
    }

    fn popup(&mut self, wait: Duration) -> Result<Option<sort::Cell>, Stop> {
        let t0 = Instant::now();
        let mut last = None;
        loop {
            self.go_on()?;
            let cell = sort::highlighted(&self.screen.grab()?);
            if cell.is_some() && cell == last {
                return Ok(cell);
            }
            last = cell;
            if t0.elapsed() > wait {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn key(&mut self, k: EKey) -> Result<(), Stop> {
        self.go_on()?;
        input::press(self.screen.game, k, POPUP_MS, POPUP_MS).map_err(|_| Stop::Touched(FOCUS_LEFT))
    }

    fn popup_key(&mut self, k: PopupKey) -> Result<(), Stop> {
        self.go_on()?;
        input::press_popup(self.screen.game, k, POPUP_MS, POPUP_MS).map_err(|_| Stop::Touched(FOCUS_LEFT))
    }
}

/// 게임 정렬을 저장 목록 순서인 곡 제목 A to Z로 바꾸고 그 뒤 멈춘 목록을 돌려줌
/// 프리스타일과 온라인 방은 같은 정렬 창으로 정렬을 따로 가짐, 원래 정렬로 되돌리지 않음
///
/// 아직 안 먹은 키가 늦게 먹을 수 있어서 효과가 안 보여도 키를 더 보내지 않음
/// F4는 한 번만 (또 누르면 방금 연 정렬 창을 닫을 수 있음)
/// Enter·Esc 뒤엔 아무 키도 안 누름 (목록에서 Enter는 곡 시작, 온라인에선 채팅 열기, Esc는 메뉴 열기)
/// Enter는 두 프레임 연속 곡 제목 A to Z에 강조가 보일 때 누름
/// Esc는 떠 있는 정렬 창이 바꿀 필요가 없거나(이미 제목 A to Z) 방향키로 못 맞췄을 때만, Enter 없이
/// `check`(취소, 손 떼기, 포커스)는 키마다 직전에, 목록을 기다리는 동안과 돌아온 뒤에도 돌림
/// 마지막 키 뒤에 건드려도 작업을 멈춤
fn sort_by_title<S: SortIo>(io: &mut S, check: &dyn Fn() -> Result<(), Stop>) -> Result<S::Frame, Stop> {
    let fail = |why: &str| Stop::Failed(why.into());
    if io.list(Duration::from_millis(1500), check)?.is_none() {
        diagnostic!("sort: no list at rest");
        return Err(fail(SORT_ASK));
    }
    check()?;
    io.key(EKey::F4)?;
    let Some(mut at) = io.popup(Duration::from_millis(600))? else {
        diagnostic!("sort: no popup after F4");
        return Err(fail(SORT_ASK));
    };
    diagnostic!("sort: popup open on {at:?}");
    if at == sort::TITLE_ASC {
        // 이미 제목 A to Z면 아무것도 적용하지 않고 닫음
        check()?;
        io.popup_key(PopupKey::Esc)?;
        if let Some(frame) = io.list(Duration::from_secs(3), check)? {
            diagnostic!("sort: already title A to Z");
            return Ok(frame);
        }
        diagnostic!("sort: no list after Esc");
        return Err(fail(SORT_ASK));
    }
    // 위·왼쪽으로만 감, 칸 끝에서 멈추니 늦게 먹은 입력이 강조를 구석에서 다시 빼낼 수 없음
    for _ in 0..3 {
        for k in (0..at.row).map(|_| EKey::Up).chain((0..at.col).map(|_| EKey::Left)) {
            check()?;
            io.key(k)?;
        }
        let Some(now) = io.popup(Duration::from_millis(600))? else {
            diagnostic!("sort: popup gone while moving");
            return Err(fail(SORT_ASK));
        };
        at = now;
        if at == sort::TITLE_ASC {
            break;
        }
    }
    if at != sort::TITLE_ASC {
        diagnostic!("sort: highlight stays on {at:?}");
        if io.popup(Duration::from_millis(300))?.is_some() {
            check()?;
            io.popup_key(PopupKey::Esc)?;
        }
        return Err(fail(SORT_ASK));
    }
    check()?;
    io.popup_key(PopupKey::Enter)?;
    if let Some(frame) = io.list(Duration::from_secs(3), check)? {
        diagnostic!("sort: title A to Z");
        return Ok(frame);
    }
    diagnostic!("sort: no list after Enter");
    Err(fail(SORT_ASK))
}

/// 곡 이동과 다시 보기용 이동기, 처음부터 입력당 25 ms이고 더 빠르게는 안 함
/// 더 빠르면 목록이 멈춘 듯한 뒤에 입력이 늦게 먹어서 커서가 본 칸을 지나칠 수 있음
fn navigator(model: &ListModel) -> Navigator {
    let mut nav = Navigator::new(model, NavConfig { max_burst: 16, max_rounds: 15, ..NavConfig::default() });
    nav.tuner = TimingTuner::new(SAFE_MS as f32, SAFE_MS as f32, 120.0);
    nav
}

/// 인식에서 확정 못 한 칸은 지나가며 잘못 읽은 경우가 많아서 하나씩 가서 화면 가운데에 멈춘 채로 다시 읽음
/// 취소, 포커스 잃음, 키·마우스 입력이 있으면 이유와 함께 멈춤 (그러면 목록 저장 안 함)
/// 못 가는 칸은 건너뜀, `progress`에는 끝낸 칸 수가 들어감
fn recheck(
    screen: &Screen,
    ctx: &Ctx,
    l: &mut Learned,
    which: &[usize],
    cancel: &AtomicBool,
    hands: &HandsOff,
    mut progress: impl FnMut(usize),
) -> Result<(), Stop> {
    let model = match l.model(&|id| ctx.title(id)) {
        Ok(m) => m,
        Err(e) => {
            diagnostic!("recheck: no model ({e}), {} entries not looked at", which.len());
            return Ok(());
        }
    };
    let mut nav = navigator(&model);
    for (done, &i) in which.iter().enumerate() {
        progress(done);
        hands.check()?;
        let mut io = LiveIo { screen, refs: &ctx.refs, learned: l, cancel, first: None, found: None, pressed: None, geom: None, mode_seen: None };
        match nav.navigate(&mut io, i) {
            Ok(_) => {}
            Err(NavError::Aborted(e @ (Abort::Cancelled | Abort::FocusLost))) => return Err(e.into()),
            Err(e) => {
                diagnostic!("recheck {i}: not reached ({e})");
                continue;
            }
        }
        let pressed = io.pressed;
        let (img, geom) = match screen.settled(cancel, Duration::from_millis(1500)) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                diagnostic!("recheck {i}: the list did not come to rest");
                continue;
            }
            Err(e) => {
                diagnostic!("recheck {i}: no frame ({e:?})");
                continue;
            }
        };
        hands.check()?; // 프레임을 찍는 동안 안 건드렸음
        let row = dmpicker_engine::read::read_row(&img, &geom, &ctx.refs, dmpicker_engine::SEL_ROW);
        if l.reread(i, &row, &ctx.titles) {
            continue;
        }
        // 커서가 막 도착했을 땐 줄이 아직 바뀌는 중일 수 있음, 흐려짐이 끝나면 한 번 더
        diagnostic!("recheck {i}: not confirmed, {}; reading again", l.unconfirmed(i, &row, &ctx.titles));
        if let Some(wait) = pressed.and_then(|t| FADE.checked_sub(t.elapsed())) {
            std::thread::sleep(wait);
        }
        let Ok(Some((img, geom))) = screen.settled(cancel, Duration::from_millis(1500)) else {
            diagnostic!("recheck {i}: no frame at rest to read again");
            continue;
        };
        hands.check()?;
        let row = dmpicker_engine::read::read_row(&img, &geom, &ctx.refs, dmpicker_engine::SEL_ROW);
        if !l.reread(i, &row, &ctx.titles) {
            diagnostic!("recheck {i}: not confirmed when read again, {}", l.unconfirmed(i, &row, &ctx.titles));
        }
    }
    if !which.is_empty() {
        progress(which.len());
    }
    Ok(())
}

/// 원형 목록이 제목 순서상 거꾸로 가는 곳들 (앞 칸, 뒤 칸), 목록 끝은 뺌
/// 끝은 제목 그룹이 가장 크게 떨어지는 곳 (마지막 그룹에서 첫 그룹으로)
/// 제목순으로 읽은 목록이면 나머지는 오인식 칸이거나 게임이 [`dmpicker_nav::order_key`]와 다르게 정렬하는 제목
fn steps_back_at(l: &Learned, titles: &HashMap<u32, String>) -> Vec<(usize, usize)> {
    let known: Vec<(usize, OrderKey)> =
        l.entries.iter().enumerate().filter_map(|(i, e)| Some((i, order_key(titles.get(&e.id?)?)?))).collect();
    let n = known.len();
    // (그룹이 떨어진 정도, 앞 칸, 뒤 칸)
    let mut back: Vec<(usize, usize, usize)> = (0..n)
        .filter_map(|k| {
            let ((i, a), (j, b)) = (&known[k], &known[(k + 1) % n]);
            goes_back(a, b).then(|| (a.0.rank().saturating_sub(b.0.rank()), *i, *j))
        })
        .collect();
    if let Some(end) = (0..back.len()).max_by_key(|&k| back[k].0) {
        back.remove(end);
    }
    back.into_iter().map(|(_, i, j)| (i, j)).collect()
}

/// 다시 볼 칸에 제목 순서가 거꾸로 가는 곳마다 양옆 칸을 더함
fn recheck_targets(doubtful: Vec<usize>, l: &Learned, titles: &HashMap<u32, String>) -> Vec<usize> {
    let mut all: std::collections::BTreeSet<usize> = doubtful.into_iter().collect();
    all.extend(steps_back_at(l, titles).into_iter().flat_map(|(i, j)| [i, j]));
    all.into_iter().collect()
}

/// 목록이 제목순이 아님, 끝을 빼고 거꾸로 가는 횟수가 오인식 몇 칸(곡의 1 %까지)으로 볼 수 있는 것보다 많음
/// 다른 정렬은 BPM, 레벨, 업데이트가 바뀔 때마다 거꾸로 감
fn out_of_order(l: &Learned, titles: &HashMap<u32, String>) -> bool {
    let back = steps_back_at(l, titles).len();
    // 칸이 아니라 곡 수로 셈, 레벨 정렬은 곡을 채보마다 한 번씩 보여줌
    let songs = l.entries.iter().filter_map(|e| e.id).collect::<HashSet<_>>().len();
    let allowed = (songs / 100).max(5);
    if back > allowed {
        diagnostic!("scan: the list goes back {back} times in title order besides its end (at most {allowed})");
        return true;
    }
    false
}

/// ALL TRACK 전체 인식 (사용자가 직접 실행할 때만)
pub fn run_scan(ctx: &Ctx, cancel: &AtomicBool, progress: impl FnMut(ScanStage)) -> JobResult {
    crate::fail_log::begin();
    let result = scan_job(ctx, cancel, progress);
    crate::fail_log::finish(&ctx.store, crate::fail_log::Kind::Scan, &result);
    result
}

fn scan_job(ctx: &Ctx, cancel: &AtomicBool, mut progress: impl FnMut(ScanStage)) -> JobResult {
    let screen = match focused_screen() {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (img, geom) = match goto_all_track(&screen, cancel) {
        Ok(f) => f,
        Err(stop) => return stop.into(),
    };
    if cancel.load(Ordering::Relaxed) {
        return JobResult::Cancelled;
    }
    let t0 = Instant::now();
    let kind = layout_kind(&geom);
    let hands = HandsOff::begin(screen.game, ctx.hotkey);
    let saved = Learned::load(&ctx.store, kind);
    // 목록은 제목순으로 익힘, 정렬 창으로 순서를 확인하고 맞춤
    // 사용자 키를 감시 못 하면 저장 목록에서 화면 위치를 찾을 때만 진행
    if hands.watching() {
        progress(ScanStage::Sorting);
        if let Err(stop) = sort_by_title(&mut LiveSort { screen: &screen, cancel }, &|| hands.check()) {
            return stop.into();
        }
        progress(ScanStage::Reading(0.0));
    } else if !saved.as_ref().is_some_and(|l| placed(&read_window(&img, &geom, &ctx.refs), l)) {
        diagnostic!("scan: the sort is unknown and the keys are not watched");
        return JobResult::Failed(SORT_ASK.into());
    }
    // 저장된 목록 길이, 처음이면 곡 데이터의 모든 곡과 RANDOM (ALL TRACK엔 가진 곡만 있어서 더 길게 잡힘)
    // 그리고 끝을 지나 더 읽는 구간
    let songs = saved.map_or(ctx.titles.len() + 1, |l| l.entries.len());
    let est = songs as i64 + PAST_END;
    let reading = |seen: usize| if (seen as i64) < est { ScanStage::Reading(seen as f32 / est as f32) } else { ScanStage::FindingEnd };
    match scan(&screen, &ctx.refs, cancel, &hands, |seen| progress(reading(seen))) {
        Ok(pending) => {
            // 다른 순서로 읽힌 목록은 다시 봐도 오래 걸리기만 하고 저장 못 함
            if out_of_order(&pending.learned, &ctx.titles) {
                return JobResult::Failed(MISREAD.into());
            }
            progress(ScanStage::Reading(1.0));
            let (mut l, doubtful) = pending.repair(&|id| ctx.title(id));
            // 제목 순서가 거꾸로 가는 곳은 양옆 중 한 칸이 오인식일 가능성이 큼
            let doubtful = recheck_targets(doubtful, &l, &ctx.titles);
            let at = Instant::now();
            if let Err(stop) = recheck(&screen, ctx, &mut l, &doubtful, cancel, &hands, |done| progress(ScanStage::Rechecking(done, doubtful.len()))) {
                return stop.into();
            }
            diagnostic!("recheck: {} entries in {:.1}s", doubtful.len(), at.elapsed().as_secs_f32());
            if cancel.load(Ordering::Relaxed) {
                return JobResult::Cancelled;
            }
            if let Err(stop) = hands.check() {
                return stop.into();
            }
            l.unlock_grey_art(&ctx.refs);
            let unknown = l.entries.iter().filter(|e| e.id.is_none() && !e.random && !e.locked).count();
            diagnostic!("learned: {} entries, {} unknown after repair", l.entries.len(), unknown);
            // 방해받은 인식은 빈칸이 많음, 이번 것보다 기존 목록을 유지
            if unknown > (l.entries.len() / 100).max(5) {
                return JobResult::Failed(MISREAD.into());
            }
            if let Err(e) = l.model(&|id| ctx.title(id)) {
                diagnostic!("scan: no model ({e})");
                return JobResult::Failed(MISREAD.into());
            }
            // 저장 전 마지막 확인, 목록 전체를 곡마다 제목 순서로 봄
            // 그래도 거꾸로 가는 곳은 로그에 남김 (오인식, 또는 게임이 `order_key`와 다르게 정렬하는 제목)
            for (i, j) in steps_back_at(&l, &ctx.titles) {
                let title = |k: usize| l.entries[k].id.map(|id| ctx.title(id)).unwrap_or_default();
                diagnostic!("scan: title order goes back from {:?} ({i}) to {:?} ({j})", title(i), title(j));
            }
            if out_of_order(&l, &ctx.titles) {
                return JobResult::Failed(MISREAD.into());
            }
            if let Err(e) = l.save(&ctx.store) {
                diagnostic!("save: {e}");
                return JobResult::Failed("목록을 저장하지 못했어요".into());
            }
            // 다른 모드 목록도 새 인식에 맞춤
            if let Err(e) = l.save_derived(&ctx.store, &|id| ctx.title(id)) {
                diagnostic!("derive {}: {e}", kind_name(other_kind(l.kind)));
            }
            JobResult::Scanned { entries: l.entries.len(), unknown, secs: t0.elapsed().as_secs_f32() }
        }
        Err(stop) => stop.into(),
    }
}

pub struct Ctx {
    pub refs: Arc<RefSet>,
    pub store: PathBuf,
    pub titles: Arc<HashMap<u32, String>>,
    /// 검색창 단축키, 작업 중엔 멈추는 키
    pub hotkey: Hotkey,
}

impl Ctx {
    fn title(&self, id: u32) -> String {
        self.titles.get(&id).cloned().unwrap_or_default()
    }
}

pub fn run_pick(ctx: &Ctx, song: u32, mode: ButtonMode, diff: usize, cancel: &AtomicBool) -> JobResult {
    crate::fail_log::begin();
    let result = pick_job(ctx, song, mode, diff, cancel);
    crate::fail_log::finish(&ctx.store, crate::fail_log::Kind::Pick, &result);
    result
}

fn pick_job(ctx: &Ctx, song: u32, mode: ButtonMode, diff: usize, cancel: &AtomicBool) -> JobResult {
    let screen = match focused_screen() {
        Ok(s) => s,
        Err(r) => return r,
    };
    // 방장이 아니면 키를 하나도 안 누름, 화면을 못 읽으면 막지 않음
    if screen.grab().is_ok_and(|img| room::not_host(&img)) {
        return JobResult::Failed(NOT_HOST.into());
    }
    diagnostic!("job: focused, reading screen");
    let first = match goto_all_track(&screen, cancel) {
        Ok(f) => f,
        Err(stop) => return stop.into(),
    };
    let kind = layout_kind(&first.1);
    diagnostic!("screen: {}", kind_name(kind));

    ensure_both(&ctx.store, &|id| ctx.title(id));
    let Some(mut learned) = Learned::load(&ctx.store, kind) else { return JobResult::NeedScan };
    if learned.index_of(song).is_none() {
        return JobResult::NotFound;
    }
    // 목록은 제목순이라 위치를 못 찾는 화면은 다른 정렬일 수 있음
    // 한 번 잘못 읽었을 수도 있어서 먼저 프레임을 하나 더 봄
    // 프레임 하나에서 11줄, 목록 위치, 버튼 모드 표시
    let look = |img: &dmpicker_engine::Rgb, geom: &ListGeom| (read_window(img, geom, &ctx.refs), *geom, dmpicker_engine::panel::button_mode(img, kind, geom));
    let (mut rows, mut geom, mut mode_seen) = look(&first.0, &first.1);
    if !placed(&rows, &learned) {
        match screen.settled(cancel, Duration::from_millis(1500)) {
            Ok(Some((img, g))) => (rows, geom, mode_seen) = look(&img, &g),
            Ok(None) => {}
            Err(e) => return Stop::from(e).into(),
        }
    }
    if !placed(&rows, &learned) {
        diagnostic!("job: the screen does not fit the list, sorting by title");
        // Enter 직전에 사용자가 키를 누르면 정렬 창이 먼저 닫힐 수 있음, 키 감시 없이는 안 바꿈
        let hands = HandsOff::begin(screen.game, ctx.hotkey);
        if !hands.watching() {
            return JobResult::Failed(SORT_ASK.into());
        }
        match sort_by_title(&mut LiveSort { screen: &screen, cancel }, &|| hands.check()) {
            Ok((img, g)) => (rows, geom, mode_seen) = look(&img, &g),
            Err(stop) => return stop.into(),
        }
    }
    let mut first_rows = Some(rows);
    // 온라인엔 목록에 없는 LOCKED 곡이 보일 수 있음, 하나씩 익히고 다시 이동
    for _ in 0..4 {
        let Some(target) = learned.index_of(song) else { return JobResult::NotFound };
        diagnostic!("job: target index {target}");
        let model = match learned.model(&|id| ctx.title(id)) {
            Ok(m) => m,
            Err(e) => {
                diagnostic!("model: {e}");
                return JobResult::ListMismatch;
            }
        };
        if !model.anomalies().is_empty() {
            diagnostic!("model anomalies: {:?}", model.anomalies());
        }
        let mut nav = navigator(&model);
        let mut io = LiveIo {
            screen: &screen,
            refs: &ctx.refs,
            learned: &learned,
            cancel,
            first: first_rows.take(),
            found: None,
            pressed: None,
            geom: Some(geom),
            mode_seen,
        };
        let res = nav.navigate(&mut io, target);
        let found = io.found.take();
        geom = io.geom.unwrap_or(geom);
        mode_seen = io.mode_seen;
        match res {
            Ok(o) => {
                diagnostic!(
                    "nav ok: presses {} rounds {} mismatches {} dropped {} {}ms hold {}",
                    o.presses, o.rounds, o.mismatches, o.dropped, o.elapsed_ms, o.hold_ms
                );
                diagnostic!("job: arrived, applying mode/difficulty");
                let diff_note = match apply_chart(&screen, mode, diff, o.hold_ms.max(30), mode_seen, cancel) {
                    Ok(note) => note,
                    Err(e @ (Abort::Cancelled | Abort::FocusLost)) => return Stop::from(e).into(),
                    Err(e) => {
                        diagnostic!("chart: {e:?}");
                        Some(CHART_ASK.into())
                    }
                };
                return JobResult::Arrived { presses: o.presses, ms: o.elapsed_ms, diff_note };
            }
            Err(NavError::Aborted(Abort::ListChanged)) => {
                diagnostic!("nav: the screen differs from the saved list");
                let Some((at, e)) = found else { break };
                diagnostic!("learned LOCKED entry at {at}: {}", e.id.map(|id| ctx.title(id)).unwrap_or_else(|| "?".into()));
                learned.entries.insert(at, e);
                if let Err(e) = learned.save(&ctx.store) {
                    diagnostic!("save: {e}");
                }
            }
            Err(e) => return nav_failed(e),
        }
    }
    JobResult::ListMismatch
}

/// 도착 못 한 이동을 사용자에게 알릴 결과, 저장 목록으로 못 가면 목록이 어긋난 걸로 봄
fn nav_failed(e: NavError) -> JobResult {
    diagnostic!("nav: {e}");
    match e {
        NavError::Aborted(Abort::ListChanged) | NavError::PositionUnknown => JobResult::ListMismatch,
        NavError::Aborted(e) => Stop::from(e).into(),
        NavError::NotMoving => JobResult::Failed(NOT_MOVING.into()),
        NavError::NotOnList => JobResult::Failed(NOT_HERE.into()),
        _ => JobResult::ListMismatch,
    }
}

fn read_slots(screen: &Screen, cancel: &AtomicBool) -> Result<Option<[dmpicker_engine::SlotState; 4]>, Abort> {
    // 곡이나 모드를 바꾸면 조금 뒤 난이도 칸이 다시 그려짐, 목록이 멈춘 뒤 60 ms 더 기다렸다가 새로 찍어 읽음
    if screen.settled(cancel, Duration::from_millis(1200))?.is_none() {
        return Ok(None);
    }
    std::thread::sleep(Duration::from_millis(60));
    Ok(dmpicker_engine::analyze(&screen.grab()?).slot_states)
}

/// 목록이 멈춘 뒤 화면의 버튼 모드 (4, 5, 6, 8), 못 읽으면 `None`
fn read_mode(screen: &Screen, cancel: &AtomicBool) -> Result<Option<u8>, Abort> {
    let Some((img, geom)) = screen.settled(cancel, Duration::from_millis(1200))? else { return Ok(None) };
    Ok(dmpicker_engine::panel::button_mode(&img, layout_kind(&geom), &geom))
}

/// 난이도 키를 누른 뒤, 멈춘 화면에 목표 난이도 칸만 채워진 게 연속 두 번 보이면 true
/// 다른 모드 표시가 보이면 안 셈 (모드를 바꾸면 칸이 늦게 다시 그려짐)
/// 600 ms 안에 안 보이면 false, 칸을 다시 읽고 다시 맞춤
fn shows_chart(screen: &Screen, mode: ButtonMode, diff: usize, cancel: &AtomicBool) -> Result<bool, Abort> {
    let t0 = Instant::now();
    let mut hits = 0;
    while t0.elapsed() < Duration::from_millis(600) {
        if cancel.load(Ordering::Relaxed) {
            return Err(Abort::Cancelled);
        }
        let img = screen.grab()?;
        let info = dmpicker_engine::analyze(&img);
        let shown = info.list.filter(|g| info.at_rest && g.on_grid()).is_some_and(|g| {
            let m = dmpicker_engine::panel::button_mode(&img, info.kind, &g);
            let d = info.slot_states.as_ref().and_then(dmpicker_engine::panel::selected_difficulty);
            m.is_none_or(|m| m == mode.buttons()) && d == Some(dmpicker_engine::panel::Difficulty::ALL[diff])
        });
        hits = if shown { hits + 1 } else { 0 };
        if hits >= 2 {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    Ok(false)
}

/// 난이도 이름 뒤에 읽는 소리에 맞춰 (으)로를 붙임, NM(엔엠)은 으로, HD·MX·SC는 로
fn label_ro(d: Diff) -> String {
    format!("{}{}", d.label(), if d == Diff::NM { "으로" } else { "로" })
}

/// 버튼 모드나 난이도를 못 맞췄거나 확인 못 함, 사용자가 직접 확인
const CHART_ASK: &str = "버튼 모드나 난이도를 맞추지 못했어요\n게임에서 직접 확인해 주세요";

/// 곡에 도착한 뒤 버튼 모드와 난이도를 맞추고 화면으로 확인, 하나라도 못 맞추면 사용자용 안내 문구를 돌려줌
/// 모드 키는 모드를 바로 지정하니 표시가 아직 다른 모드면 다시 눌러도 됨 (최대 두 번 더)
/// 모드 표시를 못 읽으면 확인 안 함, `mode_seen`은 도착한 멈춘 화면의 모드 표시
fn apply_chart(screen: &Screen, mode: ButtonMode, diff: usize, t: u32, mode_seen: Option<u8>, cancel: &AtomicBool) -> Result<Option<String>, Abort> {
    use dmpicker_engine::SlotState;
    let press = |k: NKey| {
        if cancel.load(Ordering::Relaxed) {
            return Err(Abort::Cancelled);
        }
        press_engine(screen.game, k, t, t)
    };
    let mut mode_failed = false;
    for attempt in 0..3 {
        press(NKey::Mode(mode))?;
        // 이미 그 모드였으면 바뀌는 게 없어서 안 기다림
        // 모드가 바뀌면 난이도 칸이 늦게 다시 그려져서 기다렸다가 확인
        if attempt == 0 && mode_seen == Some(mode.buttons()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        match read_mode(screen, cancel)? {
            Some(m) if m != mode.buttons() => {
                diagnostic!("chart: {m}B on screen after the {}B key", mode.buttons());
                mode_failed = attempt == 2;
            }
            _ => break,
        }
    }
    let diff_note = || -> Result<Option<String>, Abort> {
        for _attempt in 0..3 {
            let Some(states) = read_slots(screen, cancel)? else { return Ok(Some(CHART_ASK.into())) };
            let avail: [bool; 4] = states.map(|s| s != SlotState::Missing);
            let Some(cur) = states.iter().position(|s| *s == SlotState::Selected) else {
                std::thread::sleep(Duration::from_millis(120));
                continue;
            };
            if cur == diff {
                return Ok(None);
            }
            if !avail[diff] {
                return Ok(Some(format!("{} 채보가 없는 곡이에요\n{} 두었어요", Diff::ALL[diff].label(), label_ro(Diff::ALL[cur]))));
            }
            let Some(keys) = dmpicker_nav::diff_keys_from(avail, cur, diff) else { return Ok(Some(CHART_ASK.into())) };
            for k in keys {
                press(k)?;
            }
            if shows_chart(screen, mode, diff, cancel)? {
                return Ok(None);
            }
        }
        Ok(Some(CHART_ASK.into()))
    };
    let note = diff_note()?;
    Ok(if mode_failed { Some(CHART_ASK.into()) } else { note })
}
