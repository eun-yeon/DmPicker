//! 곡 데이터 선택과 백그라운드 갱신

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use dmpicker_engine::{features::FEATURE_VERSION, RefSet};
use dmpicker_update as feed;
use feed::exe::{ExeRelease, Version};
use feed::notify::{Notice, Snapshot, Summary};
use feed::state::{self, CheckAction, CheckError, UpdateState};
use feed::store::{self, Generation, Pointer, Staging};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
};

use crate::{catalog::Catalog, http};

const BASELINE_FILE: &str = "notified-baseline.json";
const CHECKED_TEXT: &str = "곡 데이터 확인됨";
const SAVE_FAILED_TEXT: &str = "곡 데이터를 저장하지 못했어요";
/// 예전 버전이 남긴 특징 캐시, 이제는 세대 폴더의 features.bin을 씀
const OLD_REFS_CACHE: &str = "refs.cache";

pub struct Active {
    pub generation: Option<Arc<Generation>>,
    pub snapshot: Snapshot,
}

impl Active {
    /// 검색할 곡이 있음(로컬 동봉 데이터 포함), 받은 세대 여부는 `generation`으로 구분
    pub fn has_data(&self) -> bool {
        !self.snapshot.songs.is_empty()
    }

    fn tag(&self) -> Option<&str> {
        self.generation
            .as_ref()
            .map(|g| g.manifest.release_tag.as_str())
    }

    /// 받아 둔 세대의 폴더 이름
    fn name(&self) -> Option<String> {
        self.generation.as_ref().and_then(|g| {
            let name = store::generation_name(&g.manifest.release_tag);
            (g.path.file_name().and_then(|n| n.to_str()) == Some(name.as_str())).then_some(name)
        })
    }
}

pub struct Startup {
    pub catalog: Catalog,
    pub refs: Option<Arc<RefSet>>,
    pub active: Arc<Active>,
    state: UpdateState,
    baseline: Snapshot,
}

/// 특징 계산 전에 곡 목록을 읽어 화면부터 준비
pub fn preview(root: &Path, bundled: &Path) -> Result<Startup, String> {
    load_startup(root, bundled, false)
}

pub type InitialLoad = Result<Startup, String>;

/// 시작 준비는 별도 스레드에서 마침
pub fn start_initial(root: PathBuf, bundled: PathBuf) -> JoinHandle<InitialLoad> {
    std::thread::spawn(move || load_startup(&root, &bundled, true))
}

/// `prepare`면 특징까지 준비한 뒤 pointer를 고치고 남은 세대 정리
/// 아니면 화면을 먼저 띄우려고 곡 목록만 읽음
fn load_startup(root: &Path, bundled: &Path, prepare: bool) -> Result<Startup, String> {
    let data = root.join("data");
    if prepare {
        if let Err(error) = std::fs::create_dir_all(&data) {
            eprintln!("data directory: {error}");
        }
    }
    let state_path = root.join(feed::state::STATE_FILE);
    let mut state = UpdateState::load(&state_path).unwrap_or_default();
    let blocked = state.blocked_generation().map(str::to_owned);
    #[allow(unused_mut)]
    let mut loaded = store::choose_generation(&data, blocked.as_deref(), |candidate| {
        let marked = state.begin_loading(&state_path, &candidate.name)?;
        let result = (|| {
            let generation = candidate.load()?;
            let refs = if prepare {
                Some(initial_refs(&generation)?)
            } else {
                None
            };
            let catalog = feed_catalog(&generation)?;
            let snapshot = Snapshot::from_data(&generation.core, &generation.dynamic);
            Ok((catalog, refs, Some(Arc::new(generation)), snapshot))
        })();
        if prepare && marked {
            state.finish_loading(&state_path);
        } else if !prepare {
            // 미리 보기에서 꺼지면 다음 실행은 이 세대를 뒤로 미룸
            // 다 읽었으면 표식을 원래대로 돌려 판단은 시작 준비에 맡김
            state.loading = blocked.clone();
            if let Err(error) = state.save(&state_path) {
                eprintln!("data loading marker: {error}");
            }
        }
        result
    });
    let (catalog, refs, generation, snapshot) =
        loaded.unwrap_or_else(|| (Catalog::empty(bundled), None, None, Snapshot::default()));
    if generation.is_some() {
        let _ = std::fs::remove_file(root.join(OLD_REFS_CACHE));
    }
    if let Some(generation) = generation
        .as_ref()
        .filter(|g| prepare && g.path.parent() == Some(data.as_path()))
    {
        let name = store::generation_name(&generation.manifest.release_tag);
        let previous = store::read_pointers(&data)
            .into_iter()
            .flat_map(|p| std::iter::once(p.active).chain(p.previous))
            .find(|other| {
                other != &name
                    && blocked.as_ref() != Some(other)
                    && store::read_generation(&data.join(other)).is_ok()
            });
        let pointer = Pointer::new(&name, previous.as_deref()).map_err(|e| e.to_string())?;
        match store::write_pointer(&data, &pointer) {
            Ok(permit) => {
                if state.applied_tag.as_deref() != Some(&generation.manifest.release_tag) {
                    state
                        .record_data_applied(&generation.manifest.release_tag)
                        .map_err(|e| e.to_string())?;
                } else if state.loading.as_deref() == Some(&name) {
                    state.finish_loading(&state_path);
                }
                if let Err(error) = state.save(&state_path) {
                    eprintln!("data state: {error}");
                }
                if let Err(error) = store::cleanup(&permit) {
                    eprintln!("data cleanup: {error}");
                }
            }
            Err(error) => eprintln!("data pointer: {error}"),
        }
    }
    let baseline = Snapshot::load(&root.join(BASELINE_FILE)).unwrap_or_else(|_| {
        if prepare && !snapshot.songs.is_empty() {
            let _ = snapshot.save(&root.join(BASELINE_FILE));
        }
        snapshot.clone()
    });
    Ok(Startup {
        catalog,
        refs,
        active: Arc::new(Active {
            generation,
            snapshot,
        }),
        state,
        baseline,
    })
}

