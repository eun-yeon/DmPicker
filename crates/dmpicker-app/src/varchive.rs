//! V-Archive 기록 사본과 수동 갱신

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dmpicker_search::{Diff, Mode};
use serde_json::{json, Value};
use windows::Win32::Foundation::{FILETIME, HWND};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const READY: u32 = WM_APP + 6;

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub score: f64,
    pub max_combo: bool,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    MaxCombo,
    Perfect,
}

impl Record {
    pub fn badge(&self) -> Option<Badge> {
        if self.score == 100.0 { Some(Badge::Perfect) } else { self.max_combo.then_some(Badge::MaxCombo) }
    }

    pub fn rate_text(&self) -> String {
        format!("{:.2}%", self.score)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub received_at: u64,
    pub records: BTreeMap<(u32, Diff), Record>,
}

fn records(values: &[Value], known: impl Fn(u32) -> bool) -> Option<BTreeMap<(u32, Diff), Record>> {
    let mut records = BTreeMap::new();
    for value in values {
        let title = u32::try_from(value["title"].as_u64()?).ok()?;
        let Some(diff) = value["pattern"].as_str().and_then(Diff::from_label) else {
            continue;
        };
        if !known(title) {
            continue;
        }
        let score = value["score"].as_f64().filter(|s| valid_rate(*s))?;
        let record = Record {
            score,
            max_combo: value["maxCombo"].as_bool()?,
            updated_at: value["updatedAt"].as_str()?.to_owned(),
        };
        records.insert((title, diff), record);
    }
    Some(records)
}

pub fn parse(raw: &[u8], mode: Mode, known: &HashSet<u32>, received_at: u64) -> Option<Snapshot> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    if !value["success"].as_bool()? || value["button"].as_u64()? != button(mode) as u64 {
        return None;
    }
    let values = value["records"].as_array()?;
    if value["count"].as_u64()? != values.len() as u64 {
        return None;
    }
    Some(Snapshot { received_at, records: records(values, |id| known.contains(&id))? })
}

pub fn button(mode: Mode) -> u8 {
    [4, 5, 6, 8][mode.index()]
}

fn url(nickname: &str, mode: Mode) -> String {
    let mut encoded = String::new();
    for byte in nickname.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    format!("https://v-archive.net/api/v2/archive/{encoded}/button/{}", button(mode))
}

pub fn valid_rate(rate: f64) -> bool {
    rate.is_finite() && (0.0..=100.0).contains(&rate)
}

/// 받는 동안의 기록 줄 상태 글
const LOADING: &str = "기록을 불러오고 있어요";

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub struct Cache {
    pub nickname: String,
    pub modes: [Option<Snapshot>; 4],
    pub loading: bool,
    /// 받기 시작한 때, 불러오는 중 글을 훑는 빛이 여기서 시작함
    pub loading_at: Instant,
    failed: [bool; 4],
    generation: u64,
    writable: bool,
}

impl Default for Cache {
    fn default() -> Self {
        Self { nickname: String::new(), modes: Default::default(), loading: false, loading_at: Instant::now(), failed: [false; 4], generation: 0, writable: true }
    }
}

impl Cache {
    pub fn load(path: &Path, nickname: &str) -> Self {
        let parsed = match std::fs::read(path) {
            Ok(raw) => Self::from_bytes(&raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Self::default()),
            Err(_) => None,
        };
        let writable = parsed.is_some();
        let mut cache = parsed.unwrap_or_default();
        cache.writable = writable;
        cache.set_nickname(nickname);
        cache
    }

    fn from_bytes(raw: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(raw).ok()?;
        if value["version"].as_u64()? != 1 {
            return None;
        }
        let mut cache = Self { nickname: value["nickname"].as_str()?.to_owned(), ..Self::default() };
        let modes = value["modes"].as_object()?;
        for mode in Mode::ALL {
            if let Some(v) = modes.get(mode.label()) {
                cache.modes[mode.index()] = Some(Snapshot {
                    received_at: v["received_at"].as_u64()?,
                    records: records(v["records"].as_array()?, |_| true)?,
                });
            }
        }
        Some(cache)
    }

    fn json(&self) -> Value {
        let mut modes = serde_json::Map::new();
        for mode in Mode::ALL {
            if let Some(snapshot) = &self.modes[mode.index()] {
                let records: Vec<_> = snapshot.records.iter().map(|(&(title, diff), record)| json!({
                    "title": title, "pattern": diff.label(), "score": record.score,
                    "maxCombo": record.max_combo, "updatedAt": record.updated_at,
                })).collect();
                modes.insert(mode.label().to_owned(), json!({ "received_at": snapshot.received_at, "records": records }));
            }
        }
        json!({ "version": 1, "nickname": self.nickname, "modes": modes })
    }

