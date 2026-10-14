//! 이동 실행: 계획, 키 묶음 전송, 화면 확인, 재계획, 타이밍 조정을 반복

use std::collections::BTreeMap;
use std::fmt;

use crate::keys::Key;
use crate::model::ListModel;
use crate::plan::{CostWeights, DistField, Planner};

/// I/O 계층이 이동을 멈추는 이유, 실행기는 재시도 안 함
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Abort {
    /// 게임이 더 이상 실제 전면 창이 아님
    FocusLost,
    /// 사용자가 취소함 (단축키, UI)
    Cancelled,
    /// 사용자가 키·클릭·휠로 게임을 직접 건드림
    Touched,
    /// 온라인 방 채팅 입력창이 열려서 키가 목록 대신 글자로 들어감
    ChatOpen,
    /// I/O 계층이 학습한 목록을 고침 (화면에 보이는 곡이 목록에 없었음)
    /// 모델을 다시 만들고 다시 이동해야 함
    ListChanged,
    /// 그 밖의 I/O 계층 오류
    Io(String),
}

/// 목록이 멈춘 뒤 화면을 읽은 결과
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    /// 커서가 학습한 목록의 이 인덱스에 있음
    At(usize),
    /// 목록은 보이는데 커서 위치를 못 찾음
    Unknown,
    /// 곡 목록 화면이 아님 (다른 메뉴, 팝업, 목록 바뀜 등)
    NotOnList,
}

/// 실행기가 게임과 닿는 유일한 통로
pub trait GameIo {
    /// `key`를 `hold_ms` 동안 누르고 뗀 뒤 `gap_ms` 기다렸다가 반환
    /// 게임이 포커스를 잃었으면 보내지 말고 [`Abort::FocusLost`]로 실패해야 함
    fn press(&mut self, key: Key, hold_ms: u32, gap_ms: u32) -> Result<(), Abort>;
    /// 목록이 멈추길 기다렸다가 커서 위치를 찾음
    fn read_pos(&mut self) -> Result<Observation, Abort>;
    /// 움직이는 중인 화면에서 보이는 위치, 기다리지 않음 (못 읽으면 `None`)
    /// 화면이 커서보다 늦게 따라와서 대략적인 위치임
    fn glance(&mut self) -> Result<Option<usize>, Abort> {
        Ok(None)
    }
    /// 단조 증가 시계, ms 단위
    fn now_ms(&mut self) -> u64 {
        static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
    }
}

/// 키 누름·뗌 시간 자동 조정 (`t` = hold = gap), 이동이 끝나도 유지
///
/// * 40 ms에서 시작, 묶음이 다 먹히면 `t`를 *검증됨*으로 두고 다음 묶음은 `t * 0.9`로 시험
///   (`floor_ms`, `margin * bad` 아래로는 안 내림)
/// * 검증값보다 낮춰 시험하다 누락되면 바로 검증값으로 복귀
///   시험한 값은 *의심*으로 두고, 비슷한 값에서 또 누락되면 너무 짧다고 보고 `bad`에 기록
///   거기서 성공하면 의심 해제 (첫 실패는 끊김이었음)
/// * 검증값에서 누락되면 대개 끊김이라 그대로 재시도 (곡 바뀔 때 버벅이면 타이밍과 무관하게 놓침)
///   `hitch_patience`번 넘게 연달아 실패해야 프레임이 진짜 떨어진 것, 검증 버리고 `t *= 1.5`
/// * 검증값이 없을 때 (시작 직후, 프레임 저하 뒤): 묶음의 `timing_drop_fraction` 이상
///   (2번 이상) 누락되면 `t *= 1.5`, 아니면 위처럼 끊김으로 셈
/// * 묶음이 통째로 누락되면 검증값이어도 검증 버리고 `t *= 1.5`
///
/// `t`는 `ceil_ms`를 안 넘음
/// `bad`는 성공마다 `bad_decay`씩 줄어서 가끔 더 빠른 값도 다시 시험함 (fps 제한을 올린 뒤 등)
#[derive(Clone, Debug, PartialEq)]
pub struct TimingTuner {
    t: f32,
    pub floor_ms: f32,
    pub ceil_ms: f32,
    pub decrease: f32,
    pub increase: f32,
    pub margin: f32,
    pub bad_decay: f32,
    pub timing_drop_fraction: f32,
    pub hitch_patience: u32,
    proven: Option<f32>,
    suspect: Option<f32>,
    bad: Option<f32>,
    hitch_streak: u32,
}