fn prepare_refs(
    generation: &Generation,
    active: Option<&Active>,
    previous: Option<&Arc<RefSet>>,
    cancel: &AtomicBool,
    parallel: bool,
) -> feed::Result<Arc<RefSet>> {
    feed::ensure_running(cancel)?;
    if let Some(refs) = cached_refs(generation) {
        return Ok(refs);
    }
    let jackets = jacket_list(&generation.core);
    let old: Vec<_> = active
        .map(|a| {
            a.snapshot
                .songs
                .iter()
                .map(|(&id, hash)| (id, hash.clone()))
                .collect()
        })
        .unwrap_or_default();
    // 재킷이 그대로면 지금 RefSet을 그대로 씀, 레벨만 바뀐 갱신은 다시 계산 안 함
    if jackets == old {
        if let Some(previous) = previous {
            save_refs(generation, previous);
            return Ok(previous.clone());
        }
    }
    // 앱 시작 확인은 특징이 빨리 준비돼야 해서 여러 스레드로 새로 계산
    // 검색창에서 시작한 갱신은 한 스레드로 바뀐 재킷만 계산
    if parallel {
        let refs = compute_refs(generation, &jackets)?;
        feed::ensure_running(cancel)?;
        return Ok(refs);
    }
    let reuse = previous.map(|refs| (refs.as_ref(), old.as_slice()));
    let refs = RefSet::update_jackets(&jackets, reuse, cancel, |id| {
        decode_jacket(&generation.path, id)
    })?;
    save_refs(generation, &refs);
    Ok(Arc::new(refs))
}

fn save_refs(generation: &Generation, refs: &RefSet) {
    let result = refs
        .to_cache_bytes(&generation.core.feature_signature(FEATURE_VERSION))
        .and_then(|bytes| std::fs::write(generation.path.join(store::FEATURES_FILE), bytes));
    if let Err(error) = result {
        eprintln!("data features: {error}");
    }
}

fn cached_refs(generation: &Generation) -> Option<Arc<RefSet>> {
    let signature = generation.core.feature_signature(FEATURE_VERSION);
    let bytes = store::read_limited(
        &generation.path.join(store::FEATURES_FILE),
        dmpicker_engine::features::MAX_CACHE_BYTES,
    )
    .ok()?;
    RefSet::from_cache_bytes(&bytes, &signature)
        .ok()
        .map(Arc::new)
}

fn initial_refs(generation: &Generation) -> feed::Result<Arc<RefSet>> {
    if let Some(refs) = cached_refs(generation) {
        return Ok(refs);
    }
    compute_refs(generation, &jacket_list(&generation.core))
}

/// 캐시 없이 모든 재킷을 여러 스레드로 계산하고 캐시를 남김
fn compute_refs(generation: &Generation, jackets: &[(u32, String)]) -> feed::Result<Arc<RefSet>> {
    let refs = RefSet::load_jackets(jackets, |id| decode_jacket(&generation.path, id))?;
    save_refs(generation, &refs);
    Ok(Arc::new(refs))
}

fn jacket_list(core: &feed::Core) -> Vec<(u32, String)> {
    core.songs.iter().map(|s| (s.id, s.jacket.clone())).collect()
}

fn decode_jacket(path: &Path, id: u32) -> std::io::Result<dmpicker_engine::img::Rgb> {
    let bytes = store::read_jacket(path, id, None).map_err(std::io::Error::other)?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP)
        .map_err(std::io::Error::other)?
        .into_rgb8();
    if image.dimensions() != (128, 128) {
        return Err(std::io::Error::other("invalid jacket dimensions"));
    }
    dmpicker_engine::img::Rgb::from_raw(128, 128, image.into_raw())
        .ok_or_else(|| std::io::Error::other("invalid jacket pixels"))
}

fn feed_catalog(generation: &Generation) -> feed::Result<Catalog> {
    Catalog::from_feed(&generation.core, &generation.dynamic, &generation.path)
        .map_err(feed::Error::invalid)
}

pub struct Ready {
    pub catalog: Catalog,
    pub active: Arc<Active>,
    pub refs: Arc<RefSet>,
    pub titles: Arc<HashMap<u32, String>>,
    pub summary: Summary,
}

