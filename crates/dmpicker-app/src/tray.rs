//! 작업 표시줄 알림 영역 아이콘: 왼쪽·오른쪽 클릭 메뉴로 시작 시 실행 켜고 끄기, 종료
//! 보이는 창이 없는 앱이라 끄는 곳이 여기뿐

use std::os::windows::ffi::OsStrExt;

use windows::core::{w, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_ANY};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hotkey::Hotkey;
use crate::{sys, APP_NAME};

/// 아이콘 클릭이 오는 메시지 (main.rs의 WM_APP 메시지와 안 겹치게)
const WM_TRAY: u32 = WM_APP + 10;
const ID_AUTOSTART: u32 = 1;
const ID_QUIT: u32 = 2;
const ID_RESET_HOTKEY: u32 = 3;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// 아이콘 그림, 배율별 크기 (16 = 100 %, 24 = 150 %), 파일만 바꾸면 됨
/// 맞는 크기가 없으면 그보다 큰 것 중 가장 작은 걸 줄여 씀 (더 크면 48을 늘림)
static ICONS: [(i32, &[u8]); 5] = [
    (16, include_bytes!("../assets/tray/16.png")),
    (20, include_bytes!("../assets/tray/20.png")),
    (24, include_bytes!("../assets/tray/24.png")),
    (32, include_bytes!("../assets/tray/32.png")),
    (48, include_bytes!("../assets/tray/48.png")),
];

/// 트레이 메뉴에서 고른, 앱이 할 일
pub enum Menu {
    ResetHotkey,
    Quit,
}

pub struct Tray {
    /// 아이콘 메시지를 받는 숨은 창 (탐색기가 다시 시작될 때 오는 방송도 받게 최상위 창)
    pub hwnd: HWND,
    taskbar_created: u32,
    icon: HICON,
}

pub fn taskbar_created() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

impl Tray {
    pub fn new(wndproc: WNDPROC) -> Result<Self> {
        let hwnd = sys::create_popup(w!("DmPickerTray"), APP_NAME, WS_EX_TOOLWINDOW, wndproc)?;
        let taskbar_created = taskbar_created();
        let tray = Self { hwnd, taskbar_created, icon: load_icon().unwrap_or_default() };
        tray.add();
        Ok(tray)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: 1, ..Default::default() }
    }

    /// 아이콘을 올림, 탐색기가 아직 안 떴으면 실패하고 TaskbarCreated 때 다시 올림
    fn add(&self) {
        let mut nid = self.data();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icon;
        for (d, c) in nid.szTip.iter_mut().zip(APP_NAME.encode_utf16()) {
            *d = c;
        }
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
            } else {
                eprintln!("tray: add failed");
            }
        }
    }

    pub fn remove(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
        }
    }

    /// 메뉴를 띄울 클릭이면 그 자리 (화면 좌표)
    /// 탐색기가 다시 시작됐으면 아이콘을 다시 올림
    pub fn clicked(&self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<(i32, i32)> {
        if msg == self.taskbar_created {
            self.add();
            return None;
        }
        if msg != WM_TRAY {
            return None;
        }
        // 버전 4: lParam 아래 16비트가 알림, wParam이 아이콘 자리
        match (lp.0 & 0xFFFF) as u32 {
            NIN_SELECT | WM_CONTEXTMENU => Some((wp.0 as i16 as i32, (wp.0 >> 16) as i16 as i32)),
            _ => None,
        }
    }

    /// 메뉴를 띄우고 시작 시 실행은 여기서 처리, 앱이 할 항목을 고르면 그 항목
    /// `hotkey`가 기본값이 아니면 되돌리기 항목을 켬 (다른 프로그램이 단축키를 가져가면 빠져나올 길)
    pub fn menu(hwnd: HWND, x: i32, y: i32, hotkey: Hotkey) -> Option<Menu> {
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            let on = autostart_on();
            let check = if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | check, ID_AUTOSTART as usize, w!("Windows 시작 시 실행"));
            let reset = HSTRING::from(format!("단축키를 {}로 되돌리기", Hotkey::DEFAULT.label()));
            let grayed = if hotkey == Hotkey::DEFAULT { MF_GRAYED } else { MF_ENABLED };
            let _ = AppendMenuW(menu, MF_STRING | grayed, ID_RESET_HOTKEY as usize, &reset);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, w!("종료"));
            // 앞으로 가져와야 메뉴 밖을 눌렀을 때 닫히고, 닫은 뒤 WM_NULL을 보내야 다음 클릭에 바로 뜸
            let _ = SetForegroundWindow(hwnd);
            let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | align;
            let cmd = TrackPopupMenuEx(menu, flags.0, x, y, hwnd, None).0 as u32;
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            match cmd {
                ID_AUTOSTART => {
                    set_autostart(!on);
                    None
                }
                ID_RESET_HOTKEY => Some(Menu::ResetHotkey),
                ID_QUIT => Some(Menu::Quit),
                _ => None,
            }
        }
    }
}

/// 작업 표시줄 배율에 맞는 크기로 만든 아이콘
fn load_icon() -> Option<HICON> {
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
    let (_, png) = ICONS.iter().find(|(s, _)| *s >= size).unwrap_or(&ICONS[ICONS.len() - 1]);
    unsafe { CreateIconFromResourceEx(png, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR) }
        .inspect_err(|e| eprintln!("tray: icon {e:?}"))
        .ok()
}

/// 시작 시 실행 값이 있는지 (가리키는 경로는 안 봄)
fn autostart_on() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, &HSTRING::from(APP_NAME), RRF_RT_ANY, None, None, None) }.is_ok()
}

/// 시작 시 실행 값을 따옴표로 감싼 지금 exe 경로로 쓰거나 지움
fn set_autostart(on: bool) {
    let name = HSTRING::from(APP_NAME);
    let r = if on {
        let Ok(exe) = std::env::current_exe() else { return };
        let quote = [u16::from(b'"')];
        let v: Vec<u16> = quote.iter().copied().chain(exe.as_os_str().encode_wide()).chain(quote).chain([0]).collect();
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, &name, REG_SZ.0, Some(v.as_ptr().cast()), (v.len() * 2) as u32) }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, &name) }
    };
    if r.is_err() {
        eprintln!("autostart: {r:?}");
    }
}