impl Default for TimingTuner {
    fn default() -> Self {
        TimingTuner::new(40.0, 12.0, 120.0)
    }
}

impl TimingTuner {
    pub fn new(start_ms: f32, floor_ms: f32, ceil_ms: f32) -> Self {
        TimingTuner {
            t: start_ms.clamp(floor_ms, ceil_ms),
            floor_ms,
            ceil_ms,
            decrease: 0.9,
            increase: 1.5,
            margin: 1.1,
            bad_decay: 0.997,
            timing_drop_fraction: 0.5,
            hitch_patience: 3,
            proven: None,
            suspect: None,
            bad: None,
            hitch_streak: 0,
        }
    }
    /// 지금 키를 누르고 있는 시간
    pub fn hold_ms(&self) -> u32 {
        self.t.ceil() as u32
    }
    /// 지금 키를 뗀 뒤 기다리는 시간
    pub fn gap_ms(&self) -> u32 {
        self.t.ceil() as u32
    }
    /// 묶음이 전부 먹힘
    pub fn on_success(&mut self) {
        self.hitch_streak = 0;
        self.proven = Some(self.t);
        if self.suspect.is_some_and(|s| self.t <= s) {
            self.suspect = None;
        }
        if let Some(b) = &mut self.bad {
            *b *= self.bad_decay;
        }
        let lower = self.bad.map_or(self.floor_ms, |b| (b * self.margin).max(self.floor_ms));
        self.t = (self.t * self.decrease).max(lower).min(self.t);
    }
    /// `sent`번 보낸 묶음에서 `dropped`번 누락됨
    pub fn on_drops(&mut self, sent: usize, dropped: usize) {
        let t = self.t;
        match self.proven {
            Some(g) if t < g => {
                // 검증값보다 낮춘 시험이 실패함
                if self.suspect.is_some_and(|s| t <= s * 1.02) {
                    self.bad = Some(self.bad.map_or(t, |b| b.max(t)));
                    self.suspect = None;
                } else {
                    self.suspect = Some(t);
                }
                self.t = g;
                self.hitch_streak = 0;
            }
            None if dropped >= 2 && dropped as f32 >= self.timing_drop_fraction * sent as f32 => {
                self.t = (t * self.increase).min(self.ceil_ms);
                self.hitch_streak = 0;
            }
            // 묶음이 통째로 안 먹으면 바로 느리게 함
            // 한 키짜리 보정은 끊김으로 세다 보면 목록이 안 움직인다고 먼저 포기함 (30 fps 아래에서 25 ms 키)
            _ if dropped >= sent => {
                self.proven = None;
                self.suspect = None;
                self.t = (t * self.increase).min(self.ceil_ms);
                self.hitch_streak = 0;
            }
            _ => {
                // 검증값에서 실패 (또는 검증값 없이 조금 누락): 끊김으로 봄
                self.hitch_streak += 1;
                if self.hitch_streak > self.hitch_patience {
                    self.proven = None;
                    self.suspect = None;
                    self.t = (t * self.increase).min(self.ceil_ms);
                    self.hitch_streak = 0;
                }
            }
        }
    }
}

