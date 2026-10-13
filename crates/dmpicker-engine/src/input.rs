//! 게임용 scan code 키 입력 (Windows 전용), 게임이 포그라운드 창일 때만 보냄
//!
//! [`Key`]에는 선곡 화면에서 눌러도 안전한 키만 있음
//! Enter(곡 시작, 온라인에선 채팅), Esc(메뉴), F5(온라인 준비/시작), F7, F10(설정),
//! Space(온라인 옵션), 숫자(온라인 이모트), Alt(창 메뉴)는 아예 표현 불가
//! 정렬 창(F4)은 Enter로 적용하고 Esc로 닫음
//! 이 둘은 따로 [`PopupKey`]로 두고 정렬 창이 화면에 보인 뒤에만 [`press_popup`]으로 누름
//!
//! 키 입력 규칙
//! * 게임이 실제 포그라운드 창일 때만 보냄, 키를 누르기 전과 떼기 전에 매번 확인
//!   포커스가 없으면 [`FocusLost`] 반환, 호출 쪽은 남은 키를 버리고 전체 동작 중단 (자동 재개 없음)
//!   이미 누른 키는 그래도 떼서 시스템 키보드 상태에 눌린 채로 남지 않게 함
//! * 게임은 프레임마다 한 번 키 상태를 읽어서 누름과 뗌을 각각 1프레임 이상 유지해야 함
//!   ~40 ms면 ~25 fps까지 안전, 60 fps가 안정적이면 17 ms도 됨
//! * 가짜 포커스 메시지는 절대 안 보냄

use std::fmt;
use std::thread::sleep;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

use crate::capture::Hwnd;

/// 조합 키를 하나씩 누르고 뗄 때 사이 간격
pub const CHORD_STAGGER_MS: u64 = 10;

/// 도우미가 누를 수 있는 키, scan code는 PC/AT set 1
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    /// 이전 곡
    Up,
    /// 다음 곡
    Down,
    /// 이전 난이도
    Left,
    /// 다음 난이도
    Right,
    /// 이전 그룹의 마지막 곡
    PageUp,
    /// 다음 그룹의 첫 곡
    PageDown,
    /// 이전 탭 (RShift와 같이 누르면 커서가 목록 처음으로)
    LShift,
    /// 다음 탭
    RShift,
    /// Ctrl+Numpad 버튼 모드 선택용
    LCtrl,
    /// LCtrl과 같이 누르면 4B
    Numpad4,
    /// LCtrl과 같이 누르면 5B
    Numpad5,
    /// LCtrl과 같이 누르면 6B
    Numpad6,
    /// LCtrl과 같이 누르면 8B
    Numpad8,
    /// 버튼 모드를 4B, 5B, 6B, 8B 순서로 돌림
    Tab,
    /// 정렬 창
    F4,
}

impl Key {
    /// A..Z 순서
    pub const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];

    /// `c`에 해당하는 글자 키 (ASCII A-Z, 대소문자 무시)
    pub fn letter(c: char) -> Option<Key> {
        let u = c.to_ascii_uppercase();
        u.is_ascii_uppercase()
            .then(|| Key::LETTERS[(u as u8 - b'A') as usize])
    }

    /// Set-1 기준 scan code
    pub const fn scan(self) -> u16 {
        match self {
            Key::Q => 0x10,
            Key::W => 0x11,
            Key::E => 0x12,
            Key::R => 0x13,
            Key::T => 0x14,
            Key::Y => 0x15,
            Key::U => 0x16,
            Key::I => 0x17,
            Key::O => 0x18,
            Key::P => 0x19,
            Key::A => 0x1E,
            Key::S => 0x1F,
            Key::D => 0x20,
            Key::F => 0x21,
            Key::G => 0x22,
            Key::H => 0x23,
            Key::J => 0x24,
            Key::K => 0x25,
            Key::L => 0x26,
            Key::Z => 0x2C,
            Key::X => 0x2D,
            Key::C => 0x2E,
            Key::V => 0x2F,
            Key::B => 0x30,
            Key::N => 0x31,
            Key::M => 0x32,
            Key::Up => 0x48,
            Key::Down => 0x50,
            Key::Left => 0x4B,
            Key::Right => 0x4D,
            Key::PageUp => 0x49,
            Key::PageDown => 0x51,
            Key::LShift => 0x2A,
            Key::RShift => 0x36,
            Key::LCtrl => 0x1D,
            Key::Numpad4 => 0x4B,
            Key::Numpad5 => 0x4C,
            Key::Numpad6 => 0x4D,
            Key::Numpad8 => 0x48,
            Key::Tab => 0x0F,
            Key::F4 => 0x3E,
        }
    }

    /// E0 접두(`KEYEVENTF_EXTENDEDKEY`)가 붙는 키, 방향키와 PgUp/PgDn만 (숫자패드 키는 아님)
    pub const fn extended(self) -> bool {
        matches!(
            self,
            Key::Up | Key::Down | Key::Left | Key::Right | Key::PageUp | Key::PageDown
        )
    }
}

