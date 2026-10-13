//! 곡 데이터 선택과 백그라운드 갱신

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use dmpicker_engine::{RefSet, features::FEATURE_VERSION};
use dmpicker_update as feed;
use feed::exe::{ExeRelease, Version};
use feed::notify::{Notice, Snapshot, Summary};
use feed::state::{CheckAction, CheckError, Scheduler, UpdateState};
use feed::store::{self, Generation, Pointer, Staging};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
};

use crate::{catalog::Catalog, http};

const BASELINE_FILE: &str = "notified-baseline.json";

pub struct Active {
    pub generation: Option<Arc<Generation>>,
    pub snapshot: Snapshot,
}

impl Active {
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
            let name = g.manifest.generation_name();
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

pub type InitialLoad = (u64, Result<Startup, String>);

/// 시작 준비는 별도 스레드에서 마치고 세대 번호와 함께 전달
pub fn start_initial(root: PathBuf, bundled: PathBuf, generation: u64) -> JoinHandle<InitialLoad> {
    std::thread::spawn(move || (generation, load_startup(&root, &bundled, true)))
}

/// 시작 준비가 끝난 뒤에만 pointer를 고치고 남은 세대를 정리
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
    let selected = store::choose_generation(&data, blocked.as_deref(), |candidate| {
        if !state.begin_loading(&state_path, &candidate.name)? {
            return Err(feed::Error::invalid("generation was interrupted"));
        }
        let result = (|| {
            let generation = candidate.load()?;
            let refs = if prepare {
                Some(initial_refs(&generation)?)
            } else {
                None
            };
            if let Some(refs) = &refs {
                if let Err(error) = store::atomic_write(
                    &generation.path.join("features.bin"),
                    &refs.to_cache_bytes(&generation.core.feature_signature(FEATURE_VERSION))?,
                ) {
                    eprintln!("data features: {error}");
                }
            }
            let catalog =
                Catalog::from_feed(&generation.core, &generation.dynamic, &generation.path)
                    .map_err(feed::Error::invalid)?;
            let snapshot = Snapshot::from_data(&generation.core, &generation.dynamic);
            Ok((catalog, refs, Some(Arc::new(generation)), snapshot))
        })();
        if prepare {
            if let Err(error) = state.finish_loading(&state_path) {
                eprintln!("data loading marker: {error}");
            }
        } else {
            state.loading = blocked.clone();
            state.save(&state_path)?;
        }
        result
    })
    .map_err(|e| e.to_string())?;
    #[allow(unused_mut)]
    let mut loaded = selected.map(|selected| selected.value);
    let (catalog, refs, generation, snapshot) =
        loaded.unwrap_or_else(|| (Catalog::empty(bundled), None, None, Snapshot::default()));
    if let Some(generation) = generation
        .as_ref()
        .filter(|g| prepare && g.path.parent() == Some(data.as_path()))
    {
        let name = generation.manifest.generation_name();
        let previous = ["pointer.json", "pointer.bak"]
            .into_iter()
            .filter_map(|file| store::read_pointer(&data.join(file)).ok())
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
    interrupted(cancel)?;
    let jackets: Vec<_> = generation
        .core
        .songs
        .iter()
        .map(|s| (s.id, s.jacket.clone()))
        .collect();
    let old: Vec<_> = active
        .map(|a| {
            a.snapshot
                .songs
                .iter()
                .map(|(&id, hash)| (id, hash.clone()))
                .collect()
        })
        .unwrap_or_default();
    // 자켓이 그대로면 지금 RefSet을 그대로 씀, 레벨만 바뀐 갱신은 다시 계산 안 함
    if jackets == old {
        if let Some(previous) = previous {
            return Ok(previous.clone());
        }
    }
    if let Some(refs) = cached_refs(generation) {
        return Ok(refs);
    }
    if parallel {
        let refs = initial_refs(generation)?;
        interrupted(cancel)?;
        return Ok(refs);
    }
    let reuse = previous.map(|refs| (refs.as_ref(), old.as_slice()));
    let refs = RefSet::update_jackets(&jackets, reuse, cancel, |id| {
        decode_jacket(&generation.path, id)
    })?;
    Ok(Arc::new(refs))
}

fn cached_refs(generation: &Generation) -> Option<Arc<RefSet>> {
    let signature = generation.core.feature_signature(FEATURE_VERSION);
    let bytes = store::read_limited(
        &generation.path.join("features.bin"),
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
    let jackets: Vec<_> = generation
        .core
        .songs
        .iter()
        .map(|s| (s.id, s.jacket.clone()))
        .collect();
    Ok(Arc::new(RefSet::load_jackets(&jackets, |id| {
        decode_jacket(&generation.path, id)
    })?))
}

fn decode_jacket(path: &Path, id: u32) -> std::io::Result<dmpicker_engine::img::Rgb> {
    let bytes = store::read_limited(
        &path.join("jackets").join(format!("{id}.webp")),
        feed::MAX_JACKET_BYTES,
    )
    .map_err(std::io::Error::other)?;
    feed::archive::validate_webp(&bytes).map_err(std::io::Error::other)?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP)
        .map_err(std::io::Error::other)?
        .into_rgb8();
    if image.dimensions() != (128, 128) {
        return Err(std::io::Error::other("invalid jacket dimensions"));
    }
    dmpicker_engine::img::Rgb::from_raw(128, 128, image.into_raw())
        .ok_or_else(|| std::io::Error::other("invalid jacket pixels"))
}

fn interrupted(cancel: &AtomicBool) -> feed::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "update cancelled").into())
    } else {
        Ok(())
    }
}

