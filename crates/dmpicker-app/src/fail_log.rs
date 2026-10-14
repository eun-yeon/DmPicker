//! 전체 인식·곡 이동이 실패했을 때 그때 상황을 `fail-log.txt`에 남김
//! 사용자가 파일만 보내 줘도 원인을 볼 수 있게 PC 환경과 그 작업의 진단 줄을 같이 적음
//! 사용자 폴더 경로는 `~`로 가리고 기록을 못 써도 작업 결과는 그대로
//! 환경은 창 API, DXGI, 레지스트리로만 읽음, 안티치트 때문에 게임 프로세스는 안 엶

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Arguments;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6, DXGI_ADAPTER_FLAG_SOFTWARE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowLongW, GetWindowRect, IsIconic, GWL_STYLE, WS_CAPTION, WS_THICKFRAME};

use crate::driver::JobResult;

const HEADER: &str = "===== 실패 기록 =====\n";
const FOOTER: &str = "===== 기록 끝 =====\n";
/// 원인을 보는 덴 최근 몇 건이면 충분함
const MAX_RECORDS: usize = 5;
/// 실패 바로 앞 흐름만 보면 돼서 오래된 줄은 버림
const MAX_LINES: usize = 200;
/// 보통 진단 줄은 수백 바이트 안쪽, 넘는 줄은 끝을 잘라도 됨
const MAX_LINE_BYTES: usize = 2048;
/// 예전 파일은 이만큼만 읽어서 파일이 이 크기 언저리에서 더 안 커짐
/// 가장 긴 기록도 진단 200줄 × 2KB 남짓이라 다섯 건이 다 들어감
const MAX_FILE_BYTES: u64 = 2560 * 1024;
const UNKNOWN: &str = "확인 불가";

// 취소된 작업이 마무리되는 동안 다음 작업이 시작돼도 진단이 안 섞이게 스레드마다 모음
// None이면 작업 밖이라 안 모음
thread_local! {
    static LINES: RefCell<Option<VecDeque<String>>> = const { RefCell::new(None) };
}
// 취소된 작업과 새 작업이 함께 실패하면 같은 임시 파일에 동시에 쓰게 됨
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// 남길 결과가 작업마다 달라서 받음
#[derive(Clone, Copy)]
pub enum Kind {
    Scan,
    Pick,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Scan => "전체 인식",
            Self::Pick => "곡 이동",
        }
    }
}

/// 작업 스레드에서 작업 직전에 부름, 이때부터 [`finish`]까지 이 스레드의 진단을 모음
pub fn begin() {
    LINES.set(Some(VecDeque::new()));
}

/// `eprintln!`처럼 찍고 작업 중인 스레드면 기록용으로도 모음
pub fn line(args: Arguments<'_>) {
    let text = args.to_string();
    eprintln!("{text}");
    LINES.with_borrow_mut(|lines| {
        let Some(lines) = lines else { return };
        // 자른 다음에 가리면 끝에 걸친 경로가 덜 맞아서 사용자 이름 일부가 남음
        for line in private_paths(&text).lines() {
            if lines.len() == MAX_LINES {
                lines.pop_front();
            }
            lines.push_back(short_line(line));
        }
    });
}

/// 작업이 끝나면 부름, 남길 실패면 모은 진단과 환경을 붙여 저장
/// 저장 오류는 stderr에만 찍음
pub fn finish(store: &Path, kind: Kind, result: &JobResult) {
    let lines = LINES.take().unwrap_or_default();
    let what = match (kind, result) {
        // 열린 채팅 입력창은 사용자가 닫으면 되는 상태라 고칠 문제가 아님
        (_, JobResult::Failed(e)) if e == crate::driver::CHAT => return,
        (_, JobResult::Failed(e)) => e.as_str(),
        // 알림 문구는 다시 인식하라는 안내라 기록엔 뜻만 적음
        (Kind::Pick, JobResult::ListMismatch) => "저장된 곡 목록과 화면이 맞지 않음",
        // 없는 채보 안내는 정상, 확인 못 한 것만
        (Kind::Pick, JobResult::Arrived { diff_note: Some(note), .. }) if note == crate::driver::CHART_ASK => "곡엔 도착했지만 버튼 모드나 난이도를 맞추지 못함",
        // 성공, 취소, 사용자가 건드림, 목록에 없는 곡, 인식 전은 고칠 문제가 아님
        _ => return,
    };
    let record = make_record(kind, what, &environment(crate::sys::find_game()), &lines);
    if let Err(e) = save(store, &record) {
        eprintln!("fail log: {e}");
    }
}