/// 실행기 한도와 정책
#[derive(Clone, Debug, PartialEq)]
pub struct NavConfig {
    /// 확인 지점 사이 키 수, 기본 8 (보수적)
    /// 지점마다 곁눈질([`GameIo::glance`])이 예상 위치 둘레면 멈추지 않고 이어 보냄
    /// 못 읽거나 어긋나면 거기서 묶음을 끊고 멈춘 화면으로 확인
    pub max_burst: usize,
    /// 글자/PgUp/PgDn 바로 뒤에서 묶음을 끊음 (설명 안 되는 결과가 나온 뒤엔 항상 켜짐)
    pub verify_after_jump: bool,
    /// 이동 한 번에 허용하는 묶음+확인 횟수
    pub max_rounds: u32,
    /// 이동 한 번의 최대 시간 (ms, [`GameIo::now_ms`] 기준)
    pub timeout_ms: u64,
    /// 포기 전까지 봐주는 연속 `Unknown` 횟수
    pub max_unknown: u32,
    /// 포기 전까지 봐주는 연속 `NotOnList` 횟수 (목록 밖에선 키를 절대 안 보냄)
    pub max_not_on_list: u32,
    pub weights: CostWeights,
    /// 같은 설명 안 되는 결과를 두 번 보면 점프 이동을 고쳐서 학습
    pub learn_edges: bool,
}

impl Default for NavConfig {
    fn default() -> Self {
        NavConfig {
            max_burst: 8,
            verify_after_jump: false,
            max_rounds: 40,
            timeout_ms: 30_000,
            max_unknown: 3,
            max_not_on_list: 2,
            weights: CostWeights::default(),
            learn_edges: true,
        }
    }
}

/// 성공한 이동의 통계
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub final_pos: usize,
    /// 보낸 키 수
    pub presses: u32,
    /// 묶음+확인 횟수
    pub rounds: u32,
    /// 계획과 다른 곳에서 끝난 횟수
    pub mismatches: u32,
    /// 누락으로 설명된 키 수 (관찰 결과를 설명하는 최소 삭제 수)
    pub dropped: u32,
    /// 누락으로 설명 안 되는 불일치 (모델 오류, 키가 더 들어감, 오인식)
    pub unexplained: u32,
    /// 이번 이동 중 플래너에서 고친 이동 수
    pub learned: u32,
    pub elapsed_ms: u64,
    pub hold_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavError {
    /// I/O 계층이 중단함 (포커스 잃음, 취소 등), 남은 키는 버림
    Aborted(Abort),
    /// 화면에 곡 목록이 더 이상 안 보임
    NotOnList,
    /// 커서 위치를 계속 못 찾음
    PositionUnknown,
    Timeout { last_pos: Option<usize> },
    TooManyRounds { last_pos: usize },
    InvalidTarget(usize),
    Unreachable { from: usize, to: usize },
    /// 묶음 세 번 연속 커서가 그대로임, 키가 목록이 아닌 다른 곳(채팅창 등)으로 가는 중
    NotMoving,
}

impl fmt::Display for NavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NavError::Aborted(a) => write!(f, "aborted: {a:?}"),
            NavError::NotOnList => write!(f, "not on the song list"),
            NavError::PositionUnknown => write!(f, "cursor position unknown"),
            NavError::Timeout { last_pos } => write!(f, "timeout (last position {last_pos:?})"),
            NavError::TooManyRounds { last_pos } => write!(f, "too many attempts (last position {last_pos})"),
            NavError::InvalidTarget(t) => write!(f, "invalid target {t}"),
            NavError::Unreachable { from, to } => write!(f, "{to} unreachable from {from}"),
            NavError::NotMoving => write!(f, "the list does not move"),
        }
    }
}

impl std::error::Error for NavError {}

/// 곁눈질로 이어 갈 때 예상 위치와의 차이 한도 (줄)
/// 움직이는 화면은 커서보다 1줄쯤 늦게 따라옴
/// 그래서 방향키 하나 빠진 건 못 가리지만 방향키는 오차를 안 키워서 끝 확인에서 고침
const GLANCE_SLACK: usize = 3;
/// 점프 키 뒤 이만큼 더 보내고 곁눈질, 점프하면 목록이 잠깐 안 보였다가 다시 그려짐
const GLANCE_AFTER_JUMP: usize = 3;

