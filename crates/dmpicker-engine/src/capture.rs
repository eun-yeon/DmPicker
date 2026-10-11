//! 게임 창 찾기와 프레임 캡처 (Windows 전용)
//!
//! 안티치트에 걸리지 않게 `FindWindowW`, `GetClientRect`/`ClientToScreen`, `PrintWindow`, GDI `BitBlt`만 씀
//! 프로세스 핸들은 절대 안 엶
//!
//! * 창 모드 / 테두리 없는 창: [`capture_window`] (`PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT)`)
//!   창이 가려져 있거나 포커스가 없어도 됨
//! * 전용 전체 화면(Full Screen): PrintWindow가 지난 프레임을 돌려줌
//!   대신 [`client_rect_screen`] 영역을 [`capture_screen_rect`]로 캡처
//!
//! 시작할 때 [`init_dpi_awareness`]를 한 번 불러야 클라이언트 크기가 실제 픽셀로 나옴
//! 인식용 [`capture`]는 16:9보다 세로로 긴 화면에서 위아래 여백을 빼고 가운데 16:9 띠만 돌려줌

use std::ffi::c_void;
use std::fmt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, ClientToScreen, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush,
    GetDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC,
    SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS, PW_CLIENTONLY};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetClientRect, IsIconic, IsWindow, PW_RENDERFULLCONTENT,
};

use crate::img::{Rect, Rgb};

/// 게임 클라이언트 창 제목
pub const GAME_TITLE: &str = "DJMAX RESPECT V";

/// `Send + Sync + Copy`인 창 핸들 (그냥 정수), `windows::HWND`로 바꿀 수 있음
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hwnd(pub isize);

impl Hwnd {
    /// 원래 `HWND`
    pub fn raw(self) -> HWND {
        HWND(self.0 as *mut c_void)
    }

    pub fn from_raw(h: HWND) -> Self {
        Hwnd(h.0 as isize)
    }

    /// 창이 아직 있는지
    pub fn is_window(self) -> bool {
        unsafe { IsWindow(Some(self.raw())).as_bool() }
    }

    /// 창이 최소화됐는지
    pub fn is_minimized(self) -> bool {
        unsafe { IsIconic(self.raw()).as_bool() }
    }
}

