//! DM PICKER: DJMAX RESPECT V 곡 검색기

// 릴리스 빌드는 콘솔 창 없이 실행
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod beam;
mod catalog;
mod driver;
mod edit;
mod gfx;
mod hands;
mod hotkey;
mod http;
mod icons;
mod keywords;
mod pace;
mod playlist;
mod popup;
mod scene;
mod sys;
mod toast;
mod tray;
mod update;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dmpicker_engine::{capture, input, ScreenKind};
use windows::core::{w, Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VIRTUAL_KEY};
use windows::Win32::UI::WindowsAndMessaging::*;

use hotkey::Hotkey;
use popup::{Action, Popup};
use toast::{Badge, Toast};

/// 사용자에게 보이는 이름: 창 제목, 오류 창, 트레이 툴팁, 시작 시 실행 값 이름
const APP_NAME: &str = "DM PICKER";
const HOTKEY_ID: i32 = 1;
const TIMER_FOREGROUND: usize = 1; // 검색창 창의 타이머, popup.rs의 TIMER_CARET (2)·TIMER_FOCUS (3)·TIMER_SEARCH (4)도 같은 창
const WM_APP_STATUS: u32 = WM_APP + 1;
/// 단축키 뒤 화면 확인이 끝남 (`request_open`)
const WM_APP_OPEN: u32 = WM_APP + 2;

/// 작업 스레드가 올리고 UI 스레드가 알림에 띄우는 상태 문구
struct Status {
    text: String,
    badge: Badge,
    secs: Option<f32>,
}

struct App {
    popup: Popup,
    toast: Toast,
    tray: tray::Tray,
    hotkey_on: bool,
    refs: Arc<Mutex<Option<Arc<dmpicker_engine::RefSet>>>>,
    titles: Arc<HashMap<u32, String>>,
    store: PathBuf,
    status: Arc<Mutex<Option<Status>>>,
    job: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>, // 실행 중인 작업과 취소 플래그
    retired_jobs: Vec<std::thread::JoinHandle<()>>,
    peek_job: Option<std::thread::JoinHandle<()>>,
    initial_refs: Option<std::thread::JoinHandle<update::Startup>>,
    updates: update::Updater,
    /// 곡 이동 중 화면이 저장된 목록과 달랐음
    /// 새 목록이 저장되거나 다음 이동이 도착할 때까지 검색창에서 다시 인식을 권함
    stale: Arc<AtomicBool>,
    /// 단축키를 눌러 검색창을 열지 화면을 확인하는 중, 그동안 누른 단축키는 무시함
    peeking: bool,
    peeked: Arc<Mutex<Peeked>>,
    /// 지금 검색창을 연 선곡 화면, 첫 데이터를 넣을 때 고를 수 있는 곡을 다시 계산
    open_kind: Option<ScreenKind>,
    playlists: playlist::Loader,
}

/// 단축키 뒤 화면 확인 결과: 선곡 목록이 보였으면 Some(그 화면, 그 목록에서 고를 수 있는 곡, 첫 인식 전엔 None)
type Peeked = Option<(ScreenKind, Option<HashSet<u32>>)>;

static mut APP: *mut App = std::ptr::null_mut();

fn app() -> Option<&'static mut App> {
    unsafe { APP.as_mut() }
}

fn store_dir() -> PathBuf {
    std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(".")).join("DmPicker")
}

fn data_dir() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.parent().ok_or_else(|| std::io::Error::other("missing executable directory"))?.join("data"))
}

fn sync_hotkey(a: &mut App) {
    let game_front = capture::find_game_window().is_some_and(input::is_foreground);
    let busy = a.job.as_ref().is_some_and(|(_, h)| !h.is_finished());
    let want = a.popup.visible || game_front || busy;
    if want && !a.hotkey_on {
        a.hotkey_on = register_hotkey(a.popup.hwnd, a.popup.hotkey);
    } else if !want && a.hotkey_on {
        unsafe {
            let _ = UnregisterHotKey(Some(a.popup.hwnd), HOTKEY_ID);
        }
        a.hotkey_on = false;
    }
}