fn copy_jackets(
    staging: &Staging,
    source: &Generation,
    core: &feed::Core,
    cancel: &AtomicBool,
) -> feed::Result<()> {
    for song in &core.songs {
        interrupted(cancel)?;
        let bytes = store::read_limited(
            &source
                .path
                .join("jackets")
                .join(format!("{}.webp", song.id)),
            feed::MAX_JACKET_BYTES,
        )?;
        staging.write_jacket(song.id, &bytes)?;
    }
    Ok(())
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
pub enum Phase {
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

enum Command {
    Check {
        id: u64,
        state: UpdateState,
        active: Arc<Active>,
        refs: Option<Arc<RefSet>>,
        check_data: bool,
        check_exe: bool,
        origin: CheckOrigin,
    },
    Apply {
        id: u64,
        state: UpdateState,
        active: Arc<Active>,
        previous: Option<String>,
    },
    Baseline(Snapshot),
    ExeNoticeShown(Version),
    Stop,
}

enum Event {
    Preparing(u64),
    Checked {
        id: u64,
        state: UpdateState,
        result: Result<Option<Box<Ready>>, Failure>,
        checked_data: bool,
    },
    Applied {
        id: u64,
        state: UpdateState,
        result: feed::Result<()>,
    },
}

/// 다음 검색창에서 표시할 버전 알림
#[allow(dead_code)]
pub struct ExeNotice {
    release: ExeRelease,
    pub text: String,
    pub action_text: &'static str,
}

#[allow(dead_code)]
impl ExeNotice {
    pub fn version(&self) -> Version {
        self.release.version()
    }

    /// 검증한 태그와 고정 경로로 만든 주소만 엶
    pub fn open_page(&self) -> std::io::Result<()> {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};

        let url = HSTRING::from(self.release.page_url());
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
}

fn exe_release(state: &UpdateState) -> Option<ExeRelease> {
    let tag = state.latest_version.as_deref()?;
    let release =
        feed::exe::parse_exe_location(&format!("{}{tag}", feed::EXE_RELEASE_BASE)).ok()?;
    (release.version() > Version::parse(env!("CARGO_PKG_VERSION")).ok()?).then_some(release)
}

fn exe_notice(state: &UpdateState) -> Option<ExeNotice> {
    let release = exe_release(state)?;
    state
        .needs_exe_notice(release.version())
        .then(|| ExeNotice {
            text: format!("새 버전 {}이 나왔어요", release.version()),
            action_text: "릴리스 페이지 열기",
            release,
        })
}

fn exe_status(state: &UpdateState) -> Option<String> {
    exe_release(state).map(|release| format!("새 버전 {}이 나왔어요", release.version()))
}

#[derive(Debug)]
struct Failure {
    stage: &'static str,
    status: Option<u16>,
    retry_after: Option<String>,
    schema: Option<u64>,
    cancelled: bool,
}

impl From<feed::Error> for Failure {
    fn from(error: feed::Error) -> Self {
        let schema = match &error {
            feed::Error::UnsupportedSchema(value) => Some(*value),
            _ => None,
        };
        let cancelled =
            matches!(&error, feed::Error::Io(e) if e.kind() == std::io::ErrorKind::Interrupted);
        Self {
            stage: "validation",
            status: None,
            retry_after: None,
            schema,
            cancelled,
        }
    }
}

impl Failure {
    fn http(stage: &'static str, error: http::Failure) -> Self {
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
    pub status_text: String,
    pub schema_text: Option<String>,
    pub exe_status_text: Option<String>,
    pub exe_notice: Option<ExeNotice>,
    pub data_notice: Option<&'static str>,
    pub phase: Phase,
    state: UpdateState,
    baseline: Snapshot,
    scheduler: Scheduler,
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
        let status_text = startup
            .active
            .generation
            .as_ref()
            .map(|g| success_text(&g.manifest.published_at, &Summary::default()))
            .unwrap_or_default();
        Self {
            active: startup.active.clone(),
            status_text,
            schema_text: None,
            exe_status_text: exe_status(&startup.state),
            exe_notice: None,
            data_notice: (!startup.active.has_data()).then_some("곡 데이터를 받고 있어요"),
            phase: Phase::Idle,
            state: startup.state.clone(),
            baseline: startup.baseline.clone(),
            scheduler: Scheduler::default(),
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
    pub fn opened(&mut self, busy: bool, refs: Option<Arc<RefSet>>) {
        if self.initializing {
            return;
        }
        self.exe_notice = exe_notice(&self.state);
        self.begin(CheckOrigin::Search, busy, refs);
    }

    fn begin(&mut self, origin: CheckOrigin, busy: bool, refs: Option<Arc<RefSet>>) {
        if self.initializing || self.phase != Phase::Idle {
            return;
        }
        let action = match origin {
            CheckOrigin::Startup => self.scheduler.startup(&mut self.state, now(), busy),
            CheckOrigin::Search => self.scheduler.due(
                &mut self.state,
                now(),
                busy,
                self.active.generation.is_none(),
            ),
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
                self.data_notice = Some("곡 데이터를 받고 있어요");
            }
            if self
                .commands
                .send(Command::Check {
                    id: self.id,
                    state: self.state.clone(),
                    active: self.active.clone(),
                    refs,
                    check_data,
                    check_exe,
                    origin,
                })
                .is_err()
            {
                self.scheduler.finish();
                self.phase = Phase::Idle;
                self.refresh_data_notice();
            }
        }
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

    /// 실제로 표시한 버전만 기록
    #[allow(dead_code)]
    pub fn exe_notice_shown(&mut self, version: Version) {
        if self
            .exe_notice
            .as_ref()
            .is_some_and(|notice| notice.version() == version)
        {
            self.state.record_exe_notice(version);
            self.exe_notice = None;
            let _ = self.commands.send(Command::ExeNoticeShown(version));
        }
    }

    fn accept_state(&mut self, mut state: UpdateState) {
        state.notified_version = state.notified_version.max(self.state.notified_version);
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
            Some("곡 데이터를 받고 있어요")
        } else {
            Some("곡 데이터를 받지 못했어요")
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
                        other => {
                            if let Err(error) = other {
                                if let Some(schema) = error.schema {
                                    if self.state.needs_schema_notice(schema) {
                                        self.schema_text = Some("앱 업데이트가 필요해요".into());
                                    }
                                }
                                if !error.cancelled {
                                    self.status_text = failure_text(&self.state);
                                }
                            } else if checked_data {
                                self.status_text = self
                                    .active
                                    .generation
                                    .as_ref()
                                    .map(|g| {
                                        success_text(&g.manifest.published_at, &Summary::default())
                                    })
                                    .unwrap_or_else(|| "곡 데이터 확인됨".into());
                                self.schema_text = None;
                            }
                            self.phase = Phase::Idle;
                            self.scheduler.finish();
                        }
                    }
                }
                Event::Applied { id, state, result } if id == self.id => {
                    self.accept_state(state);
                    if result.is_err() {
                        self.status_text = "곡 데이터를 저장하지 못했어요".into();
                    }
                    self.phase = Phase::Idle;
                    self.scheduler.finish();
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
        let titles = Arc::new(
            startup
                .catalog
                .songs
                .iter()
                .map(|song| (song.id, song.name.clone()))
                .collect(),
        );
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
        self.schema_text = None;
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
            self.phase = Phase::Idle;
            self.scheduler.finish();
            self.status_text = "곡 데이터를 저장하지 못했어요".into();
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
            Command::ExeNoticeShown(version) => {
                let result = UpdateState::load(&state_path).and_then(|mut state| {
                    state.record_exe_notice(version);
                    state.save(&state_path)
                });
                if let Err(error) = result {
                    eprintln!("version notice: {error}");
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
                    let pointer =
                        Pointer::new(&generation.manifest.generation_name(), previous.as_deref())?;
                    store::write_pointer(&root.join("data"), &pointer)?;
                    state.record_data_applied(&generation.manifest.release_tag)?;
                    state.save(&state_path)
                })();
                let _ = send.send(Event::Applied { id, state, result });
            }
            Command::Check {
                id,
                mut state,
                active,
                refs,
                check_data,
                check_exe,
                origin,
            } => {
                if check_exe && !cancel.load(Ordering::Relaxed) {
                    check_exe_version(&exe_latest_url(), &state_path, &mut state, &cancel);
                }
                let mut result = if check_data {
                    state
                        .save(&state_path)
                        .map_err(Failure::from)
                        .and_then(|_| {
                            check(
                                &source,
                                &root.join("data"),
                                &state_path,
                                id,
                                &mut state,
                                &active,
                                refs.as_ref(),
                                &cancel,
                                &send,
                                &Preparation {
                                    bundled: &bundled,
                                    parallel: origin == CheckOrigin::Startup,
                                },
                            )
                        })
                } else {
                    Ok(None)
                };
                if check_data {
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
                    if check_data {
                        result = Err(error.into());
                    }
                }
                let _ = send.send(Event::Checked {
                    id,
                    state,
                    result: result.map(|ready| ready.map(Box::new)),
                    checked_data: check_data,
                });
            }
        }
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
    if let Ok(location) = result {
        if let (Ok(release), Ok(current)) = (
            feed::exe::parse_exe_location(&location),
            Version::parse(env!("CARGO_PKG_VERSION")),
        ) {
            let _ = state.record_exe_available(release.tag(), current);
        }
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

    fn asset(&self, tag: &str, file: &str) -> feed::Result<String> {
        feed::model::validate_release_tag(tag)?;
        if !matches!(
            file,
            "manifest.json" | feed::CORE_FILE | feed::DYNAMIC_FILE | feed::JACKETS_FILE
        ) {
            return Err(feed::Error::invalid("invalid asset name"));
        }
        Ok(format!("{}{tag}/{file}", self.base))
    }
}

struct Preparation<'a> {
    bundled: &'a Path,
    parallel: bool,
}

/// 동봉 자켓이 모두 맞으면 복사, 하나라도 없거나 다르면 zip을 받음
fn copy_bundled_jackets(
    staging: &Staging,
    bundled: &Path,
    core: &feed::Core,
    cancel: &AtomicBool,
) -> feed::Result<bool> {
    let mut total = 0usize;
    for song in &core.songs {
        interrupted(cancel)?;
        let valid = (|| {
            let bytes = store::read_limited(
                &bundled.join("jackets").join(format!("{}.webp", song.id)),
                feed::MAX_JACKET_BYTES,
            )?;
            if feed::sha256(&bytes) != song.jacket {
                return Err(feed::Error::invalid("bundled jacket hash mismatch"));
            }
            feed::archive::validate_webp(&bytes)?;
            Ok(bytes)
        })();
        let Ok(bytes) = valid else {
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
#[allow(clippy::too_many_arguments)]
fn check(
    source: &Source,
    root: &Path,
    state_path: &Path,
    id: u64,
    state: &mut UpdateState,
    active: &Active,
    refs: Option<&Arc<RefSet>>,
    cancel: &AtomicBool,
    events: &mpsc::Sender<Event>,
    preparation: &Preparation<'_>,
) -> Result<Option<Ready>, Failure> {
    interrupted(cancel)?;
    let location = http::first_redirect(&source.latest, feed::MANIFEST_TIMEOUT_SECS, cancel)
        .map_err(|e| Failure::http("latest", e))?;
    let tag = source.tag(&location)?;
    if !state.needs_manifest(&tag, active.tag())? {
        return Ok(None);
    }
    if let Some(generation) = store::existing_generation(root, &tag)? {
        return prepare_existing(
            generation,
            state_path,
            id,
            state,
            active,
            refs,
            cancel,
            events,
            preparation.parallel,
        );
    }
    let bytes = http::download(
        &source.asset(&tag, "manifest.json")?,
        feed::MAX_MANIFEST_BYTES,
        feed::MANIFEST_TIMEOUT_SECS,
        cancel,
    )
    .map_err(|e| Failure::http("manifest", e))?;
    let manifest = feed::Manifest::for_tag(&bytes, &tag)?;
    let needs = feed::Needs::between(active.generation.as_ref().map(|g| &g.manifest), &manifest);
    if !needs.any() {
        if let Some(active_tag) = active.tag() {
            state.confirm_unchanged(&tag, active_tag)?;
        }
        return Ok(None);
    }
    if !state.begin_preparing(state_path, &feed::sha256(&bytes))? {
        return Ok(None);
    }
    let _ = events.send(Event::Preparing(id));
    let result = (|| {
        let json = |asset: &feed::model::JsonAsset,
                    needed: bool,
                    cap,
                    plain: &str|
         -> Result<Vec<u8>, Failure> {
            interrupted(cancel)?;
            if !needed {
                let previous = active
                    .generation
                    .as_ref()
                    .ok_or_else(|| feed::Error::invalid("missing active generation"))?;
                return store::read_limited(&previous.path.join(plain), cap).map_err(Failure::from);
            }
            let gz = http::download(
                &source.asset(&tag, &asset.file)?,
                feed::MAX_GZIP_BYTES,
                feed::JSON_TIMEOUT_SECS,
                cancel,
            )
            .map_err(|e| Failure::http("json", e))?;
            feed::archive::decode_json_gzip(&gz, asset, cap).map_err(Failure::from)
        };
        let core_bytes = json(
            &manifest.core,
            needs.core,
            feed::MAX_CORE_BYTES,
            "core.json",
        )?;
        let dynamic_bytes = json(
            &manifest.dynamic,
            needs.dynamic,
            feed::MAX_DYNAMIC_BYTES,
            "dynamic.json",
        )?;
        let core = feed::Core::parse(&core_bytes)?;
        let staging = Staging::create(root, id)?;
        staging.write_data(&bytes, &core_bytes, &dynamic_bytes)?;
        let bundled = active.generation.is_none()
            && copy_bundled_jackets(&staging, preparation.bundled, &core, cancel)?;
        if needs.jackets && !bundled {
            let zip = http::download(
                &source.asset(&tag, feed::JACKETS_FILE)?,
                feed::MAX_ZIP_BYTES,
                feed::JACKETS_TIMEOUT_SECS,
                cancel,
            )
            .map_err(|e| Failure::http("jackets", e))?;
            for jacket in feed::archive::read_jackets(&zip, &manifest.jackets, &core)? {
                interrupted(cancel)?;
                staging.write_jacket(jacket.id, jacket.bytes)?;
            }
        } else if !needs.jackets {
            let previous = active
                .generation
                .as_ref()
                .ok_or_else(|| feed::Error::invalid("missing active jackets"))?;
            copy_jackets(&staging, previous, &core, cancel)?;
        }
        let (generation, (mut catalog, refs)) = staging.finish(|generation| {
            let refs = prepare_refs(generation, Some(active), refs, cancel, preparation.parallel)?;
            let catalog =
                Catalog::from_feed(&generation.core, &generation.dynamic, &generation.path)
                    .map_err(feed::Error::invalid)?;
            let cache = refs.to_cache_bytes(&generation.core.feature_signature(FEATURE_VERSION))?;
            store::atomic_write(&generation.path.join("features.bin"), &cache)?;
            interrupted(cancel)?;
            Ok((catalog, refs))
        })?;
        catalog.jacket_dir = generation.path.join("jackets");
        Ok(make_ready(generation, catalog, refs, active))
    })();
    state.finish_preparing(state_path)?;
    result.map(Some)
}

#[allow(clippy::too_many_arguments)]
fn prepare_existing(
    generation: Generation,
    state_path: &Path,
    id: u64,
    state: &mut UpdateState,
    active: &Active,
    previous: Option<&Arc<RefSet>>,
    cancel: &AtomicBool,
    events: &mpsc::Sender<Event>,
    parallel: bool,
) -> Result<Option<Ready>, Failure> {
    if !state.begin_preparing(state_path, &generation.manifest_hash)? {
        return Ok(None);
    }
    let _ = events.send(Event::Preparing(id));
    let result = (|| {
        let refs = prepare_refs(&generation, Some(active), previous, cancel, parallel)?;
        let catalog = Catalog::from_feed(&generation.core, &generation.dynamic, &generation.path)
            .map_err(feed::Error::invalid)?;
        interrupted(cancel)?;
        Ok(make_ready(generation, catalog, refs, active))
    })();
    state.finish_preparing(state_path)?;
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
        |old| {
            Summary::between(
                &old.core,
                &old.dynamic,
                &generation.core,
                &generation.dynamic,
            )
        },
    );
    let titles = Arc::new(
        catalog
            .songs
            .iter()
            .map(|s| (s.id, s.name.clone()))
            .collect(),
    );
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
    if notice.changed_jackets != 0 {
        parts.push(format!("자켓 {}개", notice.changed_jackets));
    }
    let verb = if notice.changed_jackets != 0 {
        "가 바뀌었어요"
    } else {
        "가 추가됐어요"
    };
    let mut text = format!("{}{verb}", parts.join(" · "));
    if recognized {
        text.push_str("\nDLC를 샀다면 다시 인식해 주세요");
    }
    Some(text)
}

fn success_text(published: &str, summary: &Summary) -> String {
    let mut text = published_label(published).map_or_else(
        || "곡 데이터 확인됨".into(),
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
