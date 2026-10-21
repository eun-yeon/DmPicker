//! 창 도우미 (게임 창 찾기, 우리 팝업 창), DPI 도우미, 앱 파일 저장
//! 창 수준 API만 씀, 안티치트 때문에 게임 프로세스엔 접근 안 함

use std::path::Path;
use std::sync::OnceLock;

use windows::core::{s, w, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::UI::Accessibility::{HIGHCONTRASTW, HCF_HIGHCONTRASTON};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

/// 유리 효과를 켤 수 있는 윈도우 11 (빌드 22000 이상), 처음 한 번만 확인함
pub fn glass_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| unsafe {
        // 매니페스트에 따라 달라지지 않는 실제 빌드 번호가 필요함
        type Version = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
        let Ok(lib) = GetModuleHandleW(w!("ntdll.dll")) else { return false };
        let Some(found) = GetProcAddress(lib, s!("RtlGetVersion")) else { return false };
        let version: Version = std::mem::transmute(found);
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        version(&mut info) >= 0 && info.dwBuildNumber >= 22000
    })
}

/// 유리 효과를 쉬는 이유, 설정 시트에 켜는 법을 다르게 안내함
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassPause {
    Transparency,
    Power,
    /// 원격 세션, 고대비
    Unavailable,
}

/// 윈도우가 투명 효과를 끄는 상황이면 그 이유, 그동안 유리 효과도 잠시 끔
/// (원격 세션, 투명 효과 끔, 절전 모드, 고대비)
pub fn glass_paused() -> Option<GlassPause> {
    unsafe {
        if GetSystemMetrics(SM_REMOTESESSION) != 0 {
            return Some(GlassPause::Unavailable);
        }
        let mut transparency = 1u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("EnableTransparency"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut transparency as *mut _ as *mut _),
            Some(&mut size),
        ).is_ok() && transparency == 0 {
            return Some(GlassPause::Transparency);
        }
        let mut power = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut power).is_ok() && power.SystemStatusFlag == 1 {
            return Some(GlassPause::Power);
        }
        let mut contrast = HIGHCONTRASTW {
            cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        if SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.cbSize, Some(&mut contrast as *mut _ as *mut _), Default::default()).is_ok()
            && contrast.dwFlags.contains(HCF_HIGHCONTRASTON)
        {
            return Some(GlassPause::Unavailable);
        }
        None
    }
}

pub fn find_game() -> Option<HWND> {
    dmpicker_engine::capture::find_game_window().map(|h| h.raw())
}

/// `class`를 등록하고 그 클래스로 테두리 없는 최상위 창을 숨긴 채 만듦
pub fn create_popup(class: PCWSTR, title: &str, ex_style: WINDOW_EX_STYLE, wndproc: WNDPROC) -> Result<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: wndproc,
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(ex_style, class, &HSTRING::from(title), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)
    }
}

/// 클립보드의 유니코드 글 형식
const CF_UNICODETEXT: u32 = 13;

/// 클립보드의 글, 없거나 못 열면 None
pub fn clipboard_text(owner: HWND) -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            // 끝 NUL은 할당 크기 안에서만 찾음
            let cap = GlobalSize(g) / 2;
            let n = (0..cap).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
}

/// 클립보드에 글을 넣음, 넣었으면 true (다른 프로그램이 잡고 있으면 false)
pub fn set_clipboard_text(owner: HWND, text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let mut done = false;
        if EmptyClipboard().is_ok() {
            if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
                let p = GlobalLock(g) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                    let _ = GlobalUnlock(g);
                    // 넘긴 메모리는 클립보드가 가짐
                    done = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0))).is_ok();
                }
                if !done {
                    let _ = GlobalFree(Some(g));
                }
            }
        }
        let _ = CloseClipboard();
        done
    }
}