/// 단축키를 검색창 창에 등록, 다른 프로그램이 쓰고 있으면 false
fn register_hotkey(hwnd: HWND, hk: Hotkey) -> bool {
    unsafe { RegisterHotKey(Some(hwnd), HOTKEY_ID, HOT_KEY_MODIFIERS(hk.mods.0 | MOD_NOREPEAT.0), hk.vk.0 as u32) }.is_ok()
}

/// 검색창에서 받은 단축키로 바꿈, 다른 프로그램이 쓰고 있으면 지금 단축키를 되돌리고 false
fn change_hotkey(a: &mut App, hk: Hotkey) -> bool {
    let hwnd = a.popup.hwnd;
    if a.hotkey_on {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
        }
    }
    if register_hotkey(hwnd, hk) {
        a.hotkey_on = true;
        return true;
    }
    a.hotkey_on = a.hotkey_on && register_hotkey(hwnd, a.popup.hotkey);
    false
}

/// 트레이 메뉴: 단축키를 기본값으로 되돌려 저장, 기본값도 다른 프로그램이 쓰고 있으면 알리고 그대로 둠
fn reset_hotkey(a: &mut App) {
    let hk = Hotkey::DEFAULT;
    if change_hotkey(a, hk) {
        a.popup.hotkey = hk;
        if save_settings(a) {
            a.toast.show(&format!("단축키를 {}로 되돌렸어요", hk.label()), Badge::None, Some(2.0));
        }
    } else {
        a.toast.show(&format!("다른 프로그램이 {}를 쓰고 있어요", hk.label()), Badge::None, Some(3.0));
    }
}

/// 아무 스레드에서나 상태 문구를 올림
fn post_status(slot: &Arc<Mutex<Option<Status>>>, hwnd: isize, text: impl Into<String>, badge: Badge, secs: Option<f32>) {
    let text = text.into();
    println!("STATUS {text}");
    *slot.lock().unwrap() = Some(Status { text, badge, secs });
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_APP_STATUS, WPARAM(0), LPARAM(0));
    }
}

/// 단축키로 검색창을 엶, 선곡 목록이 보일 때만 (플레이 중에 잘못 눌러도 포커스를 안 가져감)
/// 화면 확인과 고를 곡 읽기는 작업 스레드에서, 끝나면 `WM_APP_OPEN`
/// 목록이 안 보이면 아무것도 안 함
fn request_open(a: &mut App) {
    if a.peeking {
        return;
    }
    // 닫히는 중인 검색창이 화면 캡처에 섞이지 않게 바로 숨기고 포커스를 게임에 돌려줌
    if a.popup.visible {
        a.popup.hide_now();
        a.updates.closed();
        // 숨긴 검색창이 다시 합성된 화면에서 빠질 때까지
        unsafe {
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        }
        if let Some(game) = capture::find_game_window() {
            input::request_foreground(game);
        }
    }
    a.peeking = true;
    let (slot, hwnd) = (a.peeked.clone(), a.popup.hwnd.0 as isize);
    let (store, titles) = (a.store.clone(), a.titles.clone());
    a.peek_job = Some(std::thread::spawn(move || {
        let _done = Done(hwnd);
        let t0 = std::time::Instant::now();
        let kind = driver::peek();
        eprintln!("peek: {kind:?} ({} ms)", t0.elapsed().as_millis());
        *slot.lock().unwrap() = kind.map(|k| (k, driver::searchable(&store, k, &titles)));
    }));
}

/// drop될 때 검색창 창에 `WM_APP_OPEN`을 보냄, 확인 스레드가 도중에 끝나도 `peeking`이 풀리게
struct Done(isize);

impl Drop for Done {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as _)), WM_APP_OPEN, WPARAM(0), LPARAM(0));
        }
    }
}