    pub fn save(&self, path: &Path) -> Result<(), &'static str> {
        // 읽지 못한 사본은 이번 실행 동안 덮어쓰지 않음
        if !self.writable {
            return Err("기록 파일을 읽지 못해서 저장할 수 없어요");
        }
        crate::sys::save_file(path, self.json().to_string().as_bytes()).map_err(|_| "기록을 저장하지 못했어요")
    }

    pub fn set_nickname(&mut self, nickname: &str) {
        let nickname = nickname.trim();
        if self.nickname == nickname {
            return;
        }
        self.nickname = nickname.to_owned();
        self.modes = Default::default();
        self.failed = [false; 4];
        self.loading = false;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn connected(&self) -> bool {
        !self.nickname.is_empty()
    }

    pub fn known(&self, mode: Mode) -> bool {
        self.connected() && self.modes[mode.index()].is_some()
    }

    pub fn record(&self, title: u32, mode: Mode, diff: Diff) -> Option<&Record> {
        if !self.connected() {
            return None;
        }
        self.modes[mode.index()].as_ref()?.records.get(&(title, diff))
    }

    pub fn apply(&mut self, reply: Reply) -> bool {
        if reply.generation != self.generation || reply.nickname != self.nickname {
            return false;
        }
        self.loading = false;
        for (i, result) in reply.modes.into_iter().enumerate() {
            self.failed[i] = result.is_none();
            if let Some(snapshot) = result {
                self.modes[i] = Some(snapshot);
            }
        }
        true
    }

    pub fn status(&self, mode: Mode, now: u64) -> String {
        if !self.connected() {
            return "닉네임을 넣으면 기록 필터를 쓸 수 있어요".into();
        }
        if self.loading {
            return LOADING.into();
        }
        let snapshot = self.modes[mode.index()].as_ref();
        if self.failed[mode.index()] {
            return snapshot.map_or_else(|| "기록을 불러오지 못했어요".into(), |s| format!("갱신 실패 · {} 기록 사용 중", local_clock(s.received_at)));
        }
        let Some(snapshot) = snapshot else {
            return "새로고침으로 기록을 불러와 주세요".into();
        };
        let elapsed = now.saturating_sub(snapshot.received_at);
        let ago = match elapsed {
            0..60 => "방금 전".into(),
            60..3600 => format!("{}분 전", elapsed / 60),
            3600..86400 => format!("{}시간 전", elapsed / 3600),
            _ => format!("{}일 전", elapsed / 86400),
        };
        // 닉네임은 옆 버튼에 있어서 안 붙임
        format!("{} 기록 {}개 · {ago}", mode.label(), grouped(snapshot.records.len()))
    }
}

fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut text = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            text.push(',');
        }
        text.push(c);
    }
    text
}

fn local_clock(seconds: u64) -> String {
    let Some(ticks) = seconds.checked_add(11_644_473_600).and_then(|s| s.checked_mul(10_000_000)) else {
        return "--:--".into();
    };
    let stamp = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let Some(local) = crate::sys::local_time(&stamp) else {
        return "--:--".into();
    };
    format!("{:02}:{:02}", local.wHour, local.wMinute)
}

pub struct Reply {
    generation: u64,
    nickname: String,
    modes: [Option<Snapshot>; 4],
}

struct Job {
    generation: u64,
    nickname: String,
    known: HashSet<u32>,
    hwnd: isize,
}

pub struct Loader {
    active: Option<Arc<AtomicBool>>,
    queued: Option<Job>,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Default for Loader {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { active: None, queued: None, tx, rx }
    }
}

impl Loader {
    pub fn request(&mut self, cache: &mut Cache, known: HashSet<u32>, hwnd: HWND) {
        if cache.loading {
            return;
        }
        if let Some(cancel) = &self.active {
            cancel.store(true, Ordering::Release);
        }
        self.queued = None;
        if !cache.connected() {
            return;
        }
        cache.loading = true;
        cache.loading_at = Instant::now();
        let job = Job { generation: cache.generation, nickname: cache.nickname.clone(), known, hwnd: hwnd.0 as isize };
        if self.active.is_some() {
            self.queued = Some(job);
        } else {
            self.start(job);
        }
    }

    fn start(&mut self, job: Job) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.active = Some(cancel.clone());
        let (tx, hwnd) = (self.tx.clone(), job.hwnd);
        let failed = Reply { generation: job.generation, nickname: job.nickname.clone(), modes: Default::default() };
        if std::thread::Builder::new().spawn(move || {
            let reply = receive(job, |url| {
                let got = crate::http::varchive(url, &cancel);
                got.ok()
            });
            let _ = tx.send(reply);
            crate::sys::notify(hwnd, READY);
        }).is_err() {
            let _ = self.tx.send(failed);
            crate::sys::notify(hwnd, READY);
        }
    }

    pub fn take(&mut self) -> Option<Reply> {
        let reply = self.rx.try_recv().ok()?;
        self.active = None;
        if let Some(job) = self.queued.take() {
            self.start(job);
        }
        Some(reply)
    }
}

fn receive(job: Job, mut get: impl FnMut(&str) -> Option<Vec<u8>>) -> Reply {
    let modes = Mode::ALL.map(|mode| {
        let raw = get(&url(&job.nickname, mode))?;
        parse(&raw, mode, &job.known, now())
    });
    Reply { generation: job.generation, nickname: job.nickname, modes }
}