/// 캡처 실패
#[derive(Debug)]
pub enum CaptureError {
    /// 창이 없어짐
    WindowGone,
    /// 창이 최소화됨 (캡처할 게 없음)
    Minimized,
    /// 클라이언트 영역이나 캡처 사각형 크기가 0
    Empty,
    /// Win32 호출 실패
    Win32(&'static str, windows::core::Error),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::WindowGone => write!(f, "game window no longer exists"),
            CaptureError::Minimized => write!(f, "game window is minimized"),
            CaptureError::Empty => write!(f, "empty capture area"),
            CaptureError::Win32(what, e) => write!(f, "{what} failed: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// 프로세스를 모니터별 DPI 인식(v2)으로 설정해서 창 크기와 캡처를 실제 픽셀로 받음
/// 설정 후 per-monitor-v2 상태면 true (원래 그랬어도 true)
pub fn init_dpi_awareness() -> bool {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .as_bool()
    }
}

/// 제목이 정확히 맞는 게임 최상위 창 찾기
pub fn find_game_window() -> Option<Hwnd> {
    let h = unsafe { FindWindowW(PCWSTR::null(), w!("DJMAX RESPECT V")) }.ok()?;
    (!h.is_invalid()).then(|| Hwnd::from_raw(h))
}

/// 클라이언트 영역 크기, 픽셀 단위
pub fn client_size(hwnd: Hwnd) -> Option<(u32, u32)> {
    let mut rc = RECT::default();
    unsafe { GetClientRect(hwnd.raw(), &mut rc) }.ok()?;
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

/// 화면 좌표 기준 클라이언트 영역 ([`capture_screen_rect`]에 씀)
pub fn client_rect_screen(hwnd: Hwnd) -> Option<Rect> {
    let (w, h) = client_size(hwnd)?;
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(hwnd.raw(), &mut pt) }
        .as_bool()
        .then_some(Rect::new(pt.x, pt.y, w as i32, h as i32))
}

/// drop될 때 클로저 실행 (Win32 리소스 정리)
struct Defer<F: FnMut()>(F);
impl<F: FnMut()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// `ref_dc`와 호환되는 새 메모리 DC에 top-down 32bpp DIB section을 선택해 둠
/// 그다음 `draw(mem_dc)`를 돌리고 픽셀을 RGB로 돌려줌
unsafe fn with_dib(
    ref_dc: HDC,
    w: i32,
    h: i32,
    draw: impl FnOnce(HDC) -> Result<(), CaptureError>,
) -> Result<Rgb, CaptureError> {
    unsafe {
        let mdc = CreateCompatibleDC(Some(ref_dc));
        if mdc.is_invalid() {
            return Err(CaptureError::Win32(
                "CreateCompatibleDC",
                windows::core::Error::from_thread(),
            ));
        }
        let _mdc_guard = Defer(|| {
            let _ = DeleteDC(mdc);
        });
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 음수면 위쪽 행부터 저장됨
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(mdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| CaptureError::Win32("CreateDIBSection", e))?;
        let _bmp_guard = Defer(|| {
            let _ = DeleteObject(bmp.into());
        });
        if bits.is_null() {
            return Err(CaptureError::Win32(
                "CreateDIBSection",
                windows::core::Error::from_thread(),
            ));
        }
        let old = SelectObject(mdc, bmp.into());
        let _sel_guard = Defer(|| {
            SelectObject(mdc, old);
        });
        draw(mdc)?;
        let _ = GdiFlush();
        let stride = w as usize * 4;
        let px = std::slice::from_raw_parts(bits as *const u8, stride * h as usize);
        Ok(Rgb::from_bgra(w as usize, h as usize, stride, px))
    }
}

/// `PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT)`로 게임 클라이언트 영역 캡처
/// 창 모드 / 테두리 없는 창이면 창이 가려져 있거나 포커스가 없어도 됨
pub fn capture_window(hwnd: Hwnd) -> Result<Rgb, CaptureError> {
    if !hwnd.is_window() {
        return Err(CaptureError::WindowGone);
    }
    if hwnd.is_minimized() {
        return Err(CaptureError::Minimized);
    }
    let (w, h) = client_size(hwnd).ok_or(CaptureError::Empty)?;
    unsafe {
        let wdc = GetDC(Some(hwnd.raw()));
        if wdc.is_invalid() {
            return Err(CaptureError::Win32(
                "GetDC",
                windows::core::Error::from_thread(),
            ));
        }
        let _wdc_guard = Defer(|| {
            ReleaseDC(Some(hwnd.raw()), wdc);
        });
        with_dib(wdc, w as i32, h as i32, |mdc| {
            let flags = PRINT_WINDOW_FLAGS(PW_CLIENTONLY.0 | PW_RENDERFULLCONTENT);
            if PrintWindow(hwnd.raw(), mdc, flags).as_bool() {
                Ok(())
            } else {
                Err(CaptureError::Win32(
                    "PrintWindow",
                    windows::core::Error::from_thread(),
                ))
            }
        })
    }
}

/// 가상 화면의 사각형을 GDI `BitBlt`로 캡처, PrintWindow가 지난 프레임을 주는 전용 전체 화면용
/// 게임 위에 떠 있는 것도 같이 찍힘
pub fn capture_screen_rect(rect: Rect) -> Result<Rgb, CaptureError> {
    if rect.is_empty() {
        return Err(CaptureError::Empty);
    }
    unsafe {
        let sdc = GetDC(None);
        if sdc.is_invalid() {
            return Err(CaptureError::Win32(
                "GetDC",
                windows::core::Error::from_thread(),
            ));
        }
        let _sdc_guard = Defer(|| {
            ReleaseDC(None, sdc);
        });
        with_dib(sdc, rect.w, rect.h, |mdc| {
            BitBlt(
                mdc,
                0,
                0,
                rect.w,
                rect.h,
                Some(sdc),
                rect.x,
                rect.y,
                SRCCOPY,
            )
            .map_err(|e| CaptureError::Win32("BitBlt", e))
        })
    }
}

/// 프레임 가져오는 방법
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum CaptureMethod {
    /// 게임 창에 `PrintWindow` (창 모드 / 테두리 없는 창)
    #[default]
    PrintWindow,
    /// 클라이언트 사각형을 GDI로 화면 캡처 (전용 전체 화면)
    ScreenBlt,
}

/// 고른 방법으로 게임 클라이언트 영역을 캡처한 인식용 프레임
/// 16:9보다 세로로 길면 가운데 16:9 띠만 (`crop_game_frame`), 좌표는 이 프레임 기준
pub fn capture(hwnd: Hwnd, method: CaptureMethod) -> Result<Rgb, CaptureError> {
    match method {
        CaptureMethod::PrintWindow => capture_window(hwnd),
        CaptureMethod::ScreenBlt => {
            if !hwnd.is_window() {
                return Err(CaptureError::WindowGone);
            }
            if hwnd.is_minimized() {
                return Err(CaptureError::Minimized);
            }
            capture_screen_rect(client_rect_screen(hwnd).ok_or(CaptureError::Empty)?)
        }
    }
    .map(crop_game_frame)
}

/// 게임은 16:9보다 세로로 긴 화면에 16:9 화면을 폭에 맞춰 가운데 그리고 위아래를 배경으로 채움
/// 그 가운데 띠 (높이는 폭의 9/16을 반올림)만 잘라 냄, 16:9이거나 가로로 더 길면 복사 없이 그대로
fn crop_game_frame(img: Rgb) -> Rgb {
    let h = (img.width as f32 * 9.0 / 16.0).round() as usize;
    if h >= img.height {
        return img;
    }
    // 여백이 홀수면 남는 1 px는 아래쪽
    let top = (img.height - h) / 2;
    img.crop(Rect::new(0, top as i32, img.width as i32, h as i32))
}