impl Ready {
    pub fn changed_jackets(&self, old: &Active) -> HashSet<u32> {
        old.snapshot
            .songs
            .keys()
            .chain(self.active.snapshot.songs.keys())
            .copied()
            .filter(|id| old.snapshot.songs.get(id) != self.active.snapshot.songs.get(id))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    Preparing,
    Ready,
    Applying,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckOrigin {
    Startup,
    Search,
}

struct CheckRequest {
    id: u64,
    state: UpdateState,
    active: Arc<Active>,
    refs: Option<Arc<RefSet>>,
    check_data: bool,
    check_exe: bool,
    origin: CheckOrigin,
}

enum Command {
    Check(CheckRequest),
    Apply {
        id: u64,
        state: UpdateState,
        active: Arc<Active>,
        previous: Option<String>,
    },
    Baseline(Snapshot),
    /// 화면에 나간 새 버전·형식 안내
    Noticed {
        version: Option<Version>,
        schema: Option<u64>,
    },
    Stop,
}

enum Event {
    Preparing(u64),
    Checked {
        id: u64,
        state: UpdateState,
        result: Result<Option<Box<Ready>>, CheckFailure>,
        checked_data: bool,
    },
    Applied {
        id: u64,
        state: UpdateState,
        result: feed::Result<()>,
    },
}

/// 받아 둔 곡 데이터가 없을 때의 상태
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataNotice {
    Loading,
    Failed,
    /// 받은 데이터 형식을 이 버전이 못 읽음
    Outdated,
}

/// 새 데이터 형식을 못 읽을 때의 새 버전 안내
pub const OUTDATED_TEXT: &str = "새 곡 데이터를 읽으려면 새 버전이 필요해요";

/// 이번 검색창에서 표시할 새 버전 안내, 형식 안내가 있으면 그 설명
pub struct ExeNotice {
    version: Option<Version>,
    schema: Option<u64>,
    pub text: String,
}

fn current_version() -> Option<Version> {
    Version::parse(env!("CARGO_PKG_VERSION")).ok()
}

fn exe_release(state: &UpdateState) -> Option<ExeRelease> {
    let release = ExeRelease::from_tag(state.latest_version.as_deref()?).ok()?;
    (release.version() > current_version()?).then_some(release)
}

/// 버전 뒤 조사는 끝 숫자를 읽은 소리로, 2·4·5·9로 끝나면 "가"
fn version_text(version: Version) -> String {
    let particle = if matches!(version.patch % 10, 2 | 4 | 5 | 9) {
        "가"
    } else {
        "이"
    };
    format!("새 버전 {version}{particle} 나왔어요")
}

fn exe_notice(state: &UpdateState, schema: Option<u64>) -> Option<ExeNotice> {
    let version = exe_release(state)
        .map(|release| release.version())
        .filter(|&version| state.needs_exe_notice(version));
    let text = match schema {
        Some(_) => OUTDATED_TEXT.to_owned(),
        None => version_text(version?),
    };
    Some(ExeNotice {
        version,
        schema,
        text,
    })
}

fn exe_status(state: &UpdateState) -> Option<String> {
    exe_release(state).map(|release| version_text(release.version()))
}

/// 검증한 태그와 고정 경로로 만든 주소, 새 버전을 모르면 최신 릴리즈 주소
fn release_page(state: &UpdateState) -> String {
    exe_release(state).map_or_else(|| feed::EXE_LATEST_URL.to_owned(), |r| r.page_url())
}

fn open_url(url: &str) -> std::io::Result<()> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let url = HSTRING::from(url);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &url,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err(std::io::Error::other("could not open release page"));
    }
    Ok(())
}

#[derive(Debug)]
struct CheckFailure {
    stage: &'static str,
    status: Option<u16>,
    retry_after: Option<String>,
    schema: Option<u64>,
    cancelled: bool,
}

impl From<feed::Error> for CheckFailure {
    fn from(error: feed::Error) -> Self {
        let schema = match &error {
            feed::Error::UnsupportedSchema(value) => Some(*value),
            _ => None,
        };
        let cancelled =
            matches!(&error, feed::Error::Io(e) if e.kind() == std::io::ErrorKind::Interrupted);
        if !cancelled {
            eprintln!("data check: {error}");
        }
        // 진단 기록에서 디스크 읽기·쓰기 실패를 데이터 검증 실패와 구분
        // 압축이 깨지면 나는 InvalidData·InvalidInput·UnexpectedEof는 검증 쪽
        let storage = matches!(&error, feed::Error::Io(e) if !matches!(
            e.kind(),
            std::io::ErrorKind::InvalidData
                | std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::UnexpectedEof
        ));
        let stage = if storage { "storage" } else { "validation" };
        Self {
            stage,
            status: None,
            retry_after: None,
            schema,
            cancelled,
        }
    }
}

impl CheckFailure {
    fn http(stage: &'static str, error: http::Failure) -> Self {
        if !error.cancelled() {
            eprintln!("data {stage}: {error:?}");
        }
        Self {
            stage,
            status: error.status,
            cancelled: error.cancelled(),
            retry_after: error.retry_after,
            schema: None,
        }
    }
}