/// 정렬 창에서 쓰는 키, 다른 데서 누르면 위험함
/// 곡 목록에서 Enter는 곡 시작(온라인에선 채팅 열림), Esc는 메뉴 열림
/// 정렬 창이 화면에 있을 때만 누름 ([`crate::sort::highlighted`]로 확인)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PopupKey {
    /// 강조된 정렬 순서 적용
    Enter,
    /// 안 바꾸고 닫기
    Esc,
}

impl PopupKey {
    /// Set-1 scan code (숫자패드 말고 본 Enter)
    pub const fn scan(self) -> u16 {
        match self {
            PopupKey::Enter => 0x1C,
            PopupKey::Esc => 0x01,
        }
    }
}

/// 게임이 포그라운드 창이 아니거나 아니게 됨, 동작 중단
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusLost;

impl fmt::Display for FocusLost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "game window lost focus; key sequence aborted")
    }
}

impl std::error::Error for FocusLost {}

/// 게임 창이 실제 포그라운드 창인지
pub fn is_foreground(game: Hwnd) -> bool {
    unsafe { GetForegroundWindow() == game.raw() }
}

/// 게임을 포그라운드로 올려 달라고 Windows에 요청 (SetForegroundWindow만, 가짜 포커스 메시지 없음)
/// Windows가 거절할 수 있어서 뒤에 [`is_foreground`]로 확인해야 함
pub fn request_foreground(game: Hwnd) -> bool {
    unsafe { SetForegroundWindow(game.raw()).as_bool() }
}

/// 키 이벤트의 `KEYBDINPUT` 플래그
pub const fn key_flags(key: Key, up: bool) -> u32 {
    let mut f = KEYEVENTF_SCANCODE.0;
    if key.extended() {
        f |= KEYEVENTF_EXTENDEDKEY.0;
    }
    if up {
        f |= KEYEVENTF_KEYUP.0;
    }
    f
}

/// 정렬 창 키 이벤트의 `KEYBDINPUT` 플래그 (둘 다 확장 키 아님)
pub const fn popup_flags(up: bool) -> u32 {
    if up {
        KEYEVENTF_SCANCODE.0 | KEYEVENTF_KEYUP.0
    } else {
        KEYEVENTF_SCANCODE.0
    }
}

fn key_event(scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: KEYBD_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_scan(scan: u16, flags: u32) {
    let ev = [key_event(scan, flags)];
    unsafe {
        SendInput(&ev, std::mem::size_of::<INPUT>() as i32);
    }
}

fn send(key: Key, up: bool) {
    send_scan(key.scan(), key_flags(key, up));
}

fn ms(v: u64) {
    if v > 0 {
        sleep(Duration::from_millis(v));
    }
}

/// 키 하나 누르고 뗌 (포커스 확인, 누름, `hold_ms` 유지, 포커스 확인, 뗌, `gap_ms` 대기)
pub fn press(game: Hwnd, key: Key, hold_ms: u64, gap_ms: u64) -> Result<(), FocusLost> {
    tap(game, |up| send(key, up), hold_ms, gap_ms)
}

/// 정렬 창 키용 [`press`]
/// 호출 쪽이 방금 화면에서 정렬 창을 확인했어야 함, 이 키들은 곡 목록에서도 먹힘
pub fn press_popup(game: Hwnd, key: PopupKey, hold_ms: u64, gap_ms: u64) -> Result<(), FocusLost> {
    tap(
        game,
        |up| send_scan(key.scan(), popup_flags(up)),
        hold_ms,
        gap_ms,
    )
}

fn tap(game: Hwnd, send: impl Fn(bool), hold_ms: u64, gap_ms: u64) -> Result<(), FocusLost> {
    if !is_foreground(game) {
        return Err(FocusLost);
    }
    send(false);
    ms(hold_ms);
    let focused = is_foreground(game);
    // 누른 키는 항상 뗌, 뗌을 빠뜨리면 시스템 전체에서 키가 눌린 채로 남음
    send(true);
    if !focused {
        return Err(FocusLost);
    }
    ms(gap_ms);
    Ok(())
}

/// 조합 키, 순서대로 누름(10 ms 간격), `hold_ms` 유지, 역순으로 뗌, `gap_ms` 대기
/// 키마다 누르기 전, 전부 떼기 전에 한 번 포커스 확인, 이미 누른 키는 항상 뗌
pub fn combo(game: Hwnd, keys: &[Key], hold_ms: u64, gap_ms: u64) -> Result<(), FocusLost> {
    let mut down: Vec<Key> = Vec::with_capacity(keys.len());
    let release = |down: &[Key]| {
        for (i, &k) in down.iter().rev().enumerate() {
            if i > 0 {
                ms(CHORD_STAGGER_MS);
            }
            send(k, true);
        }
    };
    for (i, &k) in keys.iter().enumerate() {
        if i > 0 {
            ms(CHORD_STAGGER_MS);
        }
        if !is_foreground(game) {
            release(&down);
            return Err(FocusLost);
        }
        send(k, false);
        down.push(k);
    }
    ms(hold_ms);
    let focused = is_foreground(game);
    release(&down);
    if !focused {
        return Err(FocusLost);
    }
    ms(gap_ms);
    Ok(())
}
