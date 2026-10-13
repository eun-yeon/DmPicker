//! 크기·시간·이동 횟수를 제한한 받기

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Networking::WinHttp::*;

const MAX_REDIRECTS: usize = 5;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const BUFFER_BYTES: usize = 8192;
const POLL: Duration = Duration::from_millis(50);
const UPDATE_RECEIVE_TIMEOUT_MS: i32 = 30_000;
const PLAYLIST_RECEIVE_TIMEOUT_MS: i32 = 3_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Cancelled,
    Timeout,
    Network,
    Status,
    InvalidResponse,
    TooLarge,
}

/// 주소와 서명된 쿼리를 담지 않는 실패 정보
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub status: Option<u16>,
    pub retry_after: Option<String>,
    pub code: Option<u32>,
}

impl Failure {
    fn new(kind: FailureKind) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
            code: None,
        }
    }

    fn system(error: windows::core::Error) -> Self {
        Self::system_code(error.code().0 as u32 & 0xffff)
    }

    fn system_code(code: u32) -> Self {
        let kind = match code {
            ERROR_WINHTTP_TIMEOUT => FailureKind::Timeout,
            ERROR_WINHTTP_OPERATION_CANCELLED => FailureKind::Cancelled,
            _ => FailureKind::Network,
        };
        Self {
            code: Some(code),
            ..Self::new(kind)
        }
    }

    pub fn cancelled(&self) -> bool {
        self.kind == FailureKind::Cancelled
    }
}

/// 첫 302의 이동 주소만 읽음
pub fn first_redirect(
    url: &str,
    timeout_secs: u64,
    cancel: &AtomicBool,
) -> Result<String, Failure> {
    let deadline = deadline(timeout_secs)?;
    check(deadline, cancel)?;
    let session = Session::new(UPDATE_RECEIVE_TIMEOUT_MS)?;
    let parsed = Url::parse(url)?;
    let request = session.request(&parsed, deadline, cancel)?;
    if request.status()? != 302 {
        return Err(request.status_failure()?);
    }
    check(deadline, cancel)?;
    request
        .header(WINHTTP_QUERY_LOCATION)?
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)
}