/// 새 취소 플래그로 `f`를 작업 스레드에서 실행 (돌던 작업은 취소)
fn spawn_job(a: &mut App, f: impl FnOnce(&AtomicBool, &dyn Fn(&str, Badge, Option<f32>)) + Send + 'static) {
    if let Some((c, handle)) = a.job.take() {
        c.store(true, Ordering::Relaxed);
        a.retired_jobs.push(handle);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let slot = a.status.clone();
    let hwnd = a.popup.hwnd.0 as isize;
    let handle = std::thread::spawn(move || {
        eprintln!("job: thread started");
        let say = |t: &str, b: Badge, s: Option<f32>| post_status(&slot, hwnd, t, b, s);
        f(&cancel, &say);
    });
    a.job = Some((flag, handle));
}

fn settings_path() -> PathBuf {
    store_dir().join("settings.json")
}

fn load_settings(p: &mut Popup) {
    let Ok(raw) = std::fs::read(settings_path()) else { return };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { return };
    apply_settings(p, &v);
}

/// 설정 파일 내용을 검색창에 넣음, 없거나 못 읽는 값은 그대로 (1.0.0 파일엔 keep_query·show_dlc가 없음)
fn apply_settings(p: &mut Popup, v: &serde_json::Value) {
    if let Some(m) = v["mode"].as_str() {
        if let Some(m) = dmpicker_search::Mode::from_label(m) {
            p.mode_choice = m;
        }
    }
    if let Some(d) = v["diff"].as_str().and_then(dmpicker_search::Diff::from_label) {
        p.diff_choice = d;
    }
    if let Some(k) = v["hotkey"].as_str().and_then(Hotkey::parse) {
        p.hotkey = k;
    }
    if let Some(b) = v["keep_query"].as_bool() {
        p.keep_query = b;
    }
    if let Some(b) = v["show_dlc"].as_bool() {
        p.show_dlc = b;
    }
}

fn settings_json(p: &Popup) -> serde_json::Value {
    serde_json::json!({ "mode": p.mode_choice.label(), "diff": p.diff_choice.label(), "hotkey": p.hotkey.label(), "keep_query": p.keep_query, "show_dlc": p.show_dlc })
}

/// 저장하지 못하면 알림 (다시 켜면 예전 값으로 돌아감)
fn save_settings(a: &mut App) -> bool {
    let ok = sys::save_file(&settings_path(), settings_json(&a.popup).to_string().as_bytes()).is_ok();
    if !ok {
        a.toast.show("설정을 저장하지 못했어요", Badge::None, Some(3.0));
    }
    ok
}

fn nav_mode(m: dmpicker_search::Mode) -> dmpicker_nav::ButtonMode {
    match m {
        dmpicker_search::Mode::B4 => dmpicker_nav::ButtonMode::B4,
        dmpicker_search::Mode::B5 => dmpicker_nav::ButtonMode::B5,
        dmpicker_search::Mode::B6 => dmpicker_nav::ButtonMode::B6,
        dmpicker_search::Mode::B8 => dmpicker_nav::ButtonMode::B8,
    }
}

/// 검색창을 닫고 포커스를 게임에 돌려줌 (우리가 앞에 있어서 가능)
/// 작업에 필요한 걸 돌려주고, 재킷 참조를 불러오는 중이면 알린 뒤 None
fn hand_over(a: &mut App) -> Option<driver::Ctx> {
    a.popup.hide();
    a.updates.closed();
    if let Some(game) = capture::find_game_window() {
        input::request_foreground(game);
    }
    let Some(refs) = a.refs.lock().unwrap().clone() else {
        a.toast.show("준비 중이에요\n잠시 뒤 다시 시도해 주세요", Badge::None, Some(2.0));
        return None;
    };
    Some(driver::Ctx { refs, store: a.store.clone(), titles: a.titles.clone(), hotkey: a.popup.hotkey })
}

/// `playlist_id`는 플레이리스트 결과에서 골랐을 때 그 플레이리스트
fn on_pick(a: &mut App, id: u32, chart: Option<(dmpicker_search::Mode, dmpicker_search::Diff)>, playlist_id: Option<String>) {
    let name = a.titles.get(&id).cloned().unwrap_or_default();
    // 레벨 검색·플레이리스트에서 고른 패턴이면 그 채보, 아니면 설정대로 (설정은 안 바꿈)
    let (m, d) = chart.unwrap_or((a.popup.mode_choice, a.popup.diff_choice));
    let (mode, diff) = (nav_mode(m), d.index());
    println!("PICK {id} {name} ({} {})", m.label(), d.label());
    // 검색창이 아는 인식 곡 수로 거르지 않음, 작업이 저장된 목록을 읽음 (짐작은 낡았을 수 있음)
    let Some(ctx) = hand_over(a) else { return };
    // 이동을 시작한 곡만 히스토리에 남김, 플레이리스트에서 골랐으면 그 플레이리스트 줄로
    let entry = match playlist_id {
        Some(list) => playlist::Recent::Playlist { id: list, last: playlist::Pattern { title: id, mode: m, diff: d } },
        None => playlist::Recent::Song(id),
    };
    a.popup.remember_pick(entry);
    let (stale, hk) = (a.stale.clone(), a.popup.hotkey.label());
    spawn_job(a, move |cancel, say| match driver::run_pick(&ctx, id, mode, diff, cancel) {
        driver::JobResult::Arrived { presses, ms, diff_note } => {
            stale.store(false, Ordering::Relaxed);
            println!("ARRIVED {name} ({presses} keys, {ms} ms)");
            if let Some(n) = diff_note {
                say(&n, Badge::None, Some(3.0));
            }
        }
        driver::JobResult::NotFound => say(&format!("내 목록에서 찾지 못한 곡이에요\n{name} · 다시 인식해 보세요"), Badge::None, Some(3.0)),
        driver::JobResult::NeedScan => say(&format!("먼저 내 곡 목록을 인식해 주세요\n{hk} → 목록 인식 시작"), Badge::None, Some(3.0)),
        driver::JobResult::ListMismatch => {
            stale.store(true, Ordering::Relaxed);
            say(&format!("저장된 곡 목록과 화면이 달라요\n계정이나 DLC가 바뀌었다면 {hk} → 다시 인식"), Badge::None, Some(5.0))
        }
        driver::JobResult::Touched(why) => say(&format!("이동하지 못했어요\n{why}"), Badge::None, Some(3.0)),
        driver::JobResult::Cancelled => say("이동을 취소했어요", Badge::None, Some(2.0)),
        driver::JobResult::Scanned { .. } => {}
        driver::JobResult::Failed(e) => say(&format!("이동하지 못했어요\n{e}"), Badge::None, Some(5.0)),
    });
}

/// 단계별 인식 알림 문구, `hk`는 멈추는 단축키 이름
fn scan_status(s: driver::ScanStage, hk: &str) -> String {
    let head = match s {
        driver::ScanStage::Sorting => "곡 제목순 정렬 확인 중".to_string(),
        // 내림, 끝을 찾기 전엔 100 %가 안 나오게
        driver::ScanStage::Reading(_) => format!("곡 목록 인식 중 {}%", (s.fill() * 100.0) as u32),
        driver::ScanStage::FindingEnd => "목록 끝 확인 중".to_string(),
        driver::ScanStage::Rechecking(done, of) => format!("다시 확인 중 {done}/{of}"),
    };
    format!("{head}\n만지면 혼나요! 멈추려면 {hk}")
}

fn on_scan(a: &mut App) {
    let Some(ctx) = hand_over(a) else { return };
    let (stale, hk) = (a.stale.clone(), a.popup.hotkey.label());
    a.toast.show(&scan_status(driver::ScanStage::Reading(0.0), &hk), Badge::Progress(0.0), None);
    spawn_job(a, move |cancel, say| {
        let r = driver::run_scan(&ctx, cancel, |s| say(&scan_status(s, &hk), Badge::Progress(s.fill()), None));
        match r {
            driver::JobResult::Scanned { entries, unknown, secs } => {
                stale.store(false, Ordering::Relaxed);
                let extra = if unknown > 0 { format!(" (확인 못 한 곡 {unknown}곡)") } else { String::new() };
                say(&format!("인식 완료!\n총 {entries}곡, {secs:.0}초 걸렸어요{extra}"), Badge::Done, Some(3.0));
            }
            driver::JobResult::Touched(why) => say(&format!("만지지 말랬잖아요!\n{why}"), Badge::Failed, Some(4.0)),
            driver::JobResult::Cancelled => say("인식을 취소했어요\n저장된 목록은 그대로예요", Badge::None, Some(3.0)),
            driver::JobResult::Failed(e) => say(&format!("인식을 멈췄어요\n{e}"), Badge::Failed, Some(5.0)),
            _ => {}
        }
    });
}

/// 검색창이 돌려준 동작 처리, 그 뒤 검색창을 다시 그림
fn on_action(a: &mut App, act: Action) {
    match act {
        Action::Pick(id, chart) => on_pick(a, id, chart, None),
        Action::PlaylistPick(id, pattern) => on_pick(a, pattern.title, Some((pattern.mode, pattern.diff)), Some(id)),
        Action::Scan => on_scan(a),
        Action::Retry => {
            let busy = jobs_busy(a);
            let refs = a.refs.lock().ok().and_then(|slot| slot.clone());
            a.updates.retry(busy, refs);
            sync_notices(a);
        }
        // 브라우저가 포커스를 가져가면 검색창은 평소처럼 닫힘
        Action::Release => {
            if let Err(e) = a.updates.open_page() {
                eprintln!("release page: {e}");
            }
        }
        Action::Hide => a.popup.hide(),
        Action::Choice => {
            save_settings(a);
            let _ = a.popup.render();
        }
        Action::Hotkey(hk) => {
            let ok = change_hotkey(a, hk);
            a.popup.hotkey_result(hk, ok);
            if ok {
                save_settings(a);
            }
            let _ = a.popup.render();
        }
        Action::None => {
            if a.popup.visible {
                if let Err(e) = a.popup.render() {
                    eprintln!("render error: {e:?}");
                }
            }
        }
    }
    if !a.popup.is_open() { a.updates.closed(); }
    request_playlist(a);
}

/// 검색창이 플레이리스트 주소를 검색했으면 작업 스레드로 받아 오기 시작
/// 검색이 일어날 수 있는 곳 (동작 처리, 검색창 열기) 뒤마다 부름
fn request_playlist(a: &mut App) {
    if let Some(id) = a.popup.take_playlist_request() {
        a.playlists.request(id, a.popup.hwnd);
    }
}

fn jobs_busy(a: &App) -> bool {
    // 화면 확인 스레드가 도는 동안은 늘 `peeking`이라 스레드는 따로 안 봄
    a.peeking
        || a.job.as_ref().is_some_and(|(_, h)| !h.is_finished())
        || a.retired_jobs.iter().any(|h| !h.is_finished())
        || a.initial_refs.is_some()
}

fn reap_jobs(a: &mut App) {
    if a.job.as_ref().is_some_and(|(_, h)| h.is_finished()) {
        if let Some((_, handle)) = a.job.take() { let _ = handle.join(); }
    }
    let mut pending = Vec::new();
    for handle in a.retired_jobs.drain(..) {
        if handle.is_finished() { let _ = handle.join(); } else { pending.push(handle); }
    }
    a.retired_jobs = pending;
    if a.peek_job.as_ref().is_some_and(|h| h.is_finished()) {
        if let Some(handle) = a.peek_job.take() { let _ = handle.join(); }
    }
    if a.initial_refs.as_ref().is_some_and(|h| h.is_finished()) {
        if let Some(handle) = a.initial_refs.take() {
            if let Ok(startup) = handle.join() {
                if let Some(refs) = a.updates.initial_loaded(startup) {
                    if let Ok(mut slot) = a.refs.lock() { *slot = Some(refs); }
                }
            }
        }
    }
}

/// 지금 교체하면 Some(열린 검색창에 바로 넣음)
/// 받아 둔 데이터가 없던 첫 받기는 열린 검색창에도 바로 넣음, 그 밖엔 검색창이 닫힌 뒤 교체
fn apply_mode(had_data: bool, visible: bool, open: bool, busy: bool) -> Option<bool> {
    let fill = !had_data && open;
    (!busy && (!visible || fill)).then_some(fill)
}

/// 지금 바꿔도 되면 받아 둔 새 곡 데이터로 교체
fn apply_ready(a: &mut App) {
    let had_data = a.updates.active.has_data();
    let busy = jobs_busy(a);
    let Some(fill) = apply_mode(had_data, a.popup.visible, a.popup.is_open(), busy) else { return; };
    let Some(ready) = a.updates.take_ready() else { return; };
    let changed = ready.changed_jackets(&a.updates.active);
    let update::Ready { catalog, active, refs, titles, summary } = ready;
    let fill_with = if fill {
        Some(catalog)
    } else if let Err(catalog) = a.popup.replace_catalog(catalog, &changed) {
        a.updates.keep_ready(update::Ready { catalog: *catalog, active, refs, titles, summary });
        return;
    } else {
        None
    };
    if let Ok(mut slot) = a.refs.lock() { *slot = active.has_data().then_some(refs); }
    a.titles = titles;
    if !had_data && active.has_data() {
        driver::ensure_both(&a.store, &|id| a.titles.get(&id).cloned().unwrap_or_default());
    }
    if let Some(catalog) = fill_with {
        let owned = refill_owned(&a.store, a.open_kind, &a.titles);
        a.popup.fill_catalog(catalog, owned);
    }
    a.updates.applied(active, &summary);
    // 치던 검색어가 플레이리스트 주소면 새 목록으로 다시 받음
    if fill {
        request_playlist(a);
    }
}

/// 열린 검색창에 첫 데이터를 넣을 때 고를 수 있는 곡
/// 열 때는 제목이 없어 모르는 칸에 있을 수 있는 곡이 빠졌음, 새 제목과 이번 열기의 화면으로 다시 계산
fn refill_owned(store: &std::path::Path, kind: Option<ScreenKind>, titles: &HashMap<u32, String>) -> Option<HashSet<u32>> {
    kind.and_then(|k| driver::searchable(store, k, titles))
}

fn update_tick(a: &mut App) {
    reap_jobs(a);
    // 닫히는 애니메이션 중에도 검색창에서 시작한 받기는 멈춤
    if !a.popup.is_open() { a.updates.closed(); }
    a.updates.poll();
    apply_ready(a);
    start_update(a);
    sync_notices(a);
}

/// 형식 안내는 실제로 그린 뒤에만 기록
fn sync_notices(a: &mut App) {
    if a.popup.take_schema_seen() {
        a.updates.schema_notice_shown();
    }
    let version = a.updates.exe_notice.as_ref().map(|n| (n.text.clone(), n.schema.is_some()));
    if a.popup.set_updates(a.updates.data_notice, version, a.updates.status_line()) && a.popup.visible {
        if let Err(e) = a.popup.render() {
            eprintln!("render error: {e:?}");
        }
    }
}

fn start_update(a: &mut App) {
    let busy = jobs_busy(a);
    let refs = a.refs.lock().ok().and_then(|slot| slot.clone());
    a.updates.started(busy, refs);
}

fn update_opened(a: &mut App) {
    if !a.popup.is_open() { return; }
    reap_jobs(a);
    a.updates.poll();
    start_update(a);
    // 지난 열기에 보인 형식 안내를 먼저 기록해야 이번 안내에서 빠짐
    if a.popup.take_schema_seen() {
        a.updates.schema_notice_shown();
    }
    let busy = jobs_busy(a);
    let refs = a.refs.lock().ok().and_then(|slot| slot.clone());
    a.updates.opened(busy, refs);
    sync_notices(a);
}

fn show_update_notice(a: &mut App) {
    if !a.popup.is_open() || jobs_busy(a)
        || a.status.lock().map_or(true, |slot| slot.is_some()) { return; }
    if let Some(text) = a.updates.notice(a.popup.has_recognition()) {
        a.toast.show(&text, Badge::None, Some(4.0));
        a.updates.notice_shown();
    }
}

fn stop_updates(a: &mut App) {
    if let Some(handle) = a.initial_refs.take() { let _ = handle.join(); }
    a.updates.stop();
}

/// 데이터 작업은 끝날 때까지 기다림, 인식·이동은 누르던 키를 떼게 최대 1초만 기다림
fn stop_all(a: &mut App) {
    if let Some((cancel, _)) = &a.job { cancel.store(true, Ordering::Relaxed); }
    stop_updates(a);
    if let Some((_, handle)) = a.job.take() {
        let t0 = std::time::Instant::now();
        while !handle.is_finished() && t0.elapsed() < std::time::Duration::from_secs(1) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
    for handle in a.retired_jobs.drain(..) {
        if handle.is_finished() { let _ = handle.join(); }
    }
}

/// 트레이 메뉴 종료와 Windows 종료·로그오프: 돌던 작업을 취소하고 아이콘을 지운 뒤 메시지 루프를 끝냄
fn quit(a: &mut App) {
    stop_all(a);
    a.tray.remove();
    unsafe { PostQuitMessage(0) };
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(a) = app() {
        if hwnd == a.tray.hwnd {
            if let Some((x, y)) = a.tray.clicked(msg, wp, lp) {
                // 메뉴가 떠 있는 동안은 메시지 루프가 멈춰 닫기 애니메이션이 안 돎
                a.popup.hide_now();
                a.updates.closed();
                match a.tray.menu(x, y, a.popup.hotkey) {
                    Some(tray::Menu::ResetHotkey) => reset_hotkey(a),
                    Some(tray::Menu::Quit) => quit(a),
                    None => {}
                }
                return LRESULT(0);
            }
        }
        if msg == WM_SETCURSOR && hwnd == a.popup.hwnd && (lp.0 & 0xFFFF) as u32 == HTCLIENT && a.popup.wants_ibeam() {
            unsafe {
                SetCursor(LoadCursorW(None, IDC_IBEAM).ok());
            }
            return LRESULT(1);
        }
        if msg == WM_HOTKEY && wp.0 as i32 == HOTKEY_ID {
            if a.popup.is_open() && a.popup.waiting_hotkey() {
                // 새 단축키를 기다리는 중 지금 단축키를 누름: 검색창이 받은 조합으로 침
                let (mods, vk) = (HOT_KEY_MODIFIERS((lp.0 & 0xFFFF) as u32), VIRTUAL_KEY((lp.0 >> 16) as u16));
                let act = a.popup.take_hotkey(mods, vk);
                on_action(a, act);
                return LRESULT(0);
            }
            if let Some((c, _)) = a.job.as_ref().filter(|(_, h)| !h.is_finished()) {
                c.store(true, Ordering::Relaxed); // 단축키는 돌고 있는 작업을 취소함
                println!("CANCEL");
                return LRESULT(0);
            }
            if a.popup.is_open() {
                a.popup.hide();
                a.updates.closed();
            } else {
                request_open(a);
                request_playlist(a);
            }
            return LRESULT(0);
        }
        if msg == WM_TIMER && wp.0 == TIMER_FOREGROUND {
            sync_hotkey(a);
            a.popup.settle();
            a.toast.settle();
            update_tick(a);
            a.toast.tick();
            return LRESULT(0);
        }
        if msg == WM_APP_OPEN {
            a.peeking = false;
            if let Some(handle) = a.peek_job.take() { let _ = handle.join(); }
            let found = a.peeked.lock().unwrap().take();
            // 확인하는 동안 다른 창으로 넘어갔으면 (Alt+Tab, 트레이 메뉴) 버림
            let game_front = capture::find_game_window().is_some_and(input::is_foreground);
            if let (Some((kind, owned)), true) = (found, game_front) {
                a.open_kind = Some(kind);
                a.popup.show(owned, a.stale.load(Ordering::Relaxed));
                // Windows가 포커스를 안 주면 바로 숨김, 보이는데 글자는 게임으로 가는 일이 없게
                if unsafe { GetForegroundWindow() } != a.popup.hwnd {
                    eprintln!("open: no focus, hidden");
                    a.popup.hide_now();
                }
                update_opened(a);
                show_update_notice(a);
            }
            request_playlist(a);
            return LRESULT(0);
        }
        if msg == playlist::READY {
            while let Some(reply) = a.playlists.take() {
                a.popup.playlist_reply(reply);
            }
            on_action(a, Action::None);
            return LRESULT(0);
        }
        if msg == WM_APP_STATUS {
            if let Some(st) = a.status.lock().unwrap().take() {
                a.toast.show(&st.text, st.badge, st.secs);
            }
            return LRESULT(0);
        }
        if msg == WM_ENDSESSION && wp.0 != 0 {
            quit(a);
            return LRESULT(0);
        }
        if hwnd == a.popup.hwnd && a.popup.is_open() {
            if let Some((ret, act)) = a.popup.handle(msg, wp, lp) {
                on_action(a, act);
                return LRESULT(ret);
            }
        }
    }
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn main() -> Result<()> {
    capture::init_dpi_awareness();

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _mutex = CreateMutexW(None, true, w!("Local\\DmPicker"))?;
        if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
            eprintln!("DmPicker is already running");
            return Ok(());
        }

        let dir = match data_dir() {
            Ok(dir) => dir,
            Err(error) => {
                eprintln!("executable directory: {error}");
                MessageBoxW(None, w!("실행 파일 위치를 읽지 못했어요"), &HSTRING::from(APP_NAME), MB_OK | MB_ICONERROR);
                return Ok(());
            }
        };
        let mut startup = update::preview(&store_dir(), &dir);
        startup.catalog.load_keywords(store_dir().join("keywords.json"));
        println!("catalog: {} songs", startup.catalog.songs.len());

        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP;
        let hwnd = sys::create_popup(w!("DmPickerPopup"), APP_NAME, ex, Some(wndproc))?;
        let toast = Toast::new(Some(wndproc))?;
        let tray = tray::Tray::new(Some(wndproc))?;

        let titles = startup.catalog.titles();
        // 한쪽 모드에서 인식한 목록을 다른 모드에도 씀
        if startup.active.has_data() {
            driver::ensure_both(&store_dir(), &|id| titles.get(&id).cloned().unwrap_or_default());
        }
        let refs = Arc::new(Mutex::new(startup.refs.clone()));
        let initial_refs = startup.active.has_data().then(|| update::start_initial(store_dir(), dir.clone()));
        let updates = update::Updater::new(store_dir(), dir.clone(), &startup);
        let mut app_box = Box::new(App {
            popup: Popup::new(hwnd, startup.catalog),
            toast,
            tray,
            hotkey_on: false,
            refs,
            titles,
            store: store_dir(),
            status: Arc::new(Mutex::new(None)),
            job: None,
            retired_jobs: Vec::new(),
            peek_job: None,
            initial_refs,
            updates,
            stale: Arc::new(AtomicBool::new(false)),
            peeking: false,
            peeked: Arc::new(Mutex::new(None)),
            open_kind: None,
            playlists: playlist::Loader::default(),
        });
        load_settings(&mut app_box.popup);
        app_box.popup.load_history(store_dir());
        app_box.popup.warm_up();
        APP = Box::into_raw(app_box);
        SetTimer(Some(hwnd), TIMER_FOREGROUND, 250, None);
        if let Some(a) = app() {
            sync_hotkey(a);
            start_update(a);
            println!("ready: {} on the song list", a.popup.hotkey.label());
        }

        let mut msg = MSG::default();
        const FRAME: std::time::Duration = std::time::Duration::from_micros(1_000_000 / gfx::MAX_FPS as u64);
        let mut frame_start = std::time::Instant::now();
        loop {
            let animating = app().map(|a| a.popup.animating() || a.toast.animating()).unwrap_or(false);
            if animating {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        if let Some(a) = app() { stop_all(a); }
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if let Some(a) = app() {
                    if a.popup.visible && a.popup.animating() {
                        if let Err(e) = a.popup.render() {
                            eprintln!("render error: {e:?}");
                        }
                    }
                    if a.toast.animating() {
                        a.toast.frame();
                    }
                }
                // Present가 이미 프레임 vblank를 기다림 (Gfx::sync), 여긴 곧장 돌아올 때 (창이 가려짐) 헛돌지 않게만 함
                // 입력이 오면 바로 깨어남
                let spent = frame_start.elapsed();
                if spent < FRAME {
                    let ms = (FRAME - spent).as_millis() as u32;
                    MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT);
                }
                frame_start = std::time::Instant::now();
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if let Some(a) = app() { stop_all(a); }
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
