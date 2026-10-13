//! 이동하는 동안 게임 입력기를 영문으로 두고, 끝나면 (취소·실패·앱 종료 포함) 원래대로
//! 보내는 글자 키도 입력기를 거쳐서 한글이면 조합으로 바뀌어 첫 글자 건너뛰기가 안 됨
//! 바꾸기 전에 원래 값을 파일에 적어 둬서 앱이 강제로 꺼져도 다음 실행 때 되돌림

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dmpicker_engine::capture::Hwnd;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::Ime::{ImmGetDefaultIMEWnd, IMC_SETCONVERSIONMODE, IMC_SETOPENSTATUS, IME_CMODE_NATIVE};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_IME_CONTROL};

/// windows 크레이트엔 없는 두 값
const IMC_GETCONVERSIONMODE: u32 = 1;
const IMC_GETOPENSTATUS: u32 = 5;
const NATIVE: usize = IME_CMODE_NATIVE.0 as usize;
const RECORD: &str = "ime-restore.json";
/// 되돌리기 전 기다림, 입력기 메시지는 줄 서 있는 키보다 먼저 처리돼서 마지막 글자 키가 한글로 바뀔 수 있음
const SETTLE: Duration = Duration::from_millis(200);

/// 바꾸기 전 게임 입력기 상태, 되돌릴 값
struct Switch {
    owner: u64,
    game: isize,
    pid: u32,
    ime: isize,
    open: usize,
    mode: usize,
}

static ACTIVE: Mutex<Option<Switch>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

/// 살아 있는 동안 게임 입력기를 영문으로 둠, `id` 0이면 바꾼 게 없음
pub struct LatinInput {
    id: u64,
}