/// 모든 이동과 본문 읽기에 하나의 시간·크기 상한 적용
pub fn download(
    url: &str,
    cap: usize,
    timeout_secs: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Failure> {
    download_inner(
        url,
        cap,
        timeout_secs,
        None,
        UPDATE_RECEIVE_TIMEOUT_MS,
        cancel,
    )
}

/// 플레이리스트 응답, 1 MiB를 넘거나 본문이 8초 넘게 걸리면 버림
pub fn playlist(url: &str) -> Result<Vec<u8>, Failure> {
    download_inner(
        url,
        1_048_576,
        30,
        Some(Duration::from_secs(8)),
        PLAYLIST_RECEIVE_TIMEOUT_MS,
        &AtomicBool::new(false),
    )
}

fn download_inner(
    url: &str,
    cap: usize,
    timeout_secs: u64,
    body_timeout: Option<Duration>,
    receive_timeout_ms: i32,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Failure> {
    let deadline = deadline(timeout_secs)?;
    check(deadline, cancel)?;
    let session = Session::new(receive_timeout_ms)?;
    let mut parsed = Url::parse(url)?;
    for redirects in 0..=MAX_REDIRECTS {
        let request = session.request(&parsed, deadline, cancel)?;
        match request.status()? {
            200 => return request.body(cap, deadline, body_timeout, cancel),
            301 | 302 | 303 | 307 | 308 if redirects < MAX_REDIRECTS => {
                let location = request
                    .header(WINHTTP_QUERY_LOCATION)?
                    .ok_or_else(invalid)?;
                parsed = parsed.redirect(&location)?;
            }
            _ => return Err(request.status_failure()?),
        }
    }
    Err(invalid())
}

fn invalid() -> Failure {
    Failure::new(FailureKind::InvalidResponse)
}

fn deadline(timeout_secs: u64) -> Result<Instant, Failure> {
    Instant::now()
        .checked_add(Duration::from_secs(timeout_secs))
        .ok_or_else(invalid)
}

fn check(deadline: Instant, cancel: &AtomicBool) -> Result<(), Failure> {
    if cancel.load(Ordering::Acquire) {
        Err(Failure::new(FailureKind::Cancelled))
    } else if Instant::now() >= deadline {
        Err(Failure::new(FailureKind::Timeout))
    } else {
        Ok(())
    }
}

struct Url {
    host: String,
    port: u16,
    path: String,
    origin: String,
    secure: bool,
}

impl Url {
    fn parse(url: &str) -> Result<Self, Failure> {
        if url.len() > MAX_HEADER_BYTES
            || url
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
            return Err(invalid());
        }
        let url = url.split('#').next().ok_or_else(invalid)?;
        let wide: Vec<u16> = url.encode_utf16().collect();
        let mut parts = URL_COMPONENTS {
            dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
            dwHostNameLength: u32::MAX,
            dwUserNameLength: u32::MAX,
            dwPasswordLength: u32::MAX,
            dwUrlPathLength: u32::MAX,
            dwExtraInfoLength: u32::MAX,
            ..Default::default()
        };
        unsafe { WinHttpCrackUrl(&wide, 0, &mut parts) }.map_err(|_| invalid())?;
        if parts.dwUserNameLength != 0 || parts.dwPasswordLength != 0 {
            return Err(invalid());
        }
        let host = component(&wide, parts.lpszHostName, parts.dwHostNameLength)?;
        if host.is_empty() {
            return Err(invalid());
        }
        let secure = parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;
        if !allowed_scheme(secure, parts.nScheme, &host) {
            return Err(invalid());
        }
        let mut path = component(&wide, parts.lpszUrlPath, parts.dwUrlPathLength)?;
        if path.is_empty() {
            path.push('/');
        }
        path.push_str(&component(
            &wide,
            parts.lpszExtraInfo,
            parts.dwExtraInfoLength,
        )?);
        let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
        let authority = rest.split(['/', '?']).next().ok_or_else(invalid)?;
        Ok(Self {
            host,
            port: parts.nPort,
            path,
            origin: format!("{scheme}://{authority}"),
            secure,
        })
    }

    fn redirect(&self, location: &str) -> Result<Self, Failure> {
        if location.is_empty() {
            return Err(invalid());
        }
        let url = if location.contains("://") {
            location.to_owned()
        } else if location.starts_with("//") {
            let (scheme, _) = self.origin.split_once("://").ok_or_else(invalid)?;
            format!("{scheme}:{location}")
        } else if location.starts_with('/') {
            format!("{}{location}", self.origin)
        } else if location.starts_with('?') {
            let path = self.path.split('?').next().ok_or_else(invalid)?;
            format!("{}{path}{location}", self.origin)
        } else {
            let path = self.path.split('?').next().ok_or_else(invalid)?;
            let (parent, _) = path.rsplit_once('/').ok_or_else(invalid)?;
            format!("{}{parent}/{location}", self.origin)
        };
        let next = Self::parse(&url)?;
        if self.secure && !next.secure {
            return Err(invalid());
        }
        Ok(next)
    }
}

fn allowed_scheme(secure: bool, _scheme: WINHTTP_INTERNET_SCHEME, _host: &str) -> bool {
    if secure {
        return true;
    }
    false
}

fn component(input: &[u16], ptr: PWSTR, count: u32) -> Result<String, Failure> {
    if count == 0 {
        return Ok(String::new());
    }
    let offset = (ptr.0 as usize)
        .checked_sub(input.as_ptr() as usize)
        .ok_or_else(invalid)?;
    if offset % 2 != 0 {
        return Err(invalid());
    }
    let start = offset / 2;
    let end = start.checked_add(count as usize).ok_or_else(invalid)?;
    String::from_utf16(input.get(start..end).ok_or_else(invalid)?).map_err(|_| invalid())
}

struct Http(*mut c_void);

impl Http {
    fn new(raw: *mut c_void) -> Result<Self, Failure> {
        if raw.is_null() {
            Err(Failure::system(windows::core::Error::from_thread()))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

struct Session(Http);

impl Session {
    fn new(receive_timeout_ms: i32) -> Result<Self, Failure> {
        unsafe {
            let agent = HSTRING::from(concat!("DmPicker/", env!("CARGO_PKG_VERSION")));
            let handle = Http::new(WinHttpOpen(
                &agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                WINHTTP_FLAG_ASYNC,
            ))?;
            // 이름 풀이·연결·보내기는 3초, 읽기 대기는 용도별 상한 적용
            WinHttpSetTimeouts(handle.0, 3000, 3000, 3000, receive_timeout_ms)
                .map_err(Failure::system)?;
            Ok(Self(handle))
        }
    }

    fn request(
        &self,
        url: &Url,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Request, Failure> {
        check(deadline, cancel)?;
        unsafe {
            let host = HSTRING::from(&url.host);
            let connection = Http::new(WinHttpConnect(self.0 .0, &host, url.port, 0))?;
            let path = HSTRING::from(&url.path);
            let flags = if url.secure {
                WINHTTP_FLAG_SECURE
            } else {
                WINHTTP_OPEN_REQUEST_FLAGS(0)
            };
            let handle = Http::new(WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &path,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ))?;
            // 쿠키·자동 인증·자동 이동을 끄고 Windows 로그인 정보는 어디에도 안 보냄
            // 이동은 HTTPS인지 직접 확인하며 따라감
            let disabled = WINHTTP_DISABLE_COOKIES
                | WINHTTP_DISABLE_AUTHENTICATION
                | WINHTTP_DISABLE_REDIRECTS;
            option(&handle, WINHTTP_OPTION_DISABLE_FEATURE, disabled)?;
            option(
                &handle,
                WINHTTP_OPTION_AUTOLOGON_POLICY,
                WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH,
            )?;
            // 헤더를 조금씩 보내도 전체 응답 헤더 대기는 10초로 제한
            option(&handle, WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT, 10_000)?;
            let (tx, rx) = mpsc::channel();
            let context = Arc::new(Context {
                tx,
                buffer: UnsafeCell::new([0; BUFFER_BYTES]),
            });
            let raw_context = Arc::into_raw(Arc::clone(&context)) as usize;
            if let Err(error) = WinHttpSetOption(
                Some(handle.0),
                WINHTTP_OPTION_CONTEXT_VALUE,
                Some(&raw_context.to_ne_bytes()),
            ) {
                drop(Arc::from_raw(raw_context as *const Context));
                return Err(Failure::system(error));
            }
            let statuses = WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
                | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE
                | WINHTTP_CALLBACK_STATUS_READ_COMPLETE
                | WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
                | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING;
            let previous = WinHttpSetStatusCallback(handle.0, Some(callback), statuses, 0);
            if previous.is_some_and(|f| f as usize == usize::MAX) {
                drop(Arc::from_raw(raw_context as *const Context));
                return Err(Failure::system(windows::core::Error::from_thread()));
            }
            let request = Request {
                handle,
                _connection: connection,
                context,
                rx,
            };
            check(deadline, cancel)?;
            WinHttpSendRequest(request.handle.0, None, None, 0, 0, raw_context)
                .map_err(Failure::system)?;
            if !matches!(request.wait(deadline, cancel)?, Event::Sent) {
                return Err(invalid());
            }
            WinHttpReceiveResponse(request.handle.0, std::ptr::null_mut())
                .map_err(Failure::system)?;
            if !matches!(request.wait(deadline, cancel)?, Event::Headers) {
                return Err(invalid());
            }
            Ok(request)
        }
    }
}

unsafe fn option(handle: &Http, name: u32, value: u32) -> Result<(), Failure> {
    WinHttpSetOption(Some(handle.0), name, Some(&value.to_ne_bytes())).map_err(Failure::system)
}

enum Event {
    Sent,
    Headers,
    Data(Vec<u8>),
    Error(Failure),
}

struct Context {
    tx: Sender<Event>,
    buffer: UnsafeCell<[u8; BUFFER_BYTES]>,
}

// 한 번에 하나만 읽고 완료 통지 뒤에만 다음 읽기 시작
unsafe impl Sync for Context {}

unsafe extern "system" fn callback(
    _: *mut c_void,
    context: usize,
    status: u32,
    info: *mut c_void,
    size: u32,
) {
    if context == 0 {
        return;
    }
    let ptr = context as *const Context;
    if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
        drop(Arc::from_raw(ptr));
        return;
    }
    Arc::increment_strong_count(ptr);
    let context = Arc::from_raw(ptr);
    let event = match status {
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => Event::Sent,
        WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => Event::Headers,
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE if size == 0 => Event::Data(Vec::new()),
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE
            if !info.is_null() && size as usize <= BUFFER_BYTES =>
        {
            Event::Data(std::slice::from_raw_parts(info.cast::<u8>(), size as usize).to_vec())
        }
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
            if !info.is_null() && size as usize >= std::mem::size_of::<WINHTTP_ASYNC_RESULT>() =>
        {
            let result = std::ptr::read_unaligned(info.cast::<WINHTTP_ASYNC_RESULT>());
            Event::Error(Failure::system_code(result.dwError))
        }
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE | WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
            Event::Error(invalid())
        }
        _ => return,
    };
    let _ = context.tx.send(event);
}

struct Request {
    // 요청을 먼저 닫고 버퍼는 마지막 핸들 닫힘 통지까지 유지
    handle: Http,
    _connection: Http,
    context: Arc<Context>,
    rx: Receiver<Event>,
}

impl Request {
    fn wait(&self, deadline: Instant, cancel: &AtomicBool) -> Result<Event, Failure> {
        loop {
            check(deadline, cancel)?;
            let wait = deadline.saturating_duration_since(Instant::now()).min(POLL);
            match self.rx.recv_timeout(wait) {
                Ok(Event::Error(error)) => return Err(error),
                Ok(event) => {
                    check(deadline, cancel)?;
                    return Ok(event);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Failure::new(FailureKind::Network))
                }
            }
        }
    }

    fn status(&self) -> Result<u16, Failure> {
        let (mut value, mut size) = (0u32, 4u32);
        unsafe {
            WinHttpQueryHeaders(
                self.handle.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut value as *mut u32).cast()),
                &mut size,
                std::ptr::null_mut(),
            )
        }
        .map_err(Failure::system)?;
        u16::try_from(value).map_err(|_| invalid())
    }

    fn status_failure(&self) -> Result<Failure, Failure> {
        Ok(Failure {
            status: Some(self.status()?),
            retry_after: self.header(WINHTTP_QUERY_RETRY_AFTER)?,
            ..Failure::new(FailureKind::Status)
        })
    }

    fn header(&self, name: u32) -> Result<Option<String>, Failure> {
        let mut size = 0u32;
        let result = unsafe {
            WinHttpQueryHeaders(
                self.handle.0,
                name,
                PCWSTR::null(),
                None,
                &mut size,
                std::ptr::null_mut(),
            )
        };
        if let Err(error) = result {
            let failure = Failure::system(error);
            if failure.code == Some(ERROR_WINHTTP_HEADER_NOT_FOUND) {
                return Ok(None);
            }
            if size == 0 {
                return Err(failure);
            }
        }
        if size as usize > MAX_HEADER_BYTES || !size.is_multiple_of(2) {
            return Err(invalid());
        }
        let mut buffer = vec![0u16; size as usize / 2];
        unsafe {
            WinHttpQueryHeaders(
                self.handle.0,
                name,
                PCWSTR::null(),
                Some(buffer.as_mut_ptr().cast()),
                &mut size,
                std::ptr::null_mut(),
            )
        }
        .map_err(Failure::system)?;
        let value = buffer.get(..size as usize / 2).ok_or_else(invalid)?;
        let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
        String::from_utf16(value.get(..end).ok_or_else(invalid)?)
            .map(Some)
            .map_err(|_| invalid())
    }

    fn body(
        &self,
        cap: usize,
        deadline: Instant,
        body_timeout: Option<Duration>,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, Failure> {
        if let Some(length) = self.header(WINHTTP_QUERY_CONTENT_LENGTH)? {
            let length = length.parse::<u64>().map_err(|_| invalid())?;
            if length > cap as u64 {
                return Err(Failure::new(FailureKind::TooLarge));
            }
        }
        let deadline = body_timeout
            .and_then(|timeout| Instant::now().checked_add(timeout))
            .map_or(deadline, |body| body.min(deadline));
        let mut bytes = Vec::new();
        loop {
            check(deadline, cancel)?;
            unsafe {
                WinHttpReadData(
                    self.handle.0,
                    self.context.buffer.get().cast(),
                    BUFFER_BYTES as u32,
                    std::ptr::null_mut(),
                )
            }
            .map_err(Failure::system)?;
            let Event::Data(part) = self.wait(deadline, cancel)? else {
                return Err(invalid());
            };
            if part.is_empty() {
                return Ok(bytes);
            }
            if bytes
                .len()
                .checked_add(part.len())
                .is_none_or(|length| length > cap)
            {
                return Err(Failure::new(FailureKind::TooLarge));
            }
            bytes.extend_from_slice(&part);
        }
    }
}