/// `start`에서 `keys`를 눌러 `observed`에 닿으려면 최소 몇 번 누락돼야 하는지
/// 어떤 부분열로도 설명 안 되면 `None`
fn explain_drops(planner: &Planner, start: usize, keys: &[Key], observed: usize) -> Option<usize> {
    // 키마다 위치별 최소 누락 수를 구하는 DP
    let mut cur: BTreeMap<usize, usize> = BTreeMap::from([(start, 0)]);
    for key in keys {
        let mut nxt: BTreeMap<usize, usize> = BTreeMap::new();
        for (&p, &d) in &cur {
            let q = planner.apply(p, *key);
            let e = nxt.entry(q).or_insert(usize::MAX);
            *e = (*e).min(d);
            let e = nxt.entry(p).or_insert(usize::MAX);
            *e = (*e).min(d + 1);
        }
        cur = nxt;
    }
    cur.get(&observed).copied()
}

/// 목록 모델 하나의 플래너와 계속 유지되는 타이밍 상태
#[derive(Clone, Debug)]
pub struct Navigator {
    planner: Planner,
    pub cfg: NavConfig,
    pub tuner: TimingTuner,
    suspects: Vec<(usize, Key, usize)>,
}

impl Navigator {
    pub fn new(model: &ListModel, cfg: NavConfig) -> Self {
        Navigator {
            planner: Planner::new(model, cfg.weights),
            cfg,
            tuner: TimingTuner::default(),
            suspects: Vec::new(),
        }
    }
    pub fn planner(&self) -> &Planner {
        &self.planner
    }

    fn observe<I: GameIo>(&self, io: &mut I, t0: u64, last: Option<usize>) -> Result<usize, NavError> {
        let (mut unknown, mut off) = (0, 0);
        loop {
            match io.read_pos().map_err(NavError::Aborted)? {
                Observation::At(p) if p < self.planner.len() => return Ok(p),
                Observation::At(_) | Observation::Unknown => {
                    unknown += 1;
                    off = 0;
                    if unknown >= self.cfg.max_unknown {
                        return Err(NavError::PositionUnknown);
                    }
                }
                Observation::NotOnList => {
                    off += 1;
                    if off >= self.cfg.max_not_on_list {
                        return Err(NavError::NotOnList);
                    }
                }
            }
            if io.now_ms().saturating_sub(t0) > self.cfg.timeout_ms {
                return Err(NavError::Timeout { last_pos: last });
            }
        }
    }

    /// `pos`에서 `sent`까지 보낸 묶음을 이어 보내도 되는지, 곁눈질 결과는 이 판단에만 씀
    /// * `max_burst`키마다: 예상 위치 둘레로 보일 때만 이어 감, 못 읽어도 멈춤
    /// * 점프 키 뒤 [`GLANCE_AFTER_JUMP`]키: 멀리 보일 때만 멈춤, 빠진 점프를 일찍 잡음
    ///
    /// 확인 지점에서 이동 시간이 다 됐으면 멈춤, 그 뒤 확인에서 시간 초과로 끝남
    fn keep_going<I: GameIo>(&self, io: &mut I, t0: u64, pos: usize, sent: &[Key]) -> Result<bool, NavError> {
        let n = sent.len();
        let checkpoint = n.is_multiple_of(self.cfg.max_burst.max(1));
        let after_jump = n > GLANCE_AFTER_JUMP && sent[n - 1 - GLANCE_AFTER_JUMP].is_jump();
        if !checkpoint && !after_jump {
            return Ok(true);
        }
        let late = |io: &mut I| checkpoint && io.now_ms().saturating_sub(t0) > self.cfg.timeout_ms;
        if late(io) {
            return Ok(false);
        }
        let expected = self.planner.apply_all(pos, sent);
        let seen = io.glance().map_err(NavError::Aborted)?;
        if late(io) {
            return Ok(false);
        }
        let near = seen.is_some_and(|p| self.planner.ring_distance(p, expected) <= GLANCE_SLACK);
        Ok(near || (!checkpoint && seen.is_none()))
    }