pub struct Updater {
    pub active: Arc<Active>,
    status_text: String,
    exe_status_text: Option<String>,
    pub exe_notice: Option<ExeNotice>,
    pub data_notice: Option<DataNotice>,
    phase: Phase,
    /// 아직 알리지 않은 못 읽는 데이터 형식
    schema_notice: Option<u64>,
    /// 마지막 데이터 확인이 형식 때문에 실패함
    outdated: bool,
    state: UpdateState,
    baseline: Snapshot,
    id: u64,
    ready: Option<Ready>,
    initializing: bool,
    startup_pending: bool,
    origin: CheckOrigin,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Updater {
    pub fn new(root: PathBuf, bundled: PathBuf, startup: &Startup) -> Self {
        let (commands, receive) = mpsc::channel();
        let (send, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || worker(root, bundled, receive, send, flag));
        Self {
            active: startup.active.clone(),
            status_text: data_status(&startup.active).unwrap_or_default(),
            exe_status_text: exe_status(&startup.state),
            exe_notice: None,
            data_notice: (!startup.active.has_data()).then_some(DataNotice::Loading),
            phase: Phase::Idle,
            schema_notice: None,
            outdated: false,
            state: startup.state.clone(),
            baseline: startup.baseline.clone(),
            id: 0,
            ready: None,
            initializing: startup.active.has_data(),
            startup_pending: true,
            origin: CheckOrigin::Startup,
            commands,
            events,
            cancel,
            worker: Some(worker),
        }
    }

    /// 창과 트레이 준비 뒤 한 번, 초기 특징 계산 중이면 끝날 때까지 보류
    pub fn started(&mut self, busy: bool, refs: Option<Arc<RefSet>>) {
        if !self.startup_pending || self.initializing || busy || self.phase != Phase::Idle {
            return;
        }
        self.startup_pending = false;
        self.begin(CheckOrigin::Startup, false, refs);
    }

    /// 선곡 화면을 확인하고 검색창이 실제로 열렸을 때 확인 시작
    /// 초기 로딩 중엔 새 버전 안내도 안 만듦, 뒤늦게 끝난 로딩이 알림 기록을 덮어쓰지 않게
    pub fn opened(&mut self, busy: bool, refs: Option<Arc<RefSet>>) {
        if self.initializing {
            return;
        }
        self.exe_notice = exe_notice(&self.state, self.schema_notice);
        self.begin(CheckOrigin::Search, busy, refs);
    }

    /// 안내 카드의 다시 시도, 기한은 무시하되 Retry-After는 지킴
    pub fn retry(&mut self, busy: bool, refs: Option<Arc<RefSet>>) {
        self.begin(CheckOrigin::Search, busy, refs);
    }

    fn begin(&mut self, origin: CheckOrigin, busy: bool, refs: Option<Arc<RefSet>>) {
        if self.initializing || self.phase != Phase::Idle {
            return;
        }
        let action = match origin {
            CheckOrigin::Startup => state::startup(&self.state, now(), busy),
            CheckOrigin::Search => {
                state::due(&self.state, now(), busy, self.active.generation.is_none())
            }
        };
        if let CheckAction::Start {
            check_data,
            check_exe,
        } = action
        {
            self.cancel.store(false, Ordering::Relaxed);
            self.id = self.id.wrapping_add(1);
            self.phase = Phase::Checking;
            self.origin = origin;
            if !self.active.has_data() {
                self.data_notice = Some(DataNotice::Loading);
            }
            if self
                .commands
                .send(Command::Check(CheckRequest {
                    id: self.id,
                    state: self.state.clone(),
                    active: self.active.clone(),
                    refs,
                    check_data,
                    check_exe,
                    origin,
                }))
                .is_err()
            {
                self.end_check();
                self.refresh_data_notice();
            }
        }
    }

    fn end_check(&mut self) {
        self.phase = Phase::Idle;
    }

    /// 받은 세대가 있으면 검색창에서 시작한 받기와 준비만 중단
    pub fn closed(&mut self) {
        if self.origin == CheckOrigin::Search
            && self.active.generation.is_some()
            && matches!(self.phase, Phase::Checking | Phase::Preparing)
        {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// 이번 열기의 안내가 화면에 나간 뒤 호출, 그 버전과 형식을 기록해 다음 열기부터 안 보이게
    /// 이번 열기 동안은 안내를 그대로 둠
    pub fn exe_notice_shown(&mut self) {
        let Some(notice) = &self.exe_notice else {
            return;
        };
        let version = notice.version.filter(|&v| self.state.needs_exe_notice(v));
        let schema = notice.schema.filter(|&s| self.state.needs_schema_notice(s));
        if let Some(version) = version {
            self.state.record_exe_notice(version);
        }
        if let Some(schema) = schema {
            self.state.record_schema_notice(schema);
            if self.schema_notice == Some(schema) {
                self.schema_notice = None;
            }
        }
        if version.is_some() || schema.is_some() {
            let _ = self.commands.send(Command::Noticed { version, schema });
        }
    }

    /// 새 데이터를 읽었으면 형식 문제가 풀림, 이번 열기의 형식 안내도 뺌
    /// 새 버전 안내만 있었으면 그대로 둠
    fn schema_resolved(&mut self) {
        self.schema_notice = None;
        self.outdated = false;
        if self.exe_notice.as_ref().is_some_and(|n| n.schema.is_some()) {
            self.exe_notice = None;
        }
    }

    /// 안내 카드의 릴리즈 페이지 버튼
    pub fn open_page(&self) -> std::io::Result<()> {
        open_url(&release_page(&self.state))
    }

    /// 설정 시트 머리 줄 문구, 새 버전이 있으면 그 안내 (true면 조금 밝게)
    pub fn status_line(&self) -> Option<(String, bool)> {
        match &self.exe_status_text {
            Some(text) => Some((text.clone(), true)),
            None => (!self.status_text.is_empty()).then(|| (self.status_text.clone(), false)),
        }
    }

    /// 알림 기록은 이 스레드에서만 바꿈, worker가 돌려준 상태로 되돌리지 않음
    fn accept_state(&mut self, mut state: UpdateState) {
        state.notified_version = state.notified_version.max(self.state.notified_version);
        state.notified_schema = self.state.notified_schema;
        self.exe_status_text = exe_status(&state);
        self.state = state;
    }

    fn refresh_data_notice(&mut self) {
        self.data_notice = if self.active.has_data() {
            None
        } else if self.startup_pending
            || matches!(
                self.phase,
                Phase::Checking | Phase::Preparing | Phase::Ready
            )
        {
            Some(DataNotice::Loading)
        } else if self.outdated {
            Some(DataNotice::Outdated)
        } else {
            Some(DataNotice::Failed)
        };
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Preparing(id) if id == self.id => self.phase = Phase::Preparing,
                Event::Checked {
                    id,
                    state,
                    result,
                    checked_data,
                } if id == self.id => {
                    self.accept_state(state);
                    match result {
                        Ok(Some(ready)) => {
                            self.ready = Some(*ready);
                            self.phase = Phase::Ready;
                        }
                        Err(error) => {
                            if let Some(schema) = error.schema {
                                if self.state.needs_schema_notice(schema) {
                                    self.schema_notice = Some(schema);
                                }
                            }
                            if !error.cancelled {
                                self.outdated = error.schema.is_some();
                                self.status_text = failure_text(&self.state);
                            }
                            self.end_check();
                        }
                        Ok(None) => {
                            if checked_data {
                                self.status_text =
                                    data_status(&self.active).unwrap_or_else(|| CHECKED_TEXT.into());
                                self.schema_resolved();
                            }
                            self.end_check();
                        }
                    }
                }
                Event::Applied { id, state, result } if id == self.id => {
                    self.accept_state(state);
                    if result.is_err() {
                        self.status_text = SAVE_FAILED_TEXT.into();
                    }
                    self.end_check();
                }
                _ => {}
            }
        }
        self.refresh_data_notice();
    }

    pub fn take_ready(&mut self) -> Option<Ready> {
        self.ready.take()
    }

    pub fn initial_loaded(&mut self, startup: Startup) -> Option<Arc<RefSet>> {
        let refs = startup.refs.unwrap_or_else(|| Arc::new(RefSet::new()));
        let unchanged = match (&self.active.generation, &startup.active.generation) {
            (Some(old), Some(new)) => old.manifest_hash == new.manifest_hash,
            (None, None) => self.active.snapshot == startup.active.snapshot,
            _ => false,
        };
        let titles = startup.catalog.titles();
        self.state = startup.state;
        self.exe_status_text = exe_status(&self.state);
        self.baseline = startup.baseline;
        if unchanged {
            self.active = startup.active;
            self.initializing = false;
            return Some(refs);
        }
        self.ready = Some(Ready {
            catalog: startup.catalog,
            active: startup.active,
            refs,
            titles,
            summary: Summary::default(),
        });
        self.phase = Phase::Ready;
        None
    }

    pub fn initial_failed(&mut self) {
        self.initializing = false;
        self.status_text = "곡 데이터를 준비하지 못했어요".into();
    }

    pub fn keep_ready(&mut self, ready: Ready) {
        self.ready = Some(ready);
    }

    /// 화면에서 교체한 뒤 pointer와 적용 기록은 worker에서 저장
    pub fn applied(&mut self, active: Arc<Active>, summary: &Summary) {
        let previous = self.active.name();
        if let Some(generation) = &active.generation {
            self.status_text = success_text(&generation.manifest.published_at, summary);
        }
        self.active = active.clone();
        if self.baseline.songs.is_empty() {
            self.baseline = active.snapshot.clone();
            let _ = self.commands.send(Command::Baseline(self.baseline.clone()));
        }
        self.data_notice = None;
        self.schema_resolved();
        // 시작 준비가 pointer와 적용 기록을 이미 저장함
        if self.initializing {
            self.initializing = false;
            self.phase = Phase::Idle;
            self.refresh_data_notice();
            return;
        }
        self.phase = Phase::Applying;
        if self
            .commands
            .send(Command::Apply {
                id: self.id,
                state: self.state.clone(),
                active,
                previous,
            })
            .is_err()
        {
            self.end_check();
            self.status_text = SAVE_FAILED_TEXT.into();
        }
    }

    pub fn notice(&self, recognized: bool) -> Option<String> {
        notice_text(&self.baseline.changes_to(&self.active.snapshot), recognized)
    }

    pub fn notice_shown(&mut self) {
        self.baseline = self.active.snapshot.clone();
        let _ = self.commands.send(Command::Baseline(self.baseline.clone()));
    }

    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker(
    root: PathBuf,
    bundled: PathBuf,
    receive: mpsc::Receiver<Command>,
    send: mpsc::Sender<Event>,
    cancel: Arc<AtomicBool>,
) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
    let state_path = root.join(feed::state::STATE_FILE);
    if let Err(error) = std::fs::create_dir_all(root.join("data")) {
        eprintln!("data directory: {error}");
    }
    let source = Source::new();
    while let Ok(command) = receive.recv() {
        match command {
            Command::Stop => break,
            Command::Baseline(snapshot) => {
                if let Err(error) = snapshot.save(&root.join(BASELINE_FILE)) {
                    eprintln!("data notice: {error}");
                }
            }
            Command::Noticed { version, schema } => {
                let result = UpdateState::load(&state_path).and_then(|mut state| {
                    if let Some(version) = version {
                        state.record_exe_notice(version);
                    }
                    if let Some(schema) = schema {
                        state.record_schema_notice(schema);
                    }
                    state.save(&state_path)
                });
                if let Err(error) = result {
                    eprintln!("update notice: {error}");
                }
            }
            Command::Apply {
                id,
                mut state,
                active,
                previous,
            } => {
                let result = (|| {
                    let generation = active
                        .generation
                        .as_ref()
                        .ok_or_else(|| feed::Error::invalid("missing generation"))?;
                    let name = store::generation_name(&generation.manifest.release_tag);
                    let pointer = Pointer::new(&name, previous.as_deref())?;
                    let permit = store::write_pointer(&root.join("data"), &pointer)?;
                    state.record_data_applied(&generation.manifest.release_tag)?;
                    if let Err(error) = state.save(&state_path) {
                        eprintln!("data state: {error}");
                    }
                    let _ = std::fs::remove_file(root.join(OLD_REFS_CACHE));
                    if let Err(error) = store::cleanup(&permit) {
                        eprintln!("data cleanup: {error}");
                    }
                    Ok(())
                })();
                let _ = send.send(Event::Applied { id, state, result });
            }
            Command::Check(request) => {
                let event = run_check(&source, &root, &bundled, &cancel, &send, request);
                let _ = send.send(event);
            }
        }
    }
}

/// exe와 데이터를 확인하고 확인 결과를 상태 파일에 남김
fn run_check(
    source: &Source,
    root: &Path,
    bundled: &Path,
    cancel: &AtomicBool,
    events: &mpsc::Sender<Event>,
    request: CheckRequest,
) -> Event {
    let CheckRequest {
        id,
        mut state,
        active,
        refs,
        check_data: wants_data,
        check_exe: wants_exe,
        origin,
    } = request;
    let state_path = root.join(feed::state::STATE_FILE);
    if wants_exe && !cancel.load(Ordering::Relaxed) {
        check_exe_version(&exe_latest_url(), &state_path, &mut state, cancel);
    }
    let result = if wants_data {
        if let Err(error) = state.save(&state_path) {
            eprintln!("data state: {error}");
        }
        let job = CheckJob {
            state_path: &state_path,
            id,
            cancel,
            events,
            refs: refs.as_ref(),
            bundled,
            parallel: origin == CheckOrigin::Startup,
        };
        check_data(source, &root.join("data"), &job, &mut state, &active)
    } else {
        Ok(None)
    };
    if wants_data {
        match &result {
            Ok(_) => state.record_success(now()),
            Err(error) if !error.cancelled => state.record_failure(
                now(),
                CheckError {
                    stage: error.stage.into(),
                    http_status: error.status,
                },
                error.retry_after.as_deref(),
            ),
            Err(_) => {}
        }
    }
    if let Err(error) = state.save(&state_path) {
        eprintln!("data state: {error}");
    }
    Event::Checked {
        id,
        state,
        result: result.map(|ready| ready.map(Box::new)),
        checked_data: wants_data,
    }
}

fn exe_latest_url() -> String {
    feed::EXE_LATEST_URL.into()
}

/// 실패도 하루에 한 번만 확인하고 데이터 확인 결과와 분리
fn check_exe_version(url: &str, state_path: &Path, state: &mut UpdateState, cancel: &AtomicBool) {
    let result = http::first_redirect(url, feed::MANIFEST_TIMEOUT_SECS, cancel);
    if result.as_ref().is_err_and(http::Failure::cancelled) {
        return;
    }
    match result {
        Ok(location) => {
            if let (Ok(release), Some(current)) =
                (feed::exe::parse_exe_location(&location), current_version())
            {
                let _ = state.record_exe_available(release.tag(), current);
            }
        }
        // 최신 릴리즈가 없어지면 알던 새 버전 안내도 내림
        Err(error) if error.status == Some(404) => state.latest_version = None,
        Err(_) => {}
    }
    state.record_exe_check(now());
    let _ = state.save(state_path);
}

struct Source {
    latest: String,
    base: String,
}

impl Source {
    fn new() -> Self {
        Self {
            latest: feed::DATA_LATEST_URL.into(),
            base: feed::DATA_RELEASE_BASE.into(),
        }
    }