fn short_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_owned();
    }
    let mut end = MAX_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} …(생략)", &line[..end])
}

fn make_record(kind: Kind, result: &str, environment: &str, lines: &VecDeque<String>) -> String {
    let t = unsafe { GetLocalTime() };
    let mut body = format!(
        "시각: {:04}-{:02}-{:02} {:02}:{:02}:{:02} (로컬)\n앱 버전: {}\n종류: {}\n결과: {result}\n환경:\n{environment}\n진단 ({}줄):\n",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        env!("CARGO_PKG_VERSION"),
        kind.name(),
        lines.len()
    );
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    // 진단에 구분선과 같은 줄이 있어도 기록 경계로 안 읽히게 모두 들여씀
    let body: String = private_paths(&body).lines().map(|line| format!("  {}\n", short_line(line))).collect();
    format!("{HEADER}{body}{FOOTER}")
}

/// 사용자 이름이 들어갈 만한 폴더를 모두 `~`로 가림
/// 임시 폴더는 짧은 이름(`NAME~1`)으로 적히기도 하고 저장 폴더는 다른 드라이브로 옮겨져 있기도 함
fn private_paths(text: &str) -> String {
    ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .fold(text.to_owned(), |text, path| redact(&text, &path))
}

/// 구분자 방향, 아스키 대소문자, Debug 표기의 겹친 `\\`가 달라도 같은 경로로 봄
fn redact(text: &str, path: &str) -> String {
    let fold = |s: &str| s.replace('\\', "/").to_ascii_lowercase();
    let path = fold(path.trim_end_matches(['\\', '/']));
    if path.is_empty() {
        return text.to_owned();
    }
    let forms = [path.replace('/', "//"), path];
    // 아스키 글자만 바뀌어서 원문과 바이트 위치가 같음
    let folded = fold(text);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(ch) = text[at..].chars().next() {
        if let Some(form) = forms.iter().find(|form| folded[at..].starts_with(form.as_str())) {
            out.push('~');
            at += form.len();
        } else {
            out.push(ch);
            at += ch.len_utf8();
        }
    }
    out
}

/// 예전 내용에선 머리 줄부터 끝 줄까지 다 있는 기록만 새 기록 뒤에 붙임
/// 끊긴 조각과 기록 밖 내용은 버려서 파일이 깨져 있어도 새 기록은 남음
fn prepend(record: &str, old: &str) -> String {
    let mut records = vec![record.to_owned()];
    let mut current: Option<String> = None;
    for line in old.lines() {
        if line == HEADER.trim_end() {
            current = Some(String::new());
        }
        let Some(block) = &mut current else { continue };
        block.push_str(line);
        block.push('\n');
        if line == FOOTER.trim_end() {
            records.extend(current.take());
        }
    }
    records.truncate(MAX_RECORDS);
    records.concat()
}

fn save(store: &Path, record: &str) -> std::io::Result<()> {
    let _lock = SAVE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let path = store.join("fail-log.txt");
    let mut old = Vec::new();
    match std::fs::File::open(&path) {
        Ok(file) => {
            file.take(MAX_FILE_BYTES).read_to_end(&mut old)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // 못 읽는 파일을 덮으면 예전 기록을 잃음
        Err(e) => return Err(e),
    }
    crate::sys::save_file(&path, prepend(record, &String::from_utf8_lossy(&old)).as_bytes())
}

fn wide(text: &[u16]) -> String {
    String::from_utf16_lossy(&text[..text.iter().position(|&ch| ch == 0).unwrap_or(text.len())])
}

/// 같은 빌드도 누적 업데이트에 따라 다를 수 있어 UBR까지 적음
fn os_version() -> String {
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let text = |name: PCWSTR| {
        let mut buf = [0u16; 128];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
        if read.is_ok() { wide(&buf) } else { UNKNOWN.into() }
    };
    let mut ubr = 0u32;
    let mut size = std::mem::size_of_val(&ubr) as u32;
    let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("UBR"), RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY, None, Some((&mut ubr as *mut u32).cast()), Some(&mut size)) };
    let ubr = if read.is_ok() { ubr.to_string() } else { UNKNOWN.into() };
    format!("운영체제: 빌드 {}.{ubr}, 표시 버전 {}", text(w!("CurrentBuildNumber")), text(w!("DisplayVersion")))
}

/// 내장 그래픽이 켜진 PC에선 같은 카드가 출력 없는 어댑터로 한 번 더 잡히기도 함
fn card_list(names: &[String]) -> String {
    let mut unique: Vec<&str> = Vec::new();
    for name in names {
        if !unique.contains(&name.as_str()) {
            unique.push(name);
        }
    }
    if unique.is_empty() { UNKNOWN.into() } else { unique.join(", ") }
}

