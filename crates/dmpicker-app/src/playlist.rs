//! djmax.gg 플레이리스트: 주소·응답 읽기, 히스토리와 사본 저장, 작업 스레드에서 받아 오기

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use dmpicker_search::{Diff, Mode};
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// 응답이 도착함, 검색창 창으로 옴 (main.rs의 WM_APP 메시지와 안 겹치게)
pub const READY: u32 = WM_APP + 3;
pub const NOT_FOUND: &str = "볼 수 없는 플레이리스트예요";
const LATER: &str = "잠시 뒤 다시 시도해 주세요";
const FAILED: &str = "플레이리스트를 불러오지 못했어요";
/// 히스토리에 남기는 곡·플레이리스트 수
const RECENT_MAX: usize = 6;

/// 영문·숫자·`_`·`-` 1~24자, 요청 경로와 사본 파일 이름에 그대로 씀
fn valid_id(id: &str) -> bool {
    (1..=24).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

/// 검색어 전체가 djmax.gg 플레이리스트 주소일 때만 그 id, 쿼리·프래그먼트는 요청에 안 보냄
pub fn id(query: &str) -> Option<&str> {
    let mut s = query.trim();
    for scheme in ["https://", "http://"] {
        if s.get(..scheme.len()).is_some_and(|p| p.eq_ignore_ascii_case(scheme)) {
            s = &s[scheme.len()..];
            break;
        }
    }
    let (host, path) = s.split_once('/')?;
    if !host.eq_ignore_ascii_case("djmax.gg") && !host.eq_ignore_ascii_case("www.djmax.gg") {
        return None;
    }
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    let id = path.strip_prefix("playlist/")?;
    valid_id(id).then_some(id)
}

/// 히스토리에서 열 때 입력 칸에 넣는 주소
pub fn url(id: &str) -> String {
    format!("https://djmax.gg/playlist/{id}")
}

/// 항목 하나의 패턴, `title`은 곡 id (응답의 필드 이름)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pattern {
    pub title: u32,
    pub mode: Mode,
    pub diff: Diff,
}

/// 플레이리스트 항목, 없거나 형식이 다른 값은 None
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub pattern: Pattern,
    /// 코멘트, 받은 글 그대로
    pub note: Option<String>,
    /// 효과 이름 (MAX RANDOM 등), 모르는 이름도 글로 남김
    pub effect: Option<String>,
    /// 목표 점수 0~100
    pub target_score: Option<f64>,
}

/// 응답에서 읽은 플레이리스트, 사본 파일도 같은 형식
#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub entries: Vec<Entry>,
}

impl Playlist {
    /// `id`의 응답이나 사본을 읽음, id가 다르거나 이름·항목 배열이 없으면 None
    /// 읽을 수 없는 항목은 건너뛰고 같은 패턴이 또 나오면 먼저 나온 항목만 남김
    pub fn parse(id: &str, bytes: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(bytes).ok()?;
        if !valid_id(id) || v.get("id")?.as_str()? != id {
            return None;
        }
        let mut seen = HashSet::new();
        let entries = v
            .get("entries")?
            .as_array()?
            .iter()
            .filter_map(|e| {
                let (mode, diff) = (e.get("buttonMode")?.as_str()?, e.get("patternType")?.as_str()?);
                let pattern = Pattern {
                    title: u32::try_from(e.get("title")?.as_u64()?).ok()?,
                    mode: Mode::ALL.into_iter().find(|m| m.label() == mode)?,
                    diff: Diff::ALL.into_iter().find(|d| d.label() == diff)?,
                };
                if !seen.insert(pattern) {
                    return None;
                }
                Some(Entry {
                    pattern,
                    note: optional_text(e, "note"),
                    effect: optional_text(e, "effect"),
                    target_score: e.get("targetScore").and_then(Value::as_f64).filter(|n| (0.0..=100.0).contains(n)),
                })
            })
            .collect();
        let name = v.get("name")?.as_str()?.into();
        Some(Self { id: id.into(), name, updated_at: optional_text(&v, "updatedAt").unwrap_or_default(), entries })
    }

    /// 사본 파일 내용, 응답과 같은 필드라 [`Self::parse`]로 다시 읽음
    fn bytes(&self) -> Vec<u8> {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                json!({
                    "title": e.pattern.title,
                    "buttonMode": e.pattern.mode.label(),
                    "patternType": e.pattern.diff.label(),
                    "note": e.note,
                    "effect": e.effect,
                    "targetScore": e.target_score,
                })
            })
            .collect();
        let v = json!({
            "id": self.id,
            "name": self.name,
            "updatedAt": self.updated_at,
            "entryCount": entries.len(),
            "entries": entries,
        });
        serde_json::to_vec(&v).unwrap()
    }
}