impl LatinInput {
    /// 한글이면 영문으로 바꿈, 영문이거나 못 읽거나 이미 취소된 작업이면 그대로
    pub fn new(game: Hwnd, cancel: &AtomicBool) -> Self {
        let mut active = lock();
        // 종료로 되돌린 뒤나 새 작업이 넘겨받은 뒤에 늦게 온 취소된 작업이 다시 바꾸면 안 됨
        if cancel.load(Ordering::Relaxed) {
            return Self { id: 0 };
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut pid = 0;
        let tid = unsafe { GetWindowThreadProcessId(game.raw(), Some(&mut pid)) };
        let ime = unsafe { ImmGetDefaultIMEWnd(game.raw()) };
        let hkl = unsafe { GetKeyboardLayout(tid) }.0 as isize;
        let now = state(ime);
        eprintln!("ime: game={:#x} ime={:#x} hkl={hkl:#x} now={now:?}", game.0, ime.0 as isize);
        let native = now.filter(|&(open, mode)| hangul(open, mode));
        // 이전 이동이 아직 안 끝남, 마지막 이동이 끝날 때 되돌리게 원래 값째로 넘겨받음
        if let Some(s) = active.as_mut().filter(|s| s.game == game.0 && same_game(s)) {
            if let Some((_, mode)) = native {
                control(ime, IMC_SETCONVERSIONMODE, mode & !NATIVE);
            }
            s.owner = id;
            return Self { id };
        }
        finish(&mut active);
        let Some((open, mode)) = native else {
            return Self { id: 0 };
        };
        let s = Switch { owner: id, game: game.0, pid, ime: ime.0 as isize, open, mode };
        if !save_record(&s) {
            return Self { id: 0 };
        }
        control(ime, IMC_SETCONVERSIONMODE, mode & !NATIVE);
        match state(ime) {
            Some((o, m)) if !hangul(o, m) => {
                eprintln!("ime: latin open={o} mode={m:#x}");
                *active = Some(s);
                Self { id }
            }
            other => {
                // 시간이 지나 처리될 수도 있어서 원래 값을 한 번 더 보냄
                // 안 닿았으면 기록을 남겨 다음 실행 때 되돌림
                if control(ime, IMC_SETCONVERSIONMODE, mode).is_some() {
                    remove_record();
                }
                eprintln!("ime: could not switch to latin, now {other:?}");
                Self { id: 0 }
            }
        }
    }
}

impl Drop for LatinInput {
    fn drop(&mut self) {
        if self.id == 0 || !owns(self.id) {
            return;
        }
        // 기다리는 사이 새 이동이 넘겨받았을 수 있어서 다시 확인
        std::thread::sleep(SETTLE);
        let mut active = lock();
        if active.as_ref().is_some_and(|s| s.owner == self.id) {
            finish(&mut active);
        }
    }
}

/// 앱 종료 때 기다려도 안 끝난 작업 대신 바로 되돌림
pub fn restore_now() {
    finish(&mut lock());
}

/// 패닉 훅용, 잠금을 누가 쥐고 있든 건너뜀 (기록 파일이 남아 다음 실행 때 되돌림)
pub fn restore_on_panic() {
    if let Ok(mut active) = ACTIVE.try_lock() {
        finish(&mut active);
    }
}

/// 앱을 켤 때, 강제로 꺼져서 못 되돌린 게 남았으면 같은 게임 창일 때만 되돌림
pub fn recover() {
    let path = crate::store_dir().join(RECORD);
    let Ok(raw) = std::fs::read(&path) else { return };
    let _ = std::fs::remove_file(&path);
    let Some(s) = parse_record(&raw) else { return };
    put_back(&s);
}

/// 창 핸들은 닫힌 뒤 다른 창에 다시 쓰일 수 있어서 pid·입력기 창까지 같아야 같은 게임
fn same_game(s: &Switch) -> bool {
    let game = HWND(s.game as _);
    let mut pid = 0;
    unsafe {
        IsWindow(Some(game)).as_bool()
            && GetWindowThreadProcessId(game, Some(&mut pid)) != 0
            && pid == s.pid
            && ImmGetDefaultIMEWnd(game).0 as isize == s.ime
    }
}

fn lock() -> MutexGuard<'static, Option<Switch>> {
    ACTIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn owns(id: u64) -> bool {
    lock().as_ref().is_some_and(|s| s.owner == id)
}

/// 되돌리는 메시지가 안 닿았으면 기록을 남겨 다음 실행 때 다시 시도
fn finish(active: &mut Option<Switch>) {
    if let Some(s) = active.take() {
        if put_back(&s) {
            remove_record();
        }
    }
}

/// 지금 한글로 읽히면 사용자가 되돌렸거나 그대로라 안 건드림, 아니면 원래 열림·변환 값을 씀
/// 못 읽어도 씀, 영문에 갇히는 것보다 원래 값으로 돌아가는 게 나음
/// 보낸 메시지가 게임에 안 닿았을 때만 false
fn put_back(s: &Switch) -> bool {
    if !same_game(s) {
        eprintln!("ime: restore skipped, game changed");
        return true;
    }
    let ime = HWND(s.ime as _);
    if state(ime).is_some_and(|(open, mode)| hangul(open, mode)) {
        eprintln!("ime: already hangul");
        return true;
    }
    control(ime, IMC_SETOPENSTATUS, s.open);
    let sent = control(ime, IMC_SETCONVERSIONMODE, s.mode).is_some();
    eprintln!("ime: restore requested open={} mode={:#x} sent={sent}", s.open, s.mode);
    sent
}

/// 입력기가 켜져 있고 한글 조합 모드
fn hangul(open: usize, mode: usize) -> bool {
    open != 0 && mode & NATIVE != 0
}

fn state(ime: HWND) -> Option<(usize, usize)> {
    Some((control(ime, IMC_GETOPENSTATUS, 0)?, control(ime, IMC_GETCONVERSIONMODE, 0)?))
}

/// 게임은 프레임마다 메시지를 처리해서 300 ms면 넉넉함
/// SMTO_BLOCK은 UI 스레드가 잠금을 쥔 채 기다리다 종료 알림을 처리해 같은 잠금을 다시 잡고 멈추는 걸 막음
fn control(ime: HWND, command: u32, value: usize) -> Option<usize> {
    if ime.is_invalid() {
        return None;
    }
    let mut result = 0;
    let sent = unsafe {
        SendMessageTimeoutW(ime, WM_IME_CONTROL, WPARAM(command as usize), LPARAM(value as isize), SMTO_BLOCK | SMTO_ABORTIFHUNG, 300, Some(&mut result))
    };
    (sent.0 != 0).then_some(result)
}

fn save_record(s: &Switch) -> bool {
    let v = json!({ "game": s.game, "pid": s.pid, "ime": s.ime, "open": s.open, "mode": s.mode });
    if let Err(e) = crate::sys::save_file(&crate::store_dir().join(RECORD), v.to_string().as_bytes()) {
        eprintln!("ime record: {e}");
        return false;
    }
    true
}

fn remove_record() {
    let _ = std::fs::remove_file(crate::store_dir().join(RECORD));
}

fn parse_record(raw: &[u8]) -> Option<Switch> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    Some(Switch {
        owner: 0,
        game: v["game"].as_i64()? as isize,
        pid: u32::try_from(v["pid"].as_u64()?).ok()?,
        ime: v["ime"].as_i64()? as isize,
        open: usize::try_from(v["open"].as_u64()?).ok()?,
        mode: usize::try_from(v["mode"].as_u64()?).ok()?,
    })
}