/// 게임 모니터를 출력으로 가진 카드에서 연결 카드와 HDR을 읽음
fn graphics(monitor: Option<HMONITOR>) -> String {
    let mut names = Vec::new();
    let mut connected = UNKNOWN.to_owned();
    let mut hdr = UNKNOWN;
    unsafe {
        if let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() {
            for adapter in (0..).map_while(|i| factory.EnumAdapters1(i).ok()) {
                let Ok(desc) = adapter.GetDesc1() else { continue };
                // 화면 출력이 없는 기본 렌더 드라이버
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    continue;
                }
                let name = wide(&desc.Description);
                for output in (0..).map_while(|i| adapter.EnumOutputs(i).ok()) {
                    if !monitor.is_some_and(|m| output.GetDesc().is_ok_and(|d| d.Monitor == m)) {
                        continue;
                    }
                    connected.clone_from(&name);
                    if let Ok(desc) = output.cast::<IDXGIOutput6>().and_then(|o| o.GetDesc1()) {
                        // HDR이 켜진 출력만 이 색 공간을 씀
                        hdr = if desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 { "켜짐" } else { "꺼짐" };
                    }
                }
                names.push(name);
            }
        }
    }
    let mut text = format!("그래픽 카드: {}", card_list(&names));
    if monitor.is_some() {
        text.push_str(&format!("\n게임 모니터 연결 카드: {connected}\nHDR 출력: {hdr}"));
    }
    text
}

fn monitor_info(monitor: HMONITOR) -> (String, Option<RECT>) {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
            return (format!("게임 모니터: {UNKNOWN}"), None);
        }
        let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        let mode_ok = EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool();
        let (mut x, mut y) = (0, 0);
        let scale = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_ok() {
            format!("{:.0}%", x as f64 * 100.0 / 96.0)
        } else {
            UNKNOWN.into()
        };
        let resolution = if mode_ok { format!("{}×{}", mode.dmPelsWidth, mode.dmPelsHeight) } else { UNKNOWN.into() };
        // 0과 1은 하드웨어 기본값이라는 뜻이라 실제 주사율이 아님
        let refresh = if mode_ok && mode.dmDisplayFrequency > 1 { format!("{} Hz", mode.dmDisplayFrequency) } else { UNKNOWN.into() };
        (format!("게임 모니터: 해상도 {resolution}, 배율 {scale}, 주사율 {refresh}"), Some(mi.monitorInfo.rcMonitor))
    }
}

/// 전용 전체 화면과 화면을 다 덮는 테두리 없는 창은 창 정보로 구별이 안 돼서 하나로 봄
fn window_shape(style: u32, rect: RECT, monitor: Option<RECT>) -> &'static str {
    if style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0 {
        "창 모드"
    } else if monitor.is_some_and(|m| rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom) {
        "화면 전체를 덮음"
    } else {
        "테두리 없는 창"
    }
}

/// 창 크기는 시작할 때 켠 모니터별 DPI 인식 덕에 배율과 상관없이 실제 픽셀
fn environment(game: Option<HWND>) -> String {
    let os = os_version();
    let Some(game) = game else {
        return format!("{os}\n{}\n게임 창: 없음\n게임 모니터: 없음", graphics(None));
    };
    let size = |r: RECT| format!("{}×{}", r.right - r.left, r.bottom - r.top);
    unsafe {
        let monitor = MonitorFromWindow(game, MONITOR_DEFAULTTONEAREST);
        let (screen, screen_rect) = monitor_info(monitor);
        let (mut rect, mut client) = (RECT::default(), RECT::default());
        let window = if GetWindowRect(game, &mut rect).is_ok() {
            // 전체 화면 게임은 포커스를 뺏기면 최소화돼서 작은 창 모드처럼 보임
            let shape = if IsIconic(game).as_bool() { "최소화됨" } else { window_shape(GetWindowLongW(game, GWL_STYLE) as u32, rect, screen_rect) };
            format!("게임 창: {shape}\n창 크기: {}", size(rect))
        } else {
            format!("게임 창: {UNKNOWN}\n창 크기: {UNKNOWN}")
        };
        let client = if GetClientRect(game, &mut client).is_ok() { size(client) } else { UNKNOWN.into() };
        format!("{os}\n{}\n{screen}\n{window}\n클라이언트 크기: {client}", graphics(Some(monitor)))
    }
}