    /// 커서를 `target`으로 옮김
    /// 위치 읽기, 계획, 묶음 전송, 확인, 본 위치에서 재계획을 도착하거나 한도에 걸릴 때까지 반복
    /// `Abort` 뒤, 목록 밖, 위치를 모를 때는 키를 절대 안 보냄
    ///
    /// 커서가 그대로인 묶음은 누락으로 셈, 세 번 연속이면 [`NavError::NotMoving`]으로 멈춤
    /// 먹히는 타이밍이면 묶음 전체가 세 번이나 누락될 리 없음
    pub fn navigate<I: GameIo>(&mut self, io: &mut I, target: usize) -> Result<Outcome, NavError> {
        let n = self.planner.len();
        if target >= n {
            return Err(NavError::InvalidTarget(target));
        }
        let t0 = io.now_ms();
        let mut out = Outcome::default();
        let mut pos = self.observe(io, t0, None)?;
        let mut field: DistField = self.planner.distances_to(target);
        let mut careful = self.cfg.verify_after_jump;
        // 커서가 그대로였던 연속 횟수
        let mut still = 0;
        loop {
            if pos == target {
                out.final_pos = pos;
                out.elapsed_ms = io.now_ms().saturating_sub(t0);
                out.hold_ms = self.tuner.hold_ms();
                return Ok(out);
            }
            if out.rounds >= self.cfg.max_rounds {
                return Err(NavError::TooManyRounds { last_pos: pos });
            }
            if io.now_ms().saturating_sub(t0) > self.cfg.timeout_ms {
                return Err(NavError::Timeout { last_pos: Some(pos) });
            }
            let plan = field.plan(&self.planner, pos);
            if plan.is_empty() {
                return Err(NavError::Unreachable { from: pos, to: target });
            }
            // 계획을 끝까지 한 묶음으로 보내고 확인 지점에서만 끊을지 봄
            let mut burst: Vec<Key> = Vec::with_capacity(plan.len());
            for key in &plan {
                burst.push(*key);
                if careful && key.is_jump() {
                    break;
                }
            }
            let (hold, gap) = (self.tuner.hold_ms(), self.tuner.gap_ms());
            let mut sent = 0;
            while sent < burst.len() {
                io.press(burst[sent], hold, gap).map_err(NavError::Aborted)?;
                out.presses += 1;
                sent += 1;
                if sent < burst.len() && !self.keep_going(io, t0, pos, &burst[..sent])? {
                    break;
                }
            }
            burst.truncate(sent);
            let expected = self.planner.apply_all(pos, &burst);
            out.rounds += 1;
            let obs = self.observe(io, t0, Some(pos))?;
            if obs == expected {
                self.tuner.on_success();
            } else {
                out.mismatches += 1;
                match explain_drops(&self.planner, pos, &burst, obs) {
                    Some(d) if d > 0 => {
                        out.dropped += d as u32;
                        self.tuner.on_drops(burst.len(), d);
                    }
                    _ => {
                        out.unexplained += 1;
                        careful = true;
                        if self.cfg.learn_edges && burst.len() == 1 && burst[0].is_jump() {
                            let s = (pos, burst[0], obs);
                            if self.suspects.contains(&s) {
                                self.suspects.retain(|x| x != &s);
                                if self.planner.override_edge(pos, burst[0], obs) {
                                    out.learned += 1;
                                    field = self.planner.distances_to(target);
                                }
                            } else {
                                if self.suspects.len() >= 64 {
                                    self.suspects.remove(0);
                                }
                                self.suspects.push(s);
                            }
                        }
                    }
                }
            }
            still = if obs == pos { still + 1 } else { 0 };
            if still >= 3 {
                return Err(NavError::NotMoving);
            }
            pos = obs;
        }
    }
}