/// 문자열이 아니면 없는 값으로 봄
fn optional_text(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_owned)
}

/// 히스토리 한 줄, `recent.txt`에 최신순으로 한 줄씩
/// 곡은 곡 id, 플레이리스트는 `p {id} {곡 id} {모드} {난이도}`
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recent {
    Song(u32),
    /// 플레이리스트와 거기서 마지막으로 고른 패턴
    Playlist { id: String, last: Pattern },
}

impl Recent {
    pub fn playlist_id(&self) -> Option<&str> {
        match self {
            Self::Playlist { id, .. } => Some(id),
            Self::Song(_) => None,
        }
    }

    /// 곡 id만 있는 예전 파일도 그대로 읽힘
    fn parse(line: &str) -> Option<Self> {
        if let Ok(id) = line.trim().parse() {
            return Some(Self::Song(id));
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        let ["p", id, title, mode, diff] = fields.as_slice() else { return None };
        if !valid_id(id) {
            return None;
        }
        let last = Pattern { title: title.parse().ok()?, mode: Mode::from_label(mode)?, diff: Diff::from_label(diff)? };
        Some(Self::Playlist { id: (*id).into(), last })
    }

    fn line(&self) -> String {
        match self {
            Self::Song(id) => format!("{id}\n"),
            Self::Playlist { id, last } => format!("p {id} {} {} {}\n", last.title, last.mode.label(), last.diff.label()),
        }
    }

    /// 같은 곡이나 같은 플레이리스트, 마지막 패턴은 안 봄
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Song(a), Self::Song(b)) => a == b,
            (Self::Playlist { id: a, .. }, Self::Playlist { id: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// 최근 고른 줄과 플레이리스트 사본, 즐겨찾기한 곡
#[derive(Default)]
pub struct Library {
    /// 최신순, 최대 `RECENT_MAX`줄
    pub recent: Vec<Recent>,
    /// 최근에 추가한 곡부터, 개수 제한 없음
    pub favorites: Vec<u32>,
    /// 히스토리에 있는 플레이리스트만, 응답이 안 와도 이걸로 보여 줌
    pub copies: HashMap<String, Playlist>,
    /// 기록·즐겨찾기·플레이리스트 사본을 두는 곳, 없으면 파일에 안 씀
    root: Option<PathBuf>,
}

impl Library {
    /// 못 읽거나 겹치는 줄은 버림, 최근 기록만 `RECENT_MAX`까지, 사본이 없어도 히스토리 줄은 남김
    pub fn load(root: PathBuf) -> Self {
        let text = std::fs::read_to_string(root.join("recent.txt")).unwrap_or_default();
        let favorites = std::fs::read_to_string(root.join("favorites.txt")).unwrap_or_default();
        let mut lib = Self { root: Some(root), ..Self::default() };
        for id in favorites.lines().filter_map(|line| line.trim().parse().ok()) {
            if !lib.is_favorite(id) {
                lib.favorites.push(id);
            }
        }
        for entry in text.lines().filter_map(Recent::parse) {
            if lib.recent.len() == RECENT_MAX {
                break;
            }
            if lib.recent.iter().any(|e| e.same(&entry)) {
                continue;
            }
            if let Some(id) = entry.playlist_id() {
                let raw = lib.path(id).and_then(|path| std::fs::read(path).ok());
                if let Some(data) = raw.and_then(|raw| Playlist::parse(id, &raw)) {
                    lib.copies.insert(id.into(), data);
                }
            }
            lib.recent.push(entry);
        }
        lib
    }

    pub fn is_favorite(&self, id: u32) -> bool {
        self.favorites.contains(&id)
    }

    /// 없으면 맨 앞에 넣고 있으면 뺀 뒤 저장, 바뀐 뒤 즐겨찾기인지 돌려줌
    pub fn toggle_favorite(&mut self, id: u32) -> bool {
        let added = !self.is_favorite(id);
        if added {
            self.favorites.insert(0, id);
        } else {
            self.favorites.retain(|&i| i != id);
        }
        self.save_favorites();
        added
    }

    fn path(&self, id: &str) -> Option<PathBuf> {
        Some(self.root.as_ref()?.join("playlists").join(format!("{id}.json")))
    }

    /// `entry`를 맨 앞으로, 밀려난 플레이리스트의 사본은 지움
    /// `data`는 방금 고른 플레이리스트의 내용, 사본으로 씀
    pub fn remember(&mut self, entry: Recent, data: Option<&Playlist>) {
        self.recent.retain(|e| !e.same(&entry));
        self.recent.insert(0, entry);
        for old in self.recent.split_off(self.recent.len().min(RECENT_MAX)) {
            if let Some(id) = old.playlist_id() {
                self.remove_copy(id);
            }
        }
        if let Some(data) = data {
            self.update(data);
        }
        self.save_recent();
    }

    /// 히스토리에 있는 플레이리스트만 사본으로 남김
    pub fn update(&mut self, data: &Playlist) {
        if !self.recent.iter().any(|e| e.playlist_id() == Some(&data.id)) {
            return;
        }
        self.copies.insert(data.id.clone(), data.clone());
        if let Some(path) = self.path(&data.id) {
            if let Err(e) = crate::keywords::write(&path, &data.bytes()) {
                eprintln!("save playlist: {e}");
            }
        }
    }

    /// 없어진 플레이리스트 (404)는 히스토리와 사본에서 뺌
    pub fn forget(&mut self, id: &str) {
        self.recent.retain(|e| e.playlist_id() != Some(id));
        self.remove_copy(id);
        self.save_recent();
    }

    fn remove_copy(&mut self, id: &str) {
        self.copies.remove(id);
        if let Some(path) = self.path(id) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn save_recent(&self) {
        let Some(root) = &self.root else { return };
        let text: String = self.recent.iter().map(Recent::line).collect();
        let result = std::fs::create_dir_all(root).and_then(|_| std::fs::write(root.join("recent.txt"), text));
        if let Err(e) = result {
            eprintln!("save recent: {e}");
        }
    }

    fn save_favorites(&self) {
        let Some(root) = &self.root else { return };
        let text: String = self.favorites.iter().map(|id| format!("{id}\n")).collect();
        if let Err(e) = crate::keywords::write(&root.join("favorites.txt"), text.as_bytes()) {
            eprintln!("save favorites: {e}");
        }
    }
}

/// 받아 오지 못한 이유, 결과 자리의 안내 문구가 달라짐
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// 404, 지워졌거나 공개하지 않은 플레이리스트
    NotFound,
    /// 429·503, 서버가 바쁨
    Later,
    /// 그 밖의 응답, 연결 실패, 읽을 수 없는 응답
    Failed,
}

impl Failure {
    pub fn note(self) -> &'static str {
        match self {
            Self::NotFound => NOT_FOUND,
            Self::Later => LATER,
            Self::Failed => FAILED,
        }
    }
}

/// 작업 스레드가 보내는 결과
pub struct Reply {
    pub id: String,
    pub result: Result<Playlist, Failure>,
}

/// 플레이리스트를 작업 스레드에서 받아 옴, 응답은 [`Self::take`]로 꺼냄
pub struct Loader {
    /// 응답을 기다리는 id, 그동안 같은 id는 다시 안 보냄
    pending: HashSet<String>,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Default for Loader {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { pending: HashSet::new(), tx, rx }
    }
}

impl Loader {
    /// 응답이 오면 `hwnd`에 [`READY`]를 보냄, 스레드를 못 만들면 실패 응답을 바로 넣음
    pub fn request(&mut self, id: String, hwnd: HWND) {
        if !self.pending.insert(id.clone()) {
            return;
        }
        let (tx, hwnd) = (self.tx.clone(), hwnd.0 as isize);
        let failed_id = id.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            let result = fetch(&id);
            let _ = tx.send(Reply { id, result });
            notify(hwnd);
        });
        if spawned.is_err() {
            let _ = self.tx.send(Reply { id: failed_id, result: Err(Failure::Failed) });
            notify(hwnd);
        }
    }

    pub fn take(&mut self) -> Option<Reply> {
        let reply = self.rx.try_recv().ok()?;
        self.pending.remove(&reply.id);
        Some(reply)
    }
}

/// `HWND`는 스레드로 못 넘겨서 정수로 받음
fn notify(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), READY, WPARAM(0), LPARAM(0));
    }
}

/// 항목 목록을 작업 스레드에서 받아 읽음
fn fetch(id: &str) -> Result<Playlist, Failure> {
    // 요청 경로에 그대로 들어감
    if !valid_id(id) {
        return Err(Failure::Failed);
    }
    let url = format!("https://djmax.gg/api/playlist/{id}/entries");
    let bytes = crate::http::playlist(&url).map_err(|error| match error.status {
        Some(404) => Failure::NotFound,
        Some(429 | 503) => Failure::Later,
        _ => Failure::Failed,
    })?;
    Playlist::parse(id, &bytes).ok_or(Failure::Failed)
}