    fn tag(&self, location: &str) -> feed::Result<String> {
        feed::model::data_location(location)
    }

    /// 파일 이름은 호출부의 고정 이름만 들어옴
    fn asset(&self, tag: &str, file: &str) -> feed::Result<String> {
        feed::model::validate_release_tag(tag)?;
        Ok(format!("{}{tag}/{file}", self.base))
    }
}

/// 데이터 확인 한 번에 같이 쓰는 값
struct CheckJob<'a> {
    state_path: &'a Path,
    id: u64,
    cancel: &'a AtomicBool,
    events: &'a mpsc::Sender<Event>,
    refs: Option<&'a Arc<RefSet>>,
    bundled: &'a Path,
    parallel: bool,
}

/// 동봉 재킷이 모두 맞으면 복사, 하나라도 없거나 다르면 zip을 받음
fn copy_bundled_jackets(
    staging: &Staging,
    bundled: &Path,
    core: &feed::Core,
    cancel: &AtomicBool,
) -> feed::Result<bool> {
    let mut total = 0usize;
    for song in &core.songs {
        feed::ensure_running(cancel)?;
        let Ok(bytes) = store::read_jacket(bundled, song.id, Some(&song.jacket)) else {
            return Ok(false);
        };
        let Some(size) = total
            .checked_add(bytes.len())
            .filter(|size| *size <= feed::MAX_ZIP_BYTES)
        else {
            return Ok(false);
        };
        total = size;
        staging.write_jacket(song.id, &bytes)?;
    }
    Ok(true)
}