/// 게임 창의 클라이언트 영역, 화면 (물리 픽셀) 좌표, 게임이 없으면 None
pub fn game_rect() -> Option<RECT> {
    let h = find_game()?;
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(h, &mut rc).ok()?;
        let mut p = POINT { x: 0, y: 0 };
        if !ClientToScreen(h, &mut p).as_bool() {
            return None;
        }
        Some(RECT { left: p.x, top: p.y, right: p.x + rc.right, bottom: p.y + rc.bottom })
    }
}

pub fn monitor_for_rect(rc: &RECT) -> HMONITOR {
    unsafe { MonitorFromRect(rc, MONITOR_DEFAULTTONEAREST) }
}

pub fn monitor_work_area(m: HMONITOR) -> RECT {
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        mi.rcWork
    }
}

/// 게임이 없을 때 검색창·알림을 띄울 곳, 커서가 있는 모니터의 작업 영역
pub fn cursor_work_area() -> RECT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    let rc = RECT { left: p.x, top: p.y, right: p.x + 1, bottom: p.y + 1 };
    monitor_work_area(monitor_for_rect(&rc))
}

/// `hwnd`가 있는 모니터의 주사율 (모르면 60)
pub fn refresh_hz(hwnd: HWND) -> u32 {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        let ok = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi as *mut _ as *mut MONITORINFO).as_bool()
            && EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool();
        if ok && dm.dmDisplayFrequency > 1 { dm.dmDisplayFrequency } else { 60 }
    }
}

fn monitor_scale(m: HMONITOR) -> f32 {
    unsafe {
        let (mut x, mut y) = (96u32, 96u32);
        if GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_ok() { x as f32 / 96.0 } else { 1.0 }
    }
}

/// `rc`에 띄울 검색창·알림 배율, `game`이면 `rc`는 게임 클라이언트 영역
/// 게임은 UI를 화면 높이에 비례해 키움 (16:9보다 세로로 길면 폭에 맞춘 16:9 화면의 높이)
/// 그래서 게임 위에선 FHD에서 보던 비율대로 그 높이 / 1080, 다만 Windows 배율보다 작게는 안 함
pub fn ui_scale(rc: &RECT, game: bool) -> f32 {
    let dpi = monitor_scale(monitor_for_rect(rc));
    if !game {
        return dpi;
    }
    let (w, h) = ((rc.right - rc.left) as f32, (rc.bottom - rc.top) as f32);
    dpi.max(h.min(w * 9.0 / 16.0) / 1080.0)
}

/// 스크린샷·녹화 도구, 이 중 하나가 포커스를 가져가도 검색창은 안 닫힘
const CAPTURE_TOOLS: &[&str] = &[
    "snippingtool.exe",       // Win+Shift+S, 캡처 도구 (Windows 11)
    "screenclippinghost.exe", // Win+Shift+S (Windows 10)
    "screensketch.exe",
    "sharex.exe",
    "lightshot.exe",
    "greenshot.exe",
    "picpick.exe",
    "snagit32.exe",
    "snagiteditor.exe",
    "gyazo.exe",
    "flameshot.exe",
    "gamebar.exe",
    "gamebarftserver.exe",
    "obs64.exe",
    "obs32.exe",
    "bdcam.exe",
    "nvidia overlay.exe",
    "nvidia share.exe",
];

/// `hwnd`가 알려진 캡처 도구 창이면 true
/// 안티치트 때문에 게임 프로세스는 절대 안 열고, 프로세스 id로 먼저 걸러냄
pub fn is_capture_tool(hwnd: HWND) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return false;
        }
        if let Some(game) = find_game() {
            let mut gpid = 0u32;
            GetWindowThreadProcessId(game, Some(&mut gpid));
            if gpid == pid {
                return false;
            }
        }
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return false;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        let exe = path.rsplit('\\').next().unwrap_or("");
        CAPTURE_TOOLS.contains(&exe)
    }
}

/// 임시 파일에 다 쓰고 디스크에 내린 뒤 바꿔 끼움, 쓰다 꺼져도 원래 파일이 남음
pub fn save_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    dmpicker_update::store::atomic_write(path, bytes).map_err(std::io::Error::other)
}