/// 첫 302로 태그를 읽고 바뀐 파일만 받아 준비, 받을 게 없으면 None
fn check_data(
    source: &Source,
    root: &Path,
    job: &CheckJob<'_>,
    state: &mut UpdateState,
    active: &Active,
) -> Result<Option<Ready>, CheckFailure> {
    let cancel = job.cancel;
    feed::ensure_running(cancel)?;
    let location = http::first_redirect(&source.latest, feed::MANIFEST_TIMEOUT_SECS, cancel)
        .map_err(|e| CheckFailure::http("latest", e))?;
    let tag = source.tag(&location)?;
    if !state.needs_manifest(&tag, active.tag())? {
        return Ok(None);
    }
    if let Some(generation) = store::existing_generation(root, &tag, active.name().as_deref())? {
        return prepare_existing(generation, job, state, active);
    }
    let bytes = http::download(
        &source.asset(&tag, store::MANIFEST_FILE)?,
        feed::MAX_MANIFEST_BYTES,
        feed::MANIFEST_TIMEOUT_SECS,
        cancel,
    )
    .map_err(|e| CheckFailure::http("manifest", e))?;
    let manifest = feed::Manifest::for_tag(&bytes, &tag)?;
    let needs = feed::Needs::between(active.generation.as_ref().map(|g| &g.manifest), &manifest);
    if !needs.any() {
        if let Some(active_tag) = active.tag() {
            state.confirm_unchanged(&tag, active_tag)?;
        }
        return Ok(None);
    }
    let manifest_hash = feed::sha256(&bytes);
    if !state.can_prepare(&manifest_hash, active.has_data()) {
        return Ok(None);
    }
    let _ = job.events.send(Event::Preparing(job.id));
    let mut preparing = false;
    let result = (|| {
        let json = |asset: &feed::model::JsonAsset,
                    needed: bool,
                    cap,
                    plain: &str|
         -> Result<Vec<u8>, CheckFailure> {
            feed::ensure_running(cancel)?;
            if !needed {
                let previous = active
                    .generation
                    .as_ref()
                    .ok_or_else(|| feed::Error::invalid("missing active generation"))?;
                return store::read_limited(&previous.path.join(plain), cap)
                    .map_err(CheckFailure::from);
            }
            let gz = http::download(
                &source.asset(&tag, &asset.file)?,
                feed::MAX_GZIP_BYTES,
                feed::JSON_TIMEOUT_SECS,
                cancel,
            )
            .map_err(|e| CheckFailure::http("json", e))?;
            feed::archive::decode_json_gzip(&gz, asset, cap).map_err(CheckFailure::from)
        };
        let core_bytes = json(
            &manifest.core,
            needs.core,
            feed::MAX_CORE_BYTES,
            store::CORE_JSON,
        )?;
        let dynamic_bytes = json(
            &manifest.dynamic,
            needs.dynamic,
            feed::MAX_DYNAMIC_BYTES,
            store::DYNAMIC_JSON,
        )?;
        let core = feed::Core::parse(&core_bytes)?;
        let staging = Staging::create(root)?;
        staging.write_data(&bytes, &core_bytes, &dynamic_bytes)?;
        let bundled = active.generation.is_none()
            && copy_bundled_jackets(&staging, job.bundled, &core, cancel)?;
        if needs.jackets && !bundled {
            let zip = http::download(
                &source.asset(&tag, feed::JACKETS_FILE)?,
                feed::MAX_ZIP_BYTES,
                feed::JACKETS_TIMEOUT_SECS,
                cancel,
            )
            .map_err(|e| CheckFailure::http("jackets", e))?;
            for jacket in feed::archive::read_jackets(&zip, &manifest.jackets, &core)? {
                feed::ensure_running(cancel)?;
                staging.write_jacket(jacket.id, jacket.bytes)?;
            }
        } else if !needs.jackets {
            let previous = active
                .generation
                .as_ref()
                .ok_or_else(|| feed::Error::invalid("missing active jackets"))?;
            staging.copy_jackets(&previous.path, &core, cancel)?;
        }
        let (generation, (mut catalog, refs)) = staging.finish(|generation| {
            state.begin_preparing(job.state_path, &manifest_hash)?;
            preparing = true;
            let refs = prepare_refs(generation, Some(active), job.refs, cancel, job.parallel)?;
            let catalog = feed_catalog(generation)?;
            feed::ensure_running(cancel)?;
            Ok((catalog, refs))
        })?;
        // 준비할 땐 staging 폴더였음, 옮긴 세대 폴더의 재킷을 보게 함
        catalog.jacket_dir = generation.path.join(store::JACKETS_DIR);
        Ok(make_ready(generation, catalog, refs, active))
    })();
    if preparing {
        state.finish_preparing(job.state_path);
    }
    result.map(Some)
}

fn prepare_existing(
    generation: Generation,
    job: &CheckJob<'_>,
    state: &mut UpdateState,
    active: &Active,
) -> Result<Option<Ready>, CheckFailure> {
    if !state.can_prepare(&generation.manifest_hash, active.has_data()) {
        return Ok(None);
    }
    state.begin_preparing(job.state_path, &generation.manifest_hash)?;
    let _ = job.events.send(Event::Preparing(job.id));
    let result = (|| {
        let refs = prepare_refs(&generation, Some(active), job.refs, job.cancel, job.parallel)?;
        let catalog = feed_catalog(&generation)?;
        feed::ensure_running(job.cancel)?;
        Ok(make_ready(generation, catalog, refs, active))
    })();
    state.finish_preparing(job.state_path);
    result.map(Some)
}

fn make_ready(
    generation: Generation,
    catalog: Catalog,
    refs: Arc<RefSet>,
    previous: &Active,
) -> Ready {
    let snapshot = Snapshot::from_data(&generation.core, &generation.dynamic);
    let summary = previous.generation.as_ref().map_or_else(
        || Summary {
            notice: previous.snapshot.changes_to(&snapshot),
            ..Summary::default()
        },
        |old| Summary::between(&previous.snapshot, &old.dynamic, &snapshot, &generation.dynamic),
    );
    let titles = catalog.titles();
    Ready {
        catalog,
        refs,
        titles,
        summary,
        active: Arc::new(Active {
            generation: Some(Arc::new(generation)),
            snapshot,
        }),
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

fn notice_text(notice: &Notice, recognized: bool) -> Option<String> {
    if !notice.any() {
        return None;
    }
    let mut parts = Vec::new();
    if notice.new_songs != 0 {
        parts.push(format!("새 곡 {}개", notice.new_songs));
    }
    if notice.new_patterns != 0 {
        parts.push(format!("새 패턴 {}개", notice.new_patterns));
    }
    // 추가가 있으면 재킷 변경은 뺌, 할 일이 없는 정보고 한 줄이 알림 폭을 넘지 않게
    let mut text = if parts.is_empty() {
        format!("자켓 {}개가 바뀌었어요", notice.changed_jackets)
    } else {
        format!("{}가 추가됐어요", parts.join(" · "))
    };
    // 다시 인식이 필요한 건 새 곡뿐, 새 패턴·재킷은 저장한 목록과 상관없음
    if recognized && notice.new_songs != 0 {
        text.push_str("\nDLC를 샀다면 다시 인식해 주세요");
    }
    Some(text)
}

/// 받아 둔 세대의 상태 줄 문구, 세대가 없으면 None
fn data_status(active: &Active) -> Option<String> {
    active
        .generation
        .as_ref()
        .map(|g| success_text(&g.manifest.published_at, &Summary::default()))
}

fn success_text(published: &str, summary: &Summary) -> String {
    let mut text = published_label(published).map_or_else(
        || CHECKED_TEXT.into(),
        |date| format!("곡 데이터 {date}"),
    );
    for (count, label) in [
        (summary.levels, "레벨"),
        (summary.tiers, "서열표"),
        (summary.pattern_info, "패턴 특성"),
    ] {
        if count != 0 {
            text.push_str(&format!(" · {label} {count}개 바뀜"));
        }
    }
    text
}

/// 발행 시각을 이 PC의 시간대로 바꾼 `M월 D일 HH:MM`
fn published_label(published: &str) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{
        FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
    };
    let part = |range: std::ops::Range<usize>| published.get(range)?.parse::<u16>().ok();
    let utc = SYSTEMTIME {
        wYear: part(0..4)?,
        wMonth: part(5..7)?,
        wDay: part(8..10)?,
        wHour: part(11..13)?,
        wMinute: part(14..16)?,
        wSecond: part(17..19)?,
        ..Default::default()
    };
    let offset = if published.ends_with(['Z', 'z']) {
        0
    } else {
        let start = published.len().checked_sub(6)?;
        let zone = published.get(start..)?;
        let hours = zone.get(1..3)?.parse::<i64>().ok()?;
        let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
        (hours * 3600 + minutes * 60) * if zone.starts_with('-') { -1 } else { 1 }
    };
    let mut stamp = FILETIME::default();
    unsafe {
        SystemTimeToFileTime(&utc, &mut stamp).ok()?;
    }
    let ticks = (u64::from(stamp.dwHighDateTime) << 32) | u64::from(stamp.dwLowDateTime);
    let ticks = u64::try_from(i128::from(ticks) - i128::from(offset) * 10_000_000).ok()?;
    let stamp = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&stamp, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(format!(
        "{}월 {}일 {:02}:{:02}",
        local.wMonth, local.wDay, local.wHour, local.wMinute
    ))
}

fn failure_text(state: &UpdateState) -> String {
    let seconds = state
        .next_eligible_at
        .unwrap_or_else(now)
        .saturating_sub(now())
        .max(0);
    let hours = seconds.saturating_add(3599) / 3600;
    format!("곡 데이터를 확인하지 못했어요 · {hours}시간 뒤 다시 확인")
}
