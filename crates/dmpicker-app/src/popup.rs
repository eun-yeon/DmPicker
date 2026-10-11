//! 검색창: 너비 480 px 고정, 알림과 같은 반투명 판, 호버하면 나오는 히스토리·톱니 버튼,
//! 결과 자리에 뜨는 설정·히스토리 시트, 한글 IME, 키보드·마우스

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::core::{Interface, Result, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::UI::Input::Ime::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use dmpicker_search::{Diff, Filter, Mode};

use crate::anim::{Ease, Spring, Tween};
use crate::catalog::Catalog;
use crate::edit::{clean_paste, Editor};
use crate::gfx::{
    self, circle, color, rect, rounded, rrect, white, with_alpha, Drawing, Gfx, Retarget, BEARING, CARET, FAINT, FILL_HOVER,
    FILL_SELECTED, FILL_STRONG, FILL_TRACK, PLATE_ALPHA, SUB, TEXT, TINT,
};
use crate::hotkey::Hotkey;
use crate::sys;

const LIMIT: usize = 50;
const IDENTITY: windows_numerics::Matrix3x2 = windows_numerics::Matrix3x2::identity();
const TIMER_CARET: usize = 2;
const CARET_BLINK_MS: u32 = 530;
const TIMER_FOCUS: usize = 3; // 포커스를 잃은 뒤, 캡처 도구가 가져간 게 아니면 닫음
const TIMER_SEARCH: usize = 4; // 입력이 멈추면 입력한 글로 검색
/// 입력이 이만큼 멈춰야 결과를 바꿈
/// 단어를 치면서 거치는 덜 조합된 글자마다 목록이 통째로 바뀌지 않게
const SEARCH_DELAY_MS: u32 = 180;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// 열기: 띄울 때 0에서 1로 (살짝 튐), 첫 프레임부터 움직임
const OPEN_SPRING: Spring = Spring::new(0.0, 340.0, 22.0);
const OPEN_KICK: f32 = 4.0;
/// 열기: 스프링과 따로 이 시간 동안 처음엔 빠르게 페이드 인
/// 스프링은 느리게 출발해서 그대로 쓰면 첫 프레임들이 거의 안 보임
const OPEN_FADE: f32 = 0.10;
/// 선택 강조 위치: 임계 감쇠라 넘치지 않고 줄까지 미끄러짐
const SEL_SPRING: Spring = Spring::new(0.0, 900.0, 60.0);
/// 닫기: 흐려지는 동안 검색창 전체가 바 가운데 쪽으로 이만큼 줄어듦
const CLOSE_SCALE: f32 = 0.95;
const CLOSE_SECS: f32 = 0.16;
/// 단축키를 못 받은 이유를 보여 주는 시간
const HOTKEY_NOTE_SECS: f32 = 2.0;
/// 세그먼트 컨트롤 손잡이가 새 선택으로 미끄러지는 시간
const SEG_SECS: f32 = 0.25;
/// 토글: 노브는 끝을 살짝 지나쳤다 돌아오고 (커브가 넘침을 만듦, 연타해도 뒤로 안 튐)
const KNOB_SECS: f32 = 0.24;
const KNOB_EASE: Ease = Ease(0.34, 1.56, 0.64, 1.0);
/// 토글 색은 노브보다 짧게 따로, 눌렸던 노브도 이걸로 돌아옴
const QUICK_SECS: f32 = 0.18;
const QUICK_EASE: Ease = Ease(0.16, 1.0, 0.3, 1.0);
/// 토글을 누르는 동안 노브가 천천히 세로로 눌려 캡슐이 됨 (가로는 그대로)
const PRESS_Y: f32 = 10.0 / 12.0;
const PRESS_SECS: f32 = 0.42;
const PRESS_EASE: Ease = Ease(0.33, 1.0, 0.68, 1.0);
/// 닫기: 판이 이 불투명도까지 흐려질 때쯤 글과 줄은 이미 사라짐
const CLOSE_TEXT_GONE: f32 = 0.5;
/// 패널이 나타날 땐 이만큼 (DIP) 위에서 내려앉고, 사라질 땐 그만큼 올라가며 흐려짐
const SHEET_RISE: f32 = 6.0;
const SHEET_IN_SECS: f32 = 0.22;
const SHEET_OUT_SECS: f32 = 0.16;

/// 바 오른쪽 버튼 순서 (`bead_circle`)
const BEAD_HISTORY: usize = 0;
const BEAD_GEAR: usize = 1;
/// 두 버튼 시차: 나올 땐 히스토리 다음 톱니, 들어갈 땐 톱니 다음 히스토리
const BEAD_STAGGER: Duration = Duration::from_millis(70);

/// 논리 (96 dpi) 레이아웃, 검색창 전체 너비는 늘 `W`
mod lay {
    pub const W: f32 = 480.0;
    pub const MARGIN: f32 = 32.0; // 그림자 자리
    pub const BAR_H: f32 = 46.0;
    pub const BAR_R: f32 = 23.0;
    pub const BTN_D: f32 = 46.0;
    pub const BTN_GAP: f32 = 8.0;
    pub const BEADS: usize = 2; // 히스토리, 톱니 버튼 (하나당 바가 54 줄어듦)
    pub const GAP: f32 = 8.0; // 바와 패널 사이
    pub const PANEL_R: f32 = 20.0;
    pub const PAD: f32 = 8.0;
    pub const INSET: f32 = 16.0; // 패널 내용 안쪽 여백 (16 정렬선)
    pub const ROW_H: f32 = 48.0;
    pub const ROWS: usize = 6;
    pub const LEVEL_W: f32 = 60.0; // 줄 오른쪽 끝의 레벨 칸
    pub const SETTINGS_H: f32 = 348.0; // 맨 아래 줄(버튼 높이 30) 아래 16 (= 오른쪽 여백)
    pub const TOGGLE_W: f32 = 42.0;
    pub const TOGGLE_H: f32 = 24.0;
    pub const KNOB: f32 = 18.0; // 둘레 3
    pub const CARD_H: f32 = 84.0;
    pub const CARD_BTN_W: f32 = 112.0;
    pub const COND_W: f32 = 52.0; // 글은 가운데, 오른쪽 끝이 16 정렬선
    pub const PANEL_MAX: f32 = PAD * 2.0 + ROW_H * ROWS as f32; // 304
}

struct Fonts {
    input: IDWriteTextFormat,
    placeholder: IDWriteTextFormat,
    title: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    head: IDWriteTextFormat,
    item: IDWriteTextFormat,
    item_bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    icon: Option<IDWriteTextFormat>,
    /// 히스토리 시계, 같은 크기면 톱니보다 잉크가 적어 작고 가늘어 보여서 조금 크게
    icon_history: Option<IDWriteTextFormat>,
}

struct Effects {
    goo_blur: ID2D1Effect,
    sheets: ID2D1Effect, // 합성: 끈적하게 붙는 바+두 버튼 (입력 0) + 선명한 패널 (입력 1)
    shadow: gfx::Shadow, // 모양 아래 그림자
    plate: ID2D1Effect,  // 모양 안을 칠한 판
    rim_light: ID2D1Effect,
    _keep: Vec<ID2D1Effect>,
}

pub enum Action {
    None,
    /// 곡 id, 레벨 검색에서 고른 패턴이면 그 (모드, 난이도) (없으면 설정대로)
    Pick(u32, Option<(Mode, Diff)>),
    Hide,
    Scan,
    /// 모드·난이도 바뀜 (저장해야 함)
    Choice,
    /// 새 단축키를 받음, 등록해 보고 [`Popup::hotkey_result`]로 알려야 함
    Hotkey(Hotkey),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Results,
    Settings,
    /// 최근 고른 곡, 히스토리 버튼으로 엶
    History,
}

/// 시트에 그리는 내용, 보이거나 나타나는 중에만 바뀌고 사라지는 동안엔 그대로 (`retarget_panel`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sheet {
    Settings,
    Card,
    /// 검색 결과 줄
    Results,
    /// 최근 고른 곡 줄
    History,
}

/// 결과 한 줄
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Row {
    /// 곡 (`songs` 인덱스), 설정의 모드·난이도로 이동
    Song(usize),
    /// 레벨 조건 검색의 패턴, 그 모드·난이도로 이동
    Chart(usize, Mode, Diff),
    /// 조건 검색 결과 중 무작위 하나
    Random,
}

impl Row {
    fn song(self) -> Option<usize> {
        match self {
            Row::Song(i) | Row::Chart(i, ..) => Some(i),
            Row::Random => None,
        }
    }
}

/// 시스템 시간으로 시작하는 xorshift64
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        Self(t | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// 무작위 줄이 뽑는 줄: 무작위 줄 말고 전부 (레벨 조건이면 패턴, BPM 조건만이면 곡)
fn picks(rows: &[Row]) -> impl Iterator<Item = Row> + '_ {
    rows.iter().copied().filter(|&r| r != Row::Random)
}

/// 앱을 켠 뒤 무작위 줄로 뽑은 줄, 검색 조건을 바꿔도 이어짐 (저장 안 함)
#[derive(Default)]
struct Rolls {
    /// 뽑은 줄, 지금 결과를 다 뽑으면 그 결과의 것만 지움
    picked: HashSet<Row>,
    /// 직전에 뽑은 줄, 새 바퀴의 첫 뽑기에서 뺌
    last: Option<Row>,
}

impl Rolls {
    /// 다음에 뽑을 수 있는 줄과 새 바퀴 여부, 이력은 안 바꿈
    /// 안 뽑은 줄이 없으면 새 바퀴라 직전 줄만 뺌 (줄이 하나뿐이면 그것)
    fn choices<'a>(&'a self, rows: &'a [Row]) -> (impl Iterator<Item = Row> + 'a, bool) {
        let restart = picks(rows).all(|r| self.picked.contains(&r));
        let skip = if restart && picks(rows).nth(1).is_some() { self.last } else { None };
        let choices = picks(rows).filter(move |&r| if restart { Some(r) != skip } else { !self.picked.contains(&r) });
        (choices, restart)
    }

    /// 남은 줄 중 하나, 다 뽑았으면 지금 결과의 이력만 비우고 다시 시작
    fn draw(&mut self, rows: &[Row], r: u64) -> Option<Row> {
        let (choices, restart) = self.choices(rows);
        let choices: Vec<Row> = choices.collect();
        if choices.is_empty() {
            return None;
        }
        if restart {
            self.picked.retain(|p| !rows.contains(p));
        }
        let row = choices[(r % choices.len() as u64) as usize];
        self.picked.insert(row);
        self.last = Some(row);
        Some(row)
    }

    /// 무작위 줄의 아랫줄: 뽑을 줄 수, 다음에 뽑을 수 있는 줄이 그보다 적으면 남은 수도
    fn note(&self, rows: &[Row]) -> String {
        let n = picks(rows).count();
        let m = self.choices(rows).0.count();
        if m < n { format!("{n}개 중 하나 ({m}개 남음)") } else { format!("{n}개 중 하나") }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    None,
    Inside,
    /// 입력 칸: 누르면 캐럿을 옮기고 끌면 선택
    Text,
    Gear,
    History,
    ModeSeg(usize),
    DiffSeg(usize),
    Rescan,
    Hotkey,
    KeepQuery,
    CardButton,
    Row(usize),
}

/// 글이 바뀐 이유, `requery`에서 씀
#[derive(Clone, Copy)]
enum Edit {
    /// 키 입력: 글자, 지우기, 조합 중인 글자
    Typed,
    /// IME가 완성된 글자를 넘김 (다음 글자 조합을 시작할 수도 있음)
    Result,
    /// IME가 조합을 끝냄, 조합 중이던 글자가 빠졌다가 결과로 바로 돌아올 수 있음
    Ended,
}

/// 물리 px 기준 설정 시트: 클릭 영역과 각 글줄의 위쪽
struct SettingsLayout {
    title: f32,
    labels: [f32; 2], // 모드·난이도 컨트롤 위
    mode: D2D_RECT_F,
    diff: D2D_RECT_F,
    list_title: f32,
    rescan: D2D_RECT_F,
    list_note: f32,
    hotkey_title: f32,
    hotkey: D2D_RECT_F, // 글 너비에 맞춘 버튼
    hotkey_note: f32,
    keep_title: f32,
    keep: D2D_RECT_F, // 토글
    keep_note: f32,
}

pub struct Popup {
    pub hwnd: HWND,
    gfx: Option<Gfx>,
    scale: f32,
    pub visible: bool,
    closing: bool, // 닫기 애니메이션 중 (창은 아직 보이고 입력은 무시)
    catalog: Catalog,
    /// 검색 대상 곡 id (저장된 목록), `None`이면 첫 인식 전이라 전부
    owned: Option<HashSet<u32>>,
    /// 곡 이동 중 화면이 저장된 목록과 달랐음 (다른 계정, 새 곡)
    stale: bool,
    /// 검색창 단축키 (앱이 등록하고 저장함)
    pub hotkey: Hotkey,
    /// 새 단축키를 기다리는 중이면 Some(대기 동안 떼어 둔 IME 입력 컨텍스트)
    hotkey_wait: Option<HIMC>,
    /// 새 조합을 못 받은 이유와 그때
    hotkey_note: Option<(&'static str, Instant)>,
    pub mode_choice: Mode,
    pub diff_choice: Diff,
    /// 최근 고른 곡 id, 최신순 (앱이 불러오고 저장함)
    pub recent: Vec<u32>,
    /// 다시 열 때 지난 검색어를 남김 (앱이 불러오고 저장함)
    pub keep_query: bool,
    /// 입력한 글, 캐럿과 선택 (조합 중인 글자는 `preedit`에 따로, 캐럿 자리에 그림)
    edit: Editor,
    preedit: String,
    /// WM_CHAR로 먼저 온 UTF-16 앞 조각 (뒤 조각과 합쳐 한 글자)
    high_surrogate: Option<u16>,
    /// 검색 결과
    results: Vec<Row>,
    /// 결과가 BPM 조건 검색이라 아랫줄에 DLC 대신 BPM
    bpm_rows: bool,
    /// 히스토리 보기의 줄, 열 때마다 최근 고른 곡으로 채움
    history: Vec<Row>,
    /// `selected`·`scroll`이 가리키는 목록 (결과나 히스토리), 보이는 목록이 바뀔 때만 첫 줄로
    sel_of: Sheet,
    rng: Rng,
    /// 무작위 줄로 뽑은 이력
    rolls: Rolls,
    /// 지금 결과의 검색어, 검색이 입력을 따라잡으면 입력한 글과 같음
    /// 제자리에서 확정된 IME 글자는 아무것도 안 바꿈
    searched: String,
    /// 마지막으로 바뀐 때의 글, IME가 마지막으로 조합을 끝내기 전의 글 (다음 변경까지)
    /// 이걸로 새 입력과 그대로 확정된 글자를 구분함
    seen: String,
    ended: Option<String>,
    selected: usize,
    scroll: usize, // 처음 보이는 결과
    wheel_acc: i32, // 아직 줄로 안 바꾼 휠 값 (정밀 휠, 터치패드)
    view: View,
    sheet: Sheet,
    kb_focus: Option<usize>, // 설정 키보드 포커스: 0 모드, 1 난이도, 2 다시 인식, 3 단축키, 4 검색어 유지
    keep_down: bool,         // 검색어 유지 토글을 누른 채 (떼면 바뀜)
    hover: Hit,
    // 애니메이션
    open_s: Spring,      // 띄울 때 0에서 1로 (살짝 튐)
    reveal: [Spring; 2], // 버튼별 0 숨김 .. 1 표시 (하나당 바가 54 줄어듦), 튐
    beads_out: bool,     // 두 버튼을 내보내는 중 (스프링은 시차를 두고 따라감)
    reveal_follow: Option<(Instant, usize, f32)>, // 시차 뒤에 움직일 버튼과 목표
    icon_alpha: Tween,      // 두 버튼 아이콘
    panel_h: Spring,        // 보이는 패널의 높이, 내용이 바뀔 때만 움직임
    shown: Tween,           // 패널 보임 0 .. 1, 나타나고 사라질 때의 페이드
    close: Tween,           // 닫는 동안 0에서 1로
    sel_y: Spring,          // 선택 강조 위쪽, 패널 px (높이는 한 줄)
    scroll_s: Spring,       // 목록 오프셋 px, scroll x 줄 간격을 따라감
    seg: [Tween; 2],        // 세그먼트 손잡이 [모드, 난이도]: 왼쪽 끝, 칸 단위
    knob: Tween,            // 토글 노브 자리 0 끔 .. 1 켬 (살짝 넘침)
    knob_tint: Tween,       // 토글 색 0 끔 .. 1 켬
    knob_press: Tween,      // 노브 세로 배율, 누르는 동안 줄어듦
    hover_since: Option<Instant>,
    leave_since: Option<Instant>,
    in_zone: bool, // 커서가 두 버튼을 드러내는 영역에 있음
    tracking: bool,
    shown_at: Instant,
    last: Instant,
    moving: bool, // 직전 프레임이 애니메이션 중이었음
    caret_t0: Instant,
    text_scroll: f32, // 입력 칸을 가로로 민 거리 px, 캐럿이 보이게 (`render`)
    dragging: bool,   // 입력 칸을 누른 채 끄는 중 (선택)
    last_mouse: Option<(i32, i32)>, // 마지막 마우스 이동의 클라이언트 좌표
    // 리소스
    jackets: HashMap<u32, Option<ID2D1Bitmap1>>,
    fonts: Option<Fonts>,
    fx: Option<Effects>,
}

/// 그릴 시트 (`sheet_geom`), 판정과 그리기가 같이 씀
struct SheetGeom {
    rect: D2D_RECT_F,
    /// 시트와 내용의 불투명도
    alpha: f32,
    /// 다 편 자리에서 비켜난 거리 (px, 위가 음수), 내용도 같이 움직임
    dy: f32,
}

fn s_to_u16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 위치 `u`의 바이트 위치 (DirectWrite 위치를 글 위치로)
fn byte_at_u16(s: &str, u: usize) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= u {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}

/// 레이아웃에서 글 `s`의 바이트 위치 `at` 자리의 x (첫 글자 왼쪽이 0)
/// 앞 글자의 오른쪽 끝으로 잼 (글 끝, 끝 공백도 됨)
fn text_x(l: &IDWriteTextLayout, s: &str, at: usize) -> f32 {
    let u = s_to_u16(&s[..at]) as u32;
    if u == 0 {
        return 0.0;
    }
    let (mut x, mut y) = (0.0, 0.0);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    let _ = unsafe { l.HitTestTextPosition(u - 1, true, &mut x, &mut y, &mut m) };
    x
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

/// 세그먼트 컨트롤 `r`의 4칸 중 `x`에 있는 칸
fn segment_at(r: &D2D_RECT_F, x: f32) -> usize {
    (((x - r.left) / (r.right - r.left)) * 4.0).clamp(0.0, 3.0) as usize
}

impl Popup {
    pub fn new(hwnd: HWND, catalog: Catalog) -> Self {
        let now = Instant::now();
        Self {
            hwnd,
            gfx: None,
            scale: 1.0,
            visible: false,
            closing: false,
            catalog,
            owned: None,
            stale: false,
            hotkey: Hotkey::DEFAULT,
            hotkey_wait: None,
            hotkey_note: None,
            mode_choice: Mode::B4,
            diff_choice: Diff::NM,
            recent: Vec::new(),
            keep_query: false,
            edit: Editor::default(),
            preedit: String::new(),
            high_surrogate: None,
            results: Vec::new(),
            bpm_rows: false,
            history: Vec::new(),
            sel_of: Sheet::Results,
            rng: Rng::new(),
            rolls: Rolls::default(),
            searched: String::new(),
            seen: String::new(),
            ended: None,
            selected: 0,
            scroll: 0,
            wheel_acc: 0,
            view: View::Results,
            sheet: Sheet::Results,
            kb_focus: None,
            keep_down: false,
            hover: Hit::None,
            open_s: OPEN_SPRING,
            reveal: [Spring::new(0.0, 300.0, 17.0); 2],
            beads_out: false,
            reveal_follow: None,
            icon_alpha: Tween::new(0.0),
            panel_h: Spring::new(0.0, 330.0, 29.0),
            shown: Tween::new(0.0),
            close: Tween::new(0.0),
            sel_y: SEL_SPRING,
            scroll_s: Spring::new(0.0, 600.0, 46.0),
            seg: [Tween::new(0.0); 2],
            knob: Tween::new(0.0),
            knob_tint: Tween::new(0.0),
            knob_press: Tween::new(1.0),
            hover_since: None,
            leave_since: None,
            in_zone: false,
            tracking: false,
            shown_at: now,
            last: now,
            moving: false,
            caret_t0: now,
            text_scroll: 0.0,
            dragging: false,
            last_mouse: None,
            jackets: HashMap::new(),
            fonts: None,
            fx: None,
        }
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    fn win_size(&self) -> (i32, i32) {
        let w = lay::MARGIN * 2.0 + lay::W;
        let h = lay::MARGIN * 2.0 + lay::BAR_H + lay::GAP + lay::PANEL_MAX.max(lay::SETTINGS_H);
        (self.px(w) as i32, self.px(h) as i32)
    }

    /// 열려 있고 입력을 받는 중 (닫기 애니메이션 중엔 false)
    pub fn is_open(&self) -> bool {
        self.visible && !self.closing
    }

    /// 검색어: 입력한 글의 캐럿 자리에 조합 중인 글자를 끼움
    fn query(&self) -> String {
        let (t, c) = (self.edit.text(), self.edit.caret());
        format!("{}{}{}", &t[..c], self.preedit, &t[c..])
    }

    pub fn animating(&self) -> bool {
        self.visible
            && (!self.open_s.settled()
                || self.shown_at.elapsed().as_secs_f32() < OPEN_FADE
                || self.reveal.iter().any(|r| !r.settled())
                || self.reveal_follow.is_some()
                || !self.icon_alpha.done()
                || !self.panel_h.settled()
                || !self.shown.done()
                || (self.shown.target() <= 0.0 && self.panel_h.value != 0.0) // 다 사라진 뒤 높이 정리 (`render`)
                || self.closing
                || !self.sel_y.settled()
                || !self.scroll_s.settled()
                || self.seg.iter().any(|t| !t.done())
                || [self.knob, self.knob_tint, self.knob_press].iter().any(|t| !t.done())
                || self.hover_since.is_some()
                || self.leave_since.is_some())
    }

    /// 검색어가 비었을 때 결과 대신 보여주는 안내 카드
    fn card(&self) -> Option<(&'static str, &'static str, &'static str)> {
        if !self.searched.trim().is_empty() || self.view != View::Results {
            return None;
        }
        self.notice()
    }

    /// 이번 열기의 목록에 맞는 안내 (제목, 설명, 인식 버튼): 첫 인식 전, 목록이 다름
    fn notice(&self) -> Option<(&'static str, &'static str, &'static str)> {
        if self.owned.is_none() {
            return Some(("곡 목록을 한 번 읽어올게요", "읽는 동안 잠시 손을 쉬어 주세요", "목록 인식 시작"));
        }
        if self.stale {
            return Some(("저장된 곡 목록과 화면이 달라요", "계정이나 DLC가 바뀌었다면 다시 읽어 주세요", "다시 인식"));
        }
        None
    }

    // ---------------------------------------------------------------- 열기 / 닫기

    /// (숨긴) 창을 게임 위에 크기 맞춰 놓고 그 모니터용 그리기 리소스를 만듦
    /// 못 만들면 false
    fn prepare(&mut self) -> bool {
        let game = sys::find_game().and_then(sys::client_rect_screen);
        let anchor = game.unwrap_or_else(|| {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            let rc = RECT { left: p.x, top: p.y, right: p.x + 1, bottom: p.y + 1 };
            sys::monitor_work_area(sys::monitor_for_rect(&rc))
        });
        let scale = sys::ui_scale(&anchor, game.is_some());
        if (scale - self.scale).abs() > 0.01 || self.gfx.is_none() {
            self.scale = scale;
            self.drop_gfx();
        }
        let (w, h) = self.win_size();
        let cx = (anchor.left + anchor.right) / 2;
        let x = cx - w / 2;
        let ah = anchor.bottom - anchor.top;
        let y = anchor.top + (ah as f32 * 0.28) as i32 - self.px(lay::MARGIN) as i32;

        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
        if self.gfx.is_none() {
            match Gfx::new(self.hwnd, w as u32, h as u32, gfx::MAX_FPS) {
                Ok(g) => self.gfx = Some(g),
                Err(e) => {
                    eprintln!("gfx init failed: {e:?}");
                    return false;
                }
            }
            if let Err(e) = self.init_resources() {
                eprintln!("init_resources failed: {e:?}");
                self.drop_gfx();
                return false;
            }
        }
        true
    }

    /// 그리기 리소스를 다 버림, 다음 `prepare`에서 새로 만듦
    fn drop_gfx(&mut self) {
        self.gfx = None;
        self.fonts = None;
        self.fx = None;
        self.jackets.clear();
    }

    /// 그리기 리소스를 시작할 때 미리 만들어 첫 열기가 기다리지 않게 함
    pub fn warm_up(&mut self) {
        self.prepare();
    }

    /// 검색창을 띄움, `owned`는 보이는 목록에서 고를 수 있는 곡 (첫 인식 전엔 None)
    /// `stale`이면 곡 이동 중 화면이 저장된 목록과 달랐음
    pub fn show(&mut self, owned: Option<HashSet<u32>>, stale: bool) {
        if !self.prepare() {
            return;
        }
        if let Some(g) = &self.gfx {
            g.retune(self.hwnd); // 게임이 (그래서 우리도) 다른 모니터로 옮겼을 수 있음
        }
        self.owned = owned;
        self.stale = stale;

        // 닫을 때 조합 중이던 글자까지 보이던 그대로
        let kept = if self.keep_query { self.query() } else { String::new() };
        self.edit.clear();
        self.preedit.clear();
        self.high_surrogate = None;
        self.text_scroll = 0.0;
        self.dragging = false;
        self.results.clear();
        self.sheet = Sheet::Results;
        self.sel_of = Sheet::Results;
        self.searched.clear();
        self.seen.clear();
        self.ended = None;
        self.reset_rows();
        self.view = View::Results;
        self.kb_focus = None;
        self.hover = Hit::None;
        self.closing = false;
        self.close.snap(0.0);
        self.open_s = OPEN_SPRING;
        self.open_s.target = 1.0;
        self.open_s.velocity = OPEN_KICK;
        self.shown_at = Instant::now();
        self.reveal.iter_mut().for_each(|r| r.snap(0.0));
        self.beads_out = false;
        self.reveal_follow = None;
        self.icon_alpha.snap(0.0);
        self.panel_h.snap(0.0);
        self.shown.snap(0.0);
        // 검색어 유지: 지난 글을 다 선택해 둠 (치면 바뀌고 Enter면 같은 검색), 결과는 이번 목록으로 다시 찾음
        if !kept.is_empty() {
            self.edit.insert(&kept);
            self.edit.select_all();
            self.seen = kept;
            self.search_now();
        }
        self.retarget_panel();
        self.snap_segments();
        let on = self.keep_query as u8 as f32;
        self.knob.snap(on);
        self.knob_tint.snap(on);
        self.knob_press.snap(1.0);
        self.hover_since = None;
        self.leave_since = None;
        self.in_zone = false;
        self.tracking = false;
        self.last_mouse = None;
        self.visible = true;
        self.last = Instant::now();
        self.caret_t0 = Instant::now();
        if let Err(e) = self.render() {
            eprintln!("render error (show): {e:?}");
        }
        if !self.visible {
            return; // 그리다 장치가 사라져 숨김 (`render`), 안 보이는 창이 포커스를 가져가지 않게
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
            SetTimer(Some(self.hwnd), TIMER_CARET, CARET_BLINK_MS, None);
        }
    }

    /// 애니메이션 없이 바로 숨김, 트레이 메뉴처럼 메시지 루프가 잠깐 멈출 때
    pub fn hide_now(&mut self) {
        self.hide();
        if self.visible {
            self.finish_hide();
        }
    }

    /// 닫기 애니메이션 시작: 바·두 버튼·패널이 한 덩어리로 조금 줄며 그 자리에서 흐려짐
    /// 보이던 걸 그대로 보여주고 (커서 없음) 끝나면 `finish_hide` 실행
    pub fn hide(&mut self) {
        if !self.visible || self.closing {
            return;
        }
        self.closing = true;
        self.end_hotkey_wait();
        self.end_drag();
        self.release_toggle();
        self.notify_composition(CPS_CANCEL); // 조합 중이던 글자는 숨겨질 때까지 그려 둠
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
            let _ = KillTimer(Some(self.hwnd), TIMER_FOCUS);
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
        self.hover_since = None;
        self.leave_since = None;
        self.close.go(1.0, CLOSE_SECS, Ease::DECEL);
        if self.gfx.is_none() {
            self.finish_hide();
        }
    }

    fn finish_hide(&mut self) {
        self.closing = false;
        self.visible = false;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    // ---------------------------------------------------------------- 리소스

    fn init_resources(&mut self) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        let icon_family = ["Segoe Fluent Icons", "Segoe MDL2 Assets"].into_iter().find(|f| g.font_exists(f));
        let s = self.scale;
        self.fonts = Some(Fonts {
            input: g.ui_format(18.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            placeholder: g.ui_format(17.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            title: g.ui_format(15.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            sub: g.ui_format(12.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            head: g.ui_format(14.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            item: g.ui_format(13.0 * s, DWRITE_FONT_WEIGHT_MEDIUM)?,
            item_bold: g.ui_format(13.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            small: g.ui_format(11.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            icon: icon_family.and_then(|f| g.text_format(f, 19.0 * s, DWRITE_FONT_WEIGHT_NORMAL).ok()),
            icon_history: icon_family.and_then(|f| g.text_format(f, 20.5 * s, DWRITE_FONT_WEIGHT_NORMAL).ok()),
        });

        // 모양 마스크: 작은 블러 + 알파 임계값, 거의 닿을 때만 모양이 녹아 붙음
        let goo_blur = g.effect(&CLSID_D2D1GaussianBlur)?;
        // 늘어나는 목이 보일 만큼 강하게, 쉴 땐 8 px 거리의 버튼이 분리될 만큼 약하게
        gfx::set_f32(&goo_blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, 5.0 * s)?;
        gfx::set_u32(&goo_blur, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32, D2D1_BORDER_MODE_SOFT.0 as u32)?;
        let thresh = g.effect(&CLSID_D2D1ColorMatrix)?;
        gfx::set_floats(
            &thresh,
            D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32,
            &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 22.0, 22.0, 22.0, 22.0, -10.5, -10.5, -10.5, -10.5],
        )?;
        gfx::set_bool(&thresh, D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT.0 as u32, true)?;
        gfx::set_input_effect(&thresh, 0, &goo_blur)?;
        let sheets = g.effect(&CLSID_D2D1Composite)?;
        gfx::set_input_effect(&sheets, 0, &thresh)?;

        // 그림자: 모양 마스크를 알림과 같은 그림자로 (`gfx::Shadow`)
        let shadow = gfx::Shadow::new(g, s)?;
        shadow.set_input_effect(&sheets)?;

        // 판: 모양 안을 알림과 같은 색으로 칠함, 뒤의 게임은 창 합성으로 비침
        // Flood 색은 premultiplied라 알파를 미리 곱함
        let flood = g.effect(&CLSID_D2D1Flood)?;
        gfx::set_floats(&flood, D2D1_FLOOD_PROP_COLOR.0 as u32, &[TINT.r, TINT.g, TINT.b, 1.0].map(|c| c * PLATE_ALPHA))?;
        let plate = g.effect(&CLSID_D2D1AlphaMask)?;
        gfx::set_input_effect(&plate, 0, &flood)?;
        gfx::set_input_effect(&plate, 1, &sheets)?;

        // 테두리 선 = 마스크 - 물리 1 px 깎은 마스크, 빛은 프레임마다 입힘
        let erode = g.effect(&CLSID_D2D1Morphology)?;
        gfx::set_u32(&erode, D2D1_MORPHOLOGY_PROP_MODE.0 as u32, D2D1_MORPHOLOGY_MODE_ERODE.0 as u32)?;
        gfx::set_u32(&erode, D2D1_MORPHOLOGY_PROP_WIDTH.0 as u32, 3)?;
        gfx::set_u32(&erode, D2D1_MORPHOLOGY_PROP_HEIGHT.0 as u32, 3)?;
        gfx::set_input_effect(&erode, 0, &sheets)?;
        let ring = g.effect(&CLSID_D2D1Composite)?;
        gfx::set_u32(&ring, D2D1_COMPOSITE_PROP_MODE.0 as u32, D2D1_COMPOSITE_MODE_DESTINATION_OUT.0 as u32)?;
        gfx::set_input_effect(&ring, 0, &sheets)?;
        gfx::set_input_effect(&ring, 1, &erode)?;
        let rim_light = g.effect(&CLSID_D2D1AlphaMask)?;
        gfx::set_input_effect(&rim_light, 1, &ring)?;

        self.fx = Some(Effects { goo_blur, sheets, shadow, plate, rim_light, _keep: vec![flood, thresh, erode, ring] });
        Ok(())
    }

    fn jacket(&mut self, id: u32) -> Option<ID2D1Bitmap1> {
        if let Some(b) = self.jackets.get(&id) {
            return b.clone();
        }
        let path = self.catalog.jacket_dir.join(format!("{id}.webp"));
        let bmp = image::open(&path).ok().and_then(|im| {
            let im = im.to_rgba8();
            self.gfx.as_ref()?.bitmap_from_rgba(im.width(), im.height(), im.as_raw()).ok()
        });
        self.jackets.insert(id, bmp.clone());
        bmp
    }

    // ---------------------------------------------------------------- 위치 계산

    fn bar_rect(&self) -> D2D_RECT_F {
        let m = self.px(lay::MARGIN);
        let r: f32 = self.reveal.iter().map(|s| s.value.clamp(-0.15, 1.15)).sum();
        let w = self.px(lay::W) - self.px(lay::BTN_D + lay::BTN_GAP) * r;
        let h = self.px(lay::BAR_H);
        let o = self.open_s.value;
        let (sx, sy) = (0.90 + 0.10 * o, 0.70 + 0.30 * o);
        let (cx, cy) = (m + w / 2.0, m + h / 2.0);
        rect(cx - w * sx / 2.0, cy - h * sy / 2.0, cx + w * sx / 2.0, cy + h * sy / 2.0)
    }

    /// 버튼 `k`가 지금 그려지는 위치, 사슬처럼 차례로 짜내듯 나와 살짝 넘침
    /// 히스토리가 먼저 바 끝에서 나와 오른쪽 끝에 서고, 톱니가 그 안에서 나오는 동안
    /// 히스토리는 줄어드는 바를 따라 왼쪽으로 비켜남 (들어갈 땐 거꾸로)
    fn bead_now(&self, k: usize) -> (f32, f32, f32) {
        let (gx, gy, rad) = self.bead_circle(BEAD_GEAR);
        let [rh, rg] = self.reveal.map(|s| s.value);
        let x = gx - (1.0 - rh) * self.px(lay::BTN_D) * 0.65;
        let (x, r) = match k {
            BEAD_HISTORY => (x - self.px(lay::BTN_D + lay::BTN_GAP) * rg.clamp(-0.15, 1.15), rh),
            _ => (x, rg),
        };
        (x, gy, rad * (0.55 + 0.45 * r.clamp(0.0, 1.2)))
    }

    /// 버튼 `k`의 아이콘이 보이는 정도, 거의 다 나와야 (진행 0.6부터) 보이고 눌림
    /// 바나 다른 버튼 속에 숨은 버튼은 0
    fn bead_shown(&self, k: usize) -> f32 {
        ((self.reveal[k].value - 0.6) / 0.3).clamp(0.0, 1.0)
    }

    /// 버튼 `k`의 최종 (고정) 위치, 톱니가 오른쪽 끝이고 히스토리가 그 왼쪽
    fn bead_circle(&self, k: usize) -> (f32, f32, f32) {
        let r = self.px(lay::BTN_D) / 2.0;
        let x = self.px(lay::MARGIN + lay::W) - r - self.px(lay::BTN_D + lay::BTN_GAP) * (lay::BEADS - 1 - k) as f32;
        (x, self.px(lay::MARGIN) + self.px(lay::BAR_H) / 2.0, r)
    }

    fn panel_top(&self) -> f32 {
        self.px(lay::MARGIN + lay::BAR_H + lay::GAP)
    }

    /// 그릴 시트: 지금 높이에서, 나타나고 사라질 땐 시트와 내용이 같이 흐려지며 위로 비켜남
    /// 불투명도는 1/64 단위 (브러시 캐시가 안 커지게)
    fn sheet_geom(&self) -> SheetGeom {
        let alpha = (self.shown.value() * 64.0).round() / 64.0;
        let dy = -SHEET_RISE * self.scale * (1.0 - alpha);
        let top = self.panel_top() + dy;
        SheetGeom { rect: rect(self.px(lay::MARGIN), top, self.px(lay::MARGIN + lay::W), top + self.panel_h.value.max(0.0)), alpha, dy }
    }

    /// 보여 줄 게 있으면 그 높이로 나타나고 (다 사라져 있었으면 그 높이에서 바로) 시트 내용도 한 프레임에 바꿈
    /// 없으면 높이와 내용을 그대로 둔 채 사라짐, 다 사라지면 높이 0 (`render`)
    /// 나타나다 사라지거나 그 반대로 바뀌어도 지금 불투명도·자리에서 이어짐
    fn retarget_panel(&mut self) {
        let to = self.panel_target();
        if to <= 0.0 {
            self.shown.go(0.0, SHEET_OUT_SECS, Ease::STANDARD);
            return;
        }
        if self.shown.target() <= 0.0 && self.shown.done() {
            self.panel_h.snap(to);
        }
        self.panel_h.target = to;
        self.shown.go(1.0, SHEET_IN_SECS, Ease::DECEL);
        let sheet = self.sheet_now();
        if matches!(sheet, Sheet::Results | Sheet::History) && sheet != self.sel_of {
            self.sel_of = sheet;
            self.reset_rows();
        }
        self.sheet = sheet;
    }

    /// 지금 보기가 시트에 그릴 내용
    fn sheet_now(&self) -> Sheet {
        match self.view {
            View::Settings => Sheet::Settings,
            View::History => Sheet::History,
            View::Results if self.card().is_some() => Sheet::Card,
            View::Results => Sheet::Results,
        }
    }

    /// 시트 `s`의 줄
    fn rows_of(&self, s: Sheet) -> &[Row] {
        if s == Sheet::History { &self.history } else { &self.results }
    }

    /// 지금 보기의 줄, 키·마우스·높이 계산용 (그리기는 `sheet`의 줄)
    fn rows(&self) -> &[Row] {
        self.rows_of(self.sheet_now())
    }

    fn cond_rect(&self) -> D2D_RECT_F {
        let b = self.bar_rect();
        let cy = (b.top + b.bottom) / 2.0;
        rect(b.right - self.px(8.0 + lay::COND_W), cy - self.px(15.0), b.right - self.px(8.0), cy + self.px(15.0))
    }

    fn text_origin(&self) -> (f32, f32) {
        let b = self.bar_rect();
        (b.left + self.px(46.0), b.top)
    }

    fn settings_layout(&self) -> SettingsLayout {
        let top = self.panel_top();
        let (l, r) = (self.px(lay::MARGIN + lay::INSET), self.px(lay::MARGIN + lay::W - lay::INSET));
        let y = |v: f32| top + self.px(v);
        // 글자 기준 한 리듬 (글자는 텍스트 박스보다 ~2 아래): 위 끝에서 제목까지 20 (모서리 반지름,
        // 둥근 모서리에 안 붙게), 첫 라벨까지 20, 라벨에서 컨트롤까지 8, 컨트롤에서 다음 라벨까지 16,
        // 묶음 사이 24 (구분선 없이 따로 묶음), 맨 아래 버튼 아래는 오른쪽 여백과 같은 16
        SettingsLayout {
            title: y(18.0),
            labels: [y(52.0), y(118.0)],
            mode: rect(l, y(72.0), r, y(104.0)),
            diff: rect(l, y(138.0), r, y(170.0)),
            list_title: y(192.0),
            rescan: rect(r - self.px(88.0), y(194.0), r, y(224.0)), // 두 줄 묶음의 가운데
            list_note: y(212.0),
            // 단축키도 따로 묶음 (목록에서 24), 버튼은 글 좌우로 14, 다시 인식 버튼보다 좁진 않게
            hotkey_title: y(246.0),
            hotkey: rect(r - (self.measure(&self.hotkey_text(), |f| &f.item) + self.px(28.0)).max(self.px(88.0)).round(), y(248.0), r, y(278.0)),
            hotkey_note: y(266.0),
            // 같은 두 줄 묶음, 토글은 버튼 줄(302..332) 가운데
            keep_title: y(300.0),
            keep: rect(r - self.px(lay::TOGGLE_W), y(317.0 - lay::TOGGLE_H / 2.0), r, y(317.0 + lay::TOGGLE_H / 2.0)),
            keep_note: y(320.0),
        }
    }

    /// 단축키 버튼 글, 새 조합을 기다리는 중이면 안내
    fn hotkey_text(&self) -> String {
        if self.hotkey_wait.is_some() {
            "새 단축키를 눌러 주세요".into()
        } else {
            self.hotkey.label()
        }
    }

    /// `font`로 쓴 `text`의 너비, 끝 공백 포함 (글꼴이 아직 없으면 0)
    fn measure(&self, text: &str, font: impl Fn(&Fonts) -> &IDWriteTextFormat) -> f32 {
        let (Some(g), Some(f)) = (&self.gfx, &self.fonts) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        match g.layout(text, font(f), 4000.0, self.px(lay::BAR_H)) {
            Ok(l) if unsafe { l.GetMetrics(&mut m) }.is_ok() => m.widthIncludingTrailingWhitespace,
            _ => 0.0,
        }
    }

    fn card_button(&self) -> D2D_RECT_F {
        let top = self.panel_top();
        let r = self.px(lay::MARGIN + lay::W - lay::INSET);
        let cy = top + self.px(lay::CARD_H) / 2.0;
        rect(r - self.px(lay::CARD_BTN_W), cy - self.px(16.0), r, cy + self.px(16.0))
    }

    /// 캐럿 자리 (조합 중이면 조합 글자의 앞), IME 후보 창을 여기에 둠
    fn caret_pos(&self) -> (f32, f32) {
        let (x0, y0) = self.text_origin();
        let x = self.input_layout().map_or(0.0, |l| text_x(&l, &self.query(), self.edit.caret()));
        (x0 - self.text_scroll + x, y0 + self.px(lay::BAR_H) * 0.25)
    }

    /// 입력 글 (조합 중인 글자 포함)의 레이아웃, 가로로 안 자름
    fn input_layout(&self) -> Option<IDWriteTextLayout> {
        let (g, f) = (self.gfx.as_ref()?, self.fonts.as_ref()?);
        g.layout(&self.query(), &f.input, 100_000.0, self.px(lay::BAR_H)).ok()
    }

    /// 입력 칸의 `x` (클라이언트 px)에 가장 가까운 글자 경계, 검색어의 바이트 위치
    fn text_index(&self, x: f32) -> usize {
        let Some(l) = self.input_layout() else { return 0 };
        let (tx, _) = self.text_origin();
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if unsafe { l.HitTestPoint(x - tx + self.text_scroll, 1.0, &mut trailing, &mut inside, &mut m) }.is_err() {
            return 0;
        }
        let u = m.textPosition + if trailing.as_bool() { m.length } else { 0 };
        byte_at_u16(&self.query(), u as usize)
    }

    /// 입력 칸 커서 모양 (I 빔)을 쓸 때
    pub fn wants_ibeam(&self) -> bool {
        self.is_open() && (self.dragging || self.hover == Hit::Text)
    }

    fn visible_rows(&self) -> usize {
        self.rows().len().min(lay::ROWS)
    }

    /// 지금 보기의 패널 높이, 보여 줄 게 없으면 0 (줄이 없으면 문구 한 줄)
    fn panel_target(&self) -> f32 {
        match self.sheet_now() {
            Sheet::Settings => self.px(lay::SETTINGS_H),
            Sheet::Card => self.px(lay::CARD_H),
            _ if !self.listing() => 0.0,
            _ => self.px(lay::PAD * 2.0 + lay::ROW_H * self.visible_rows().max(1) as f32),
        }
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        // 버튼은 지금 그려진 원으로 판정, 아이콘이 보이는 버튼만 (`bead_shown`), 둘 다 걸리면 가운데가 가까운 쪽
        let bead = [(BEAD_HISTORY, Hit::History), (BEAD_GEAR, Hit::Gear)]
            .into_iter()
            .filter_map(|(k, h)| {
                let (bx, by, br) = self.bead_now(k);
                let d2 = (x - bx).powi(2) + (y - by).powi(2);
                (self.bead_shown(k) > 0.0 && d2 <= br * br).then_some((d2, h))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, h)) = bead {
            return h;
        }
        let bar = self.bar_rect();
        if inside(&bar, x, y) {
            return if x < self.cond_rect().left - self.px(8.0) { Hit::Text } else { Hit::Inside };
        }
        // 그려진 대로 판정: 시트는 비켜난 자리, 내용은 같이 비켜난 자리 (`dy`)
        // 반 넘게 보이기 전엔 줄·버튼을 못 누름
        let sg = self.sheet_geom();
        if !inside(&sg.rect, x, y) {
            return Hit::None;
        }
        if sg.alpha < 0.5 {
            return Hit::Inside;
        }
        let y = y - sg.dy;
        if self.view == View::Settings {
            let sl = self.settings_layout();
            if inside(&sl.mode, x, y) {
                return Hit::ModeSeg(segment_at(&sl.mode, x));
            }
            if inside(&sl.diff, x, y) {
                return Hit::DiffSeg(segment_at(&sl.diff, x));
            }
            if inside(&sl.rescan, x, y) {
                return Hit::Rescan;
            }
            if inside(&sl.hotkey, x, y) {
                return Hit::Hotkey;
            }
            // 토글은 작아서 둘레를 조금 더, 위아래는 버튼 줄만큼
            let k = sl.keep;
            if inside(&rect(k.left - self.px(6.0), k.top - self.px(3.0), k.right, k.bottom + self.px(3.0)), x, y) {
                return Hit::KeepQuery;
            }
            return Hit::Inside;
        }
        if self.card().is_some() {
            if inside(&self.card_button(), x, y) {
                return Hit::CardButton;
            }
            return Hit::Inside;
        }
        // 검색어를 지운 뒤 사라지는 줄은 호버·클릭 대상 아님
        // 스크롤 중엔 줄이 아직 `scroll` 위치가 아니라 그려진 위치에 있음
        if self.listing() {
            if let Some(i) = self.row_at(y, self.scroll_s.value) {
                return Hit::Row(i);
            }
        }
        Hit::Inside
    }

    /// 목록이 `off` px 스크롤됐을 때 내용 자리 `y`의 결과 줄 (없으면 None)
    fn row_at(&self, y: f32, off: f32) -> Option<usize> {
        let pitch = self.px(lay::ROW_H);
        let top = self.panel_top() + self.px(lay::PAD);
        if y < top || y >= top + pitch * self.visible_rows() as f32 {
            return None;
        }
        let i = ((y - top + off) / pitch) as usize;
        (i < self.rows().len()).then_some(i)
    }

    // ---------------------------------------------------------------- 상태 변경

    /// 보여 줄 내용이 있으면 한 프레임에 그 내용으로, 없으면 직전 내용 그대로 사라짐 (`retarget_panel`)
    /// 히스토리는 열 때마다 최근 곡으로 다시 채움
    fn set_view(&mut self, v: View) {
        if self.view == v {
            return;
        }
        self.view = v;
        self.end_hotkey_wait();
        if v == View::History {
            self.fill_history();
        }
        if v == View::Results {
            self.kb_focus = None;
            // 키로 나간 경우: 커서가 떠날 때처럼 두 버튼이 들어감
            if !self.in_zone && self.leave_since.is_none() {
                self.leave_since = Some(Instant::now());
            }
        }
        self.retarget_panel();
    }

    fn toggle_settings(&mut self) {
        self.set_view(if self.view == View::Settings { View::Results } else { View::Settings });
    }

    fn toggle_history(&mut self) {
        self.set_view(if self.view == View::History { View::Results } else { View::History });
    }

    /// 새 단축키를 기다림 (설정 보기에서만, 거기선 조합 중인 글자가 없음)
    /// 한글 IME가 키를 가져가지 않게 그동안 입력 컨텍스트를 뗌
    fn start_hotkey_wait(&mut self) {
        if self.hotkey_wait.is_some() {
            return;
        }
        self.hotkey_wait = Some(unsafe { ImmAssociateContext(self.hwnd, HIMC::default()) });
        self.hotkey_note = None;
    }

    /// 기다림을 끝내고 입력 컨텍스트를 되돌림 (받음, 취소, 다른 데를 누름, 포커스를 잃음, 닫힘)
    fn end_hotkey_wait(&mut self) {
        if let Some(imc) = self.hotkey_wait.take() {
            unsafe {
                ImmAssociateContext(self.hwnd, imc);
            }
        }
        self.hotkey_note = None;
    }

    pub fn waiting_hotkey(&self) -> bool {
        self.hotkey_wait.is_some()
    }

    /// 기다리는 중 누른 조합: 받을 수 있으면 `Action::Hotkey`, 아니면 이유를 잠깐 보여 주고 계속 기다림
    /// 지금 단축키를 그대로 누르면 바꿀 것 없이 끝냄
    pub fn take_hotkey(&mut self, mods: HOT_KEY_MODIFIERS, vk: VIRTUAL_KEY) -> Action {
        match (Hotkey { mods, vk }).check() {
            Ok(k) if k == self.hotkey => {
                self.end_hotkey_wait();
                Action::None
            }
            Ok(k) => Action::Hotkey(k),
            Err(why) => {
                self.hotkey_note = Some((why, Instant::now()));
                Action::None
            }
        }
    }

    /// 새 단축키를 등록해 본 결과, 다른 프로그램이 쓰고 있으면 이유를 보여 주고 계속 기다림
    pub fn hotkey_result(&mut self, k: Hotkey, ok: bool) {
        if ok {
            self.hotkey = k;
            self.end_hotkey_wait();
        } else {
            self.hotkey_note = Some(("다른 프로그램이 쓰고 있는 단축키예요", Instant::now()));
        }
    }

    /// 입력한 글이 바뀜: 글은 바로 보이고 결과는 입력이 멈춘 뒤에 바뀜 (`TIMER_SEARCH`)
    /// 검색어가 비어도 기다림, Enter·Esc·화살표로 글자를 확정하면 IME가 먼저 조합을 끝내고
    /// (글에서 그 글자가 빠짐) 그다음에 입력으로 넘겨주기 때문
    fn requery(&mut self, edit: Edit) {
        self.caret_t0 = Instant::now();
        // 입력하면 글이 바뀌고, 그대로 확정된 글자는 글을 안 바꿈
        // IME가 먼저 조합을 끝낸 경우도 같음 (끝내기 전 글과 비교)
        let was = match (edit, self.ended.take()) {
            (Edit::Result, Some(before_end)) => before_end,
            _ => std::mem::take(&mut self.seen),
        };
        self.seen = self.query();
        let typing = match edit {
            Edit::Ended => {
                self.ended = Some(was);
                false
            }
            _ => self.seen != was,
        };
        if typing && matches!(self.view, View::Settings | View::History) {
            // 설정·히스토리에서 입력하면 그 글로 결과 화면으로 돌아감
            // 설정이 쓰는 키로 확정된 글자나 아직 기다리는 검색이면 그대로 둠
            self.search_now();
            if !self.searched.is_empty() {
                self.set_view(View::Results);
            }
            return;
        }
        if self.query() == self.searched {
            // 쳤다가 지웠거나 글자가 그대로 확정됨: 결과와 선택 유지
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
            }
            return;
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SEARCH, SEARCH_DELAY_MS, None); // 기다림을 처음부터 다시
        }
    }

    /// 키로 다 지웠으면 결과를 기다리지 않고 바로 사라지게 함
    /// 글이 잠깐 비는 건 IME가 글자를 넘길 때뿐 (`requery`)
    fn fade_if_cleared(&mut self) {
        if self.query().is_empty() {
            self.search_now();
        }
    }

    /// 결과를 입력한 글에 맞춤, 입력이 멈출 때와 결과를 쓰기 전 (Enter, 화살표, 클릭, 휠)
    /// 바뀌었으면 true
    fn search_now(&mut self) -> bool {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
        let q = self.query();
        if q == self.searched {
            return false;
        }
        // 검색어를 지우면 사라지는 동안 지난 결과를 시트에 남겨 둠
        if !q.trim().is_empty() {
            self.results = self.find(&q);
            self.bpm_rows = Filter::parse(&q).is_some_and(|f| f.bpm.is_some());
            if self.sel_of == Sheet::Results {
                self.reset_rows();
            }
        }
        self.searched = q;
        self.retarget_panel();
        true
    }

    /// 검색어로 찾은 줄: 조건 검색이면 무작위 줄과 맞는 줄 전부, 아니면 잘 맞는 곡
    /// 레벨 조건이 있으면 그 패턴, BPM 조건만이면 곡 (설정의 모드·난이도로 이동)
    fn find(&self, q: &str) -> Vec<Row> {
        let Some(f) = Filter::parse(q) else {
            return self.catalog.search(q, LIMIT, self.owned.as_ref()).into_iter().map(Row::Song).collect();
        };
        let mode = f.level.and_then(|l| l.mode).unwrap_or(self.mode_choice);
        let row = |i| match f.level {
            Some(l) => Row::Chart(i, mode, l.diff),
            None => Row::Song(i),
        };
        let hits = self.catalog.filter(&f, mode, self.owned.as_ref());
        let random = (!hits.is_empty()).then_some(Row::Random);
        random.into_iter().chain(hits.into_iter().map(row)).collect()
    }

    /// 히스토리 줄: 최근 고른 곡 중 지금 화면 모드에서 고를 수 있는 곡
    fn fill_history(&mut self) {
        self.history = self.catalog.pick_out(&self.recent, self.owned.as_ref()).into_iter().map(Row::Song).collect();
        if self.sel_of == Sheet::History {
            self.reset_rows();
        }
    }

    /// 새 결과: 첫 줄 선택, 맨 위부터, 강조도 바로 그 줄로
    fn reset_rows(&mut self) {
        self.selected = 0;
        self.scroll = 0;
        self.scroll_s.snap(0.0);
        self.wheel_acc = 0;
        self.retarget_selection(true);
    }

    /// 결과 줄을 고를 수 있음: 검색 결과나 히스토리, 검색어를 지운 뒤 사라지는 중인 줄은 아님
    fn listing(&self) -> bool {
        !self.searched.trim().is_empty() || self.view == View::History
    }

    fn row_y(&self, i: usize) -> f32 {
        self.px(lay::PAD) + self.px(lay::ROW_H) * i as f32
    }

    /// 강조를 선택된 줄 자리로 옮김, 높이는 그대로 미끄러짐
    fn retarget_selection(&mut self, snap: bool) {
        let top = self.row_y(self.selected.saturating_sub(self.scroll));
        if snap {
            self.sel_y.snap(top);
        } else {
            self.sel_y.target = top;
        }
    }

    /// 고를 줄이 있을 때만 (사라지는 시트는 그대로 둠)
    fn move_sel(&mut self, d: i32) {
        if !self.listing() || self.rows().is_empty() {
            return;
        }
        let n = self.rows().len() as i32;
        let ns = (self.selected as i32 + d).clamp(0, n - 1) as usize;
        if ns == self.selected {
            return;
        }
        self.selected = ns;
        if self.selected < self.scroll {
            self.set_scroll(self.selected);
        } else if self.selected >= self.scroll + lay::ROWS {
            self.set_scroll(self.selected + 1 - lay::ROWS);
        }
        self.retarget_selection(false);
    }

    /// 휠: 목록을 `d`줄 스크롤, 강조는 자기 자리를 지키거나 커서 아래 줄로 감
    /// 그래서 선택은 늘 강조가 보여주는 줄, 사라지는 시트는 그대로 둠
    fn scroll_by(&mut self, d: i32, cursor: (f32, f32)) {
        if !self.listing() {
            return;
        }
        let max = self.rows().len().saturating_sub(lay::ROWS) as i32;
        let ns = (self.scroll as i32 + d).clamp(0, max) as usize;
        if ns == self.scroll {
            return;
        }
        let slot = self.selected.saturating_sub(self.scroll);
        self.set_scroll(ns);
        self.hover = self.hit(cursor.0, cursor.1);
        // 멈췄을 때 커서 아래에 오는 줄
        let dy = self.sheet_geom().dy;
        let under = match self.hover {
            Hit::Row(_) => self.row_at(cursor.1 - dy, self.scroll_s.target),
            _ => None,
        };
        self.selected = under.unwrap_or(ns + slot);
        self.retarget_selection(false);
    }

    fn set_scroll(&mut self, s: usize) {
        self.scroll = s;
        self.scroll_s.target = s as f32 * self.px(lay::ROW_H);
    }

    /// `i`번째 줄을 고름, 무작위 줄이면 하나를 뽑음
    fn choose(&mut self, i: usize) -> Action {
        if !self.listing() {
            return Action::None; // 검색어를 지운 뒤 남은 결과
        }
        let row = match self.rows().get(i) {
            Some(Row::Random) => self.roll(),
            r => r.copied(),
        };
        match row {
            Some(Row::Song(s)) => Action::Pick(self.catalog.songs[s].id, None),
            Some(Row::Chart(s, m, d)) => Action::Pick(self.catalog.songs[s].id, Some((m, d))),
            _ => Action::None,
        }
    }

    /// 무작위 줄: 남은 줄 중 하나를 고르게 뽑음 (`Rolls`), 무작위 줄은 검색 결과에만 있음
    fn roll(&mut self) -> Option<Row> {
        self.rolls.draw(&self.results, self.rng.next())
    }

    /// 검색어 유지를 바꿈, 노브와 색이 따로 움직임
    fn set_keep_query(&mut self, on: bool) {
        self.keep_query = on;
        let v = on as u8 as f32;
        self.knob.go(v, KNOB_SECS, KNOB_EASE);
        self.knob_tint.go(v, QUICK_SECS, QUICK_EASE);
    }

    /// 토글에서 손을 뗌, 노브가 원으로 돌아옴
    fn release_toggle(&mut self) {
        if std::mem::take(&mut self.keep_down) {
            self.knob_press.go(1.0, QUICK_SECS, QUICK_EASE);
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    fn snap_segments(&mut self) {
        for (k, idx) in [self.mode_choice.index(), self.diff_choice.index()].into_iter().enumerate() {
            self.seg[k].snap(idx as f32);
        }
    }

    /// 손잡이를 지금 선택으로 보냄, 한 덩어리로 처음엔 빠르게 미끄러지다 부드럽게 멈춤
    /// 라벨 색도 손잡이를 따라감
    fn retarget_segments(&mut self) {
        for (k, idx) in [self.mode_choice.index(), self.diff_choice.index()].into_iter().enumerate() {
            let to = idx as f32;
            if (self.seg[k].target() - to).abs() < 1e-3 {
                continue;
            }
            self.seg[k].go(to, SEG_SECS, Ease::OUT);
        }
    }

    fn cycle_mode(&mut self, d: i32) {
        let i = (self.mode_choice.index() as i32 + d).rem_euclid(4) as usize;
        self.set_mode(Mode::ALL[i]);
    }

    /// 모드를 정하지 않은 레벨 조건 검색은 설정 모드의 패턴이라 다시 찾음
    fn set_mode(&mut self, m: Mode) {
        self.mode_choice = m;
        if Filter::parse(&self.searched).and_then(|f| f.level).is_some_and(|l| l.mode.is_none()) {
            self.searched.clear();
            self.search_now();
        }
    }

    fn cycle_diff(&mut self, d: i32) {
        let i = (self.diff_choice.index() as i32 + d).rem_euclid(4) as usize;
        self.diff_choice = Diff::ALL[i];
    }

    /// 두 버튼용 호버 머무름·떠남 시간 처리, 앞 버튼이 먼저 움직이고 다른 버튼은 시차 뒤에 따라감
    fn update_reveal(&mut self, now: Instant) {
        if let Some((at, k, to)) = self.reveal_follow {
            if now >= at {
                self.reveal[k].target = to;
                self.reveal_follow = None;
            }
        }
        let mut want = self.beads_out;
        if let Some(t) = self.hover_since {
            if now - t >= Duration::from_millis(100) {
                want = true;
                self.hover_since = None;
            }
        }
        if let Some(t) = self.leave_since {
            if now - t >= Duration::from_millis(400) {
                want = false;
                self.leave_since = None;
            }
        }
        if matches!(self.view, View::Settings | View::History) {
            want = true;
        }
        if self.closing {
            return;
        }
        if want == self.beads_out {
            return;
        }
        self.beads_out = want;
        let (lead, follow, to) = if want { (BEAD_HISTORY, BEAD_GEAR, 1.0) } else { (BEAD_GEAR, BEAD_HISTORY, 0.0) };
        self.reveal[lead].target = to;
        self.reveal_follow = Some((now + BEAD_STAGGER, follow, to));
        if want {
            self.icon_alpha.go_delayed(1.0, 0.09, 0.12, Ease::STANDARD);
        } else {
            self.icon_alpha.go(0.0, 0.05, Ease::STANDARD);
        }
    }

    // ---------------------------------------------------------------- 입력

    /// 메시지를 처리했으면 Some(action)
    pub fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<(isize, Action)> {
        match msg {
            WM_IME_SETCONTEXT => {
                // 조합 중인 글자는 직접 그림, 후보 창 (한자)은 남겨 둠
                let lp = LPARAM(lp.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
                let r = unsafe { DefWindowProcW(self.hwnd, msg, wp, lp) };
                Some((r.0, Action::None))
            }
            WM_IME_STARTCOMPOSITION => {
                // 선택이 있으면 먼저 지우고 그 자리에서 조합 (검색은 이어 오는 조합 메시지가 함)
                self.edit.delete_selection();
                self.place_candidate_window();
                Some((0, Action::None))
            }
            WM_IME_COMPOSITION => {
                let flags = lp.0 as u32;
                unsafe {
                    let himc = ImmGetContext(self.hwnd);
                    if flags & GCS_RESULTSTR.0 != 0 {
                        let r = ime_string(himc, GCS_RESULTSTR);
                        self.edit.insert(&r);
                        self.preedit.clear();
                    }
                    if flags & GCS_COMPSTR.0 != 0 {
                        self.preedit = ime_string(himc, GCS_COMPSTR);
                    } else if flags & GCS_RESULTSTR.0 == 0 {
                        self.preedit.clear();
                    }
                    let _ = ImmReleaseContext(self.hwnd, himc);
                }
                self.requery(if flags & GCS_RESULTSTR.0 != 0 { Edit::Result } else { Edit::Typed });
                self.place_candidate_window();
                Some((0, Action::None))
            }
            WM_IME_ENDCOMPOSITION => {
                if !self.preedit.is_empty() {
                    self.preedit.clear();
                    self.requery(Edit::Ended);
                }
                Some((0, Action::None))
            }
            WM_CHAR => {
                // BMP 밖 글자 (이모지)는 UTF-16 두 조각으로 옴, 앞 조각을 들고 있다가 합침 (짝이 안 맞으면 버림)
                let c = wp.0 as u32;
                let ch = match c {
                    0xD800..=0xDBFF => {
                        self.high_surrogate = Some(c as u16);
                        None
                    }
                    0xDC00..=0xDFFF => self.high_surrogate.take().and_then(|h| char::decode_utf16([h, c as u16]).next()?.ok()),
                    _ => {
                        self.high_surrogate = None;
                        char::from_u32(c).filter(|_| c >= 0x20 && c != 0x7F)
                    }
                };
                if let Some(ch) = ch {
                    self.edit.insert(ch.encode_utf8(&mut [0; 4]));
                    self.requery(Edit::Typed);
                }
                Some((0, Action::None))
            }
            WM_SYSCHAR => Some((0, Action::None)), // Alt+글자, 경고음 안 나게
            // 새 단축키를 기다리는 중: 수식 키만 누른 건 기다리고, Esc는 취소, 나머지는 조합으로 받음
            // Alt를 뗀 것도 여기서 끝내야 창 메뉴로 안 넘어감
            WM_KEYDOWN | WM_SYSKEYDOWN if self.hotkey_wait.is_some() => {
                // 이 키로 생긴 글자는 버림, 받은 뒤에 오는 Ctrl+Space의 공백 같은 게 검색어로 안 들어가게
                unsafe {
                    let _ = PeekMessageW(&mut MSG::default(), Some(self.hwnd), WM_CHAR, WM_CHAR, PM_REMOVE);
                }
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let repeat = lp.0 & (1 << 30) != 0; // 누르고 있어 반복되는 입력
                let modifier = matches!(
                    vk,
                    VK_CONTROL | VK_LCONTROL | VK_RCONTROL | VK_MENU | VK_LMENU | VK_RMENU | VK_SHIFT | VK_LSHIFT | VK_RSHIFT | VK_LWIN | VK_RWIN
                );
                let act = if repeat || modifier {
                    Action::None
                } else if vk == VK_ESCAPE {
                    self.end_hotkey_wait();
                    Action::None
                } else {
                    let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
                    let held = [(VK_CONTROL, MOD_CONTROL), (VK_MENU, MOD_ALT), (VK_SHIFT, MOD_SHIFT), (VK_LWIN, MOD_WIN), (VK_RWIN, MOD_WIN)];
                    let mods = held.iter().filter(|&&(k, _)| down(k)).fold(0, |a, (_, m)| a | m.0);
                    self.take_hotkey(HOT_KEY_MODIFIERS(mods), vk)
                };
                Some((0, act))
            }
            WM_SYSKEYUP if self.hotkey_wait.is_some() => Some((0, Action::None)),
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let mut vk = VIRTUAL_KEY(wp.0 as u16);
                if vk == VK_PROCESSKEY {
                    let real = unsafe { ImmGetVirtualKey(self.hwnd) };
                    vk = VIRTUAL_KEY(real as u16);
                    if matches!(vk, VK_RETURN | VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_DELETE | VK_ESCAPE | VK_PRIOR | VK_NEXT | VK_TAB) {
                        // 한국어 Windows 관례: Enter·화살표는 글자를 확정하고 동작도 함
                        self.complete_composition();
                    } else {
                        return None; // IME가 처리하는 키 (자모, 조합 중 Backspace 등)
                    }
                }
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let alt = msg == WM_SYSKEYDOWN || unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                // 어디서나 되는 단축키
                if alt && vk == VIRTUAL_KEY(b'M' as u16) {
                    self.cycle_mode(if shift { -1 } else { 1 });
                    return Some((0, Action::Choice));
                }
                if alt && (vk == VK_LEFT || vk == VK_RIGHT) {
                    self.cycle_diff(if vk == VK_LEFT { -1 } else { 1 });
                    return Some((0, Action::Choice));
                }
                if vk == VK_TAB {
                    self.toggle_settings();
                    return Some((0, Action::None));
                }
                // 입력 칸 편집 단축키는 어느 보기에서나, 조합 중인 글자는 먼저 확정
                if ctrl && !shift && !alt && matches!(vk.0 as u8, b'A' | b'C' | b'X' | b'V') {
                    self.complete_composition();
                    match vk.0 as u8 {
                        b'A' => self.edit.select_all(),
                        b'C' => {
                            self.copy();
                        }
                        b'X' => self.cut(),
                        _ => self.paste(),
                    }
                    return Some((0, Action::None));
                }
                let act = if self.view == View::Settings {
                    let f = self.kb_focus.unwrap_or(0);
                    match vk {
                        VK_ESCAPE => {
                            self.set_view(View::Results);
                            Action::None
                        }
                        VK_UP => {
                            self.kb_focus = Some(f.saturating_sub(1));
                            Action::None
                        }
                        VK_DOWN => {
                            // 처음 누른 화살표는 첫 컨트롤로
                            self.kb_focus = Some(if self.kb_focus.is_some() { (f + 1).min(4) } else { 0 });
                            Action::None
                        }
                        VK_LEFT | VK_RIGHT => {
                            let d = if vk == VK_LEFT { -1 } else { 1 };
                            self.kb_focus = Some(f);
                            match f {
                                0 => {
                                    self.cycle_mode(d);
                                    Action::Choice
                                }
                                1 => {
                                    self.cycle_diff(d);
                                    Action::Choice
                                }
                                // 토글은 왼쪽이 끔, 오른쪽이 켬
                                4 if (d > 0) != self.keep_query => {
                                    self.set_keep_query(d > 0);
                                    Action::Choice
                                }
                                _ => Action::None,
                            }
                        }
                        VK_RETURN if f == 2 => Action::Scan,
                        VK_RETURN if f == 3 => {
                            self.start_hotkey_wait();
                            Action::None
                        }
                        VK_RETURN if f == 4 => {
                            self.set_keep_query(!self.keep_query);
                            Action::Choice
                        }
                        VK_BACK => {
                            self.edit.backspace(false);
                            self.requery(Edit::Typed);
                            Action::None
                        }
                        _ => Action::None,
                    }
                } else {
                    if matches!(vk, VK_RETURN | VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT) {
                        self.search_now(); // 입력한 글에 맞는 결과로 동작함
                    }
                    match vk {
                        VK_ESCAPE if self.view == View::History => {
                            self.set_view(View::Results);
                            Action::None
                        }
                        VK_ESCAPE => Action::Hide,
                        VK_RETURN if self.card().is_some() => Action::Scan,
                        VK_RETURN => self.choose(self.selected),
                        VK_UP => {
                            self.move_sel(-1);
                            Action::None
                        }
                        VK_DOWN => {
                            self.move_sel(1);
                            Action::None
                        }
                        VK_PRIOR => {
                            self.move_sel(-(lay::ROWS as i32));
                            Action::None
                        }
                        VK_NEXT => {
                            self.move_sel(lay::ROWS as i32);
                            Action::None
                        }
                        // 캐럿만 움직이면 다시 검색하지 않음, Ctrl은 단어 단위, Shift는 선택
                        // 조합 중인 글자는 먼저 확정 (그래야 그 자리에 남음)
                        VK_LEFT | VK_RIGHT | VK_HOME | VK_END => {
                            self.complete_composition();
                            match vk {
                                VK_LEFT => self.edit.left(ctrl, shift),
                                VK_RIGHT => self.edit.right(ctrl, shift),
                                VK_HOME => self.edit.home(shift),
                                _ => self.edit.end(shift),
                            }
                            self.caret_t0 = Instant::now();
                            Action::None
                        }
                        // 조합 중 Backspace는 IME가 자모를 지움, Delete는 확정한 뒤 지움
                        VK_BACK | VK_DELETE if vk == VK_DELETE || self.preedit.is_empty() => {
                            self.complete_composition();
                            // 지운 게 없으면 결과를 새로 기다리지 않음
                            let gone = if vk == VK_BACK { self.edit.backspace(ctrl) } else { self.edit.delete(ctrl) };
                            if gone {
                                self.requery(Edit::Typed);
                                self.fade_if_cleared();
                            }
                            Action::None
                        }
                        _ => Action::None,
                    }
                };
                Some((0, act))
            }
            WM_MOUSEMOVE => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                if self.dragging {
                    self.complete_composition(); // 위치는 조합 글자가 없는 글 기준
                    self.edit.move_to(self.text_index(x), true);
                    self.caret_t0 = Instant::now();
                    return Some((0, Action::None));
                }
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    self.tracking = true;
                }
                self.hover = self.hit(x, y);
                // 드러내는 영역: 바 오른쪽 부분 + 두 버튼이 나오는 자리
                let bar = self.bar_rect();
                let (gx, _, gr) = self.bead_circle(BEAD_GEAR);
                let near = |k: usize| {
                    let (bx, by, br) = self.bead_circle(k);
                    (x - bx).powi(2) + (y - by).powi(2) <= (br + self.px(6.0)).powi(2)
                };
                let in_zone = (y >= bar.top - self.px(6.0)
                    && y <= bar.bottom + self.px(6.0)
                    && x >= bar.left + (bar.right - bar.left) * 0.55
                    && x <= gx + gr + self.px(6.0))
                    || near(BEAD_HISTORY)
                    || near(BEAD_GEAR);
                self.in_zone = in_zone;
                if in_zone {
                    self.leave_since = None;
                    if !self.beads_out && self.hover_since.is_none() {
                        self.hover_since = Some(Instant::now());
                    }
                } else {
                    self.hover_since = None;
                    if self.hover == Hit::None {
                        if self.beads_out && self.leave_since.is_none() {
                            self.leave_since = Some(Instant::now());
                        }
                    } else {
                        self.leave_since = None;
                    }
                }
                // Windows는 커서가 가만히 있어도 이동 메시지를 보냄 (띄운 뒤, 입력·스크롤 뒤)
                // 진짜 움직였을 때만 줄을 호버함, 아니면 선택이 커서 아래로 되돌아감
                let pos = (x as i32, y as i32);
                if self.last_mouse.replace(pos) == Some(pos) {
                    return Some((0, Action::None));
                }
                if let Hit::Row(i) = self.hover {
                    // 스크롤 중 목록 밖으로 나가는 줄엔 강조 자리가 없음
                    if i != self.selected && (self.scroll..self.scroll + lay::ROWS).contains(&i) {
                        self.selected = i;
                        self.retarget_selection(false);
                    }
                }
                Some((0, Action::None))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.in_zone = false;
                self.hover = Hit::None;
                self.hover_since = None;
                if self.beads_out && self.leave_since.is_none() {
                    self.leave_since = Some(Instant::now());
                }
                Some((0, Action::None))
            }
            WM_MOUSEWHEEL => {
                // 한 칸에 한 줄, 정밀 휠과 터치패드는 모아서 칸으로 셈
                let d = ((wp.0 >> 16) as i16) as i32;
                if matches!(self.view, View::Results | View::History) {
                    if d.signum() != self.wheel_acc.signum() {
                        self.wheel_acc = 0; // 방향 바뀜
                    }
                    self.wheel_acc += d;
                    let rows = self.wheel_acc / WHEEL_DELTA as i32;
                    self.wheel_acc -= rows * WHEEL_DELTA as i32;
                    if rows != 0 {
                        self.search_now();
                        let mut p = POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 }; // 화면 좌표
                        let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut p) };
                        self.scroll_by(-rows, (p.x as f32, p.y as f32));
                    }
                }
                Some((0, Action::None))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                let hit = self.hit(x, y);
                // 새 단축키를 기다리는 중 누르면 기다림을 끝냄, 단축키 버튼을 다시 누른 거면 그걸로 끝
                if self.hotkey_wait.is_some() {
                    self.end_hotkey_wait();
                    if hit == Hit::Hotkey {
                        return Some((0, Action::None));
                    }
                }
                let act = match hit {
                    Hit::Text => {
                        // 조합 중인 글자는 확정하고, Shift를 누른 채면 선택을 넓힘
                        self.complete_composition();
                        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                        self.edit.move_to(self.text_index(x), shift);
                        self.caret_t0 = Instant::now();
                        self.dragging = true;
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        Action::None
                    }
                    Hit::Gear => {
                        self.toggle_settings();
                        Action::None
                    }
                    Hit::History => {
                        self.toggle_history();
                        Action::None
                    }
                    Hit::ModeSeg(i) => {
                        self.set_mode(Mode::ALL[i]);
                        self.kb_focus = None;
                        Action::Choice
                    }
                    Hit::DiffSeg(i) => {
                        self.diff_choice = Diff::ALL[i];
                        self.kb_focus = None;
                        Action::Choice
                    }
                    Hit::Rescan | Hit::CardButton => Action::Scan,
                    Hit::Hotkey => {
                        self.kb_focus = None;
                        self.start_hotkey_wait();
                        Action::None
                    }
                    // 누르는 동안 노브가 눌리고, 토글 위에서 떼면 바뀜
                    Hit::KeepQuery => {
                        self.kb_focus = None;
                        self.keep_down = true;
                        self.knob_press.go(PRESS_Y, PRESS_SECS, PRESS_EASE);
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        Action::None
                    }
                    // 예전 검색어 결과가 보이던 줄을 누르면 대신 지금 결과를 띄움
                    Hit::Row(_) if self.search_now() => Action::None,
                    Hit::Row(i) => {
                        self.selected = i;
                        self.choose(i)
                    }
                    Hit::None => Action::Hide,
                    _ => Action::None,
                };
                Some((0, act))
            }
            WM_LBUTTONUP if self.dragging => {
                self.end_drag();
                Some((0, Action::None))
            }
            WM_LBUTTONUP if self.keep_down => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                let over = self.hit(x, y) == Hit::KeepQuery;
                self.release_toggle();
                if over {
                    self.set_keep_query(!self.keep_query);
                    return Some((0, Action::Choice));
                }
                Some((0, Action::None))
            }
            WM_CAPTURECHANGED => {
                self.dragging = false;
                // 다른 데로 캡처를 뺏기면 안 바꾸고 놓음
                if std::mem::take(&mut self.keep_down) {
                    self.knob_press.go(1.0, QUICK_SECS, QUICK_EASE);
                }
                None
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    self.end_hotkey_wait();
                    // 바로 닫지 않음, 스크린샷 도구가 포커스를 가져갔을 수 있음
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_FOCUS, 80, None);
                    }
                    return Some((0, Action::None));
                }
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_FOCUS);
                }
                None
            }
            WM_TIMER => {
                if wp.0 == TIMER_FOCUS {
                    let fg = unsafe { GetForegroundWindow() };
                    if fg == self.hwnd {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), TIMER_FOCUS);
                        }
                    } else if !fg.0.is_null() && !sys::is_capture_tool(fg) {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), TIMER_FOCUS);
                        }
                        return Some((0, Action::Hide));
                    }
                    return Some((0, Action::None));
                }
                if wp.0 == TIMER_SEARCH {
                    self.search_now();
                }
                Some((0, Action::None)) // 그리는 건 호출하는 쪽 (캐럿 깜빡임도)
            }
            _ => None,
        }
    }

    /// IME에 조합을 끝내라고 (CPS_COMPLETE) 또는 버리라고 (CPS_CANCEL) 알림
    fn notify_composition(&self, how: NOTIFY_IME_INDEX) {
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, how, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
        }
    }

    /// 조합 중인 글자를 캐럿 자리에 확정, 조합이 없으면 아무것도 안 함
    fn complete_composition(&mut self) {
        if self.preedit.is_empty() {
            return;
        }
        self.notify_composition(CPS_COMPLETE);
        if !self.preedit.is_empty() {
            let p = std::mem::take(&mut self.preedit);
            self.edit.insert(&p);
            self.requery(Edit::Result);
        }
    }

    /// 선택한 글을 클립보드에, 넣었으면 true (선택이 없거나 다른 프로그램이 클립보드를 잡고 있으면 false)
    fn copy(&self) -> bool {
        let s = self.edit.selected();
        !s.is_empty() && sys::set_clipboard_text(self.hwnd, s)
    }

    /// 클립보드에 넣었을 때만 선택을 지움
    fn cut(&mut self) {
        if self.copy() && self.edit.delete_selection() {
            self.requery(Edit::Typed);
            self.fade_if_cleared();
        }
    }

    /// 클립보드 글을 한 줄로 정리해 선택 자리에 넣음
    fn paste(&mut self) {
        let Some(t) = sys::clipboard_text(self.hwnd) else { return };
        let t = clean_paste(&t);
        if t.is_empty() && self.edit.selection().is_none() {
            return;
        }
        self.edit.insert(&t);
        self.requery(Edit::Typed);
        self.fade_if_cleared();
    }

    fn end_drag(&mut self) {
        if std::mem::take(&mut self.dragging) {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    fn place_candidate_window(&self) {
        let (cx, cy) = self.caret_pos();
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let cf = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_CANDIDATEPOS,
                ptCurrentPos: POINT { x: cx as i32, y: (cy + self.px(lay::BAR_H) * 0.35) as i32 },
                rcArea: RECT::default(),
            };
            let _ = ImmSetCandidateWindow(himc, &cf);
            let comp = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x: cx as i32, y: cy as i32 }, rcArea: RECT::default() };
            let _ = ImmSetCompositionWindow(himc, &comp);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    // ---------------------------------------------------------------- 그리기

    /// 한 프레임 그림, 그래픽 장치가 사라졌으면 (`gfx::device_lost`) 바로 숨기고 리소스를 버림
    /// 다음 열기의 `prepare`가 새로 만듦
    pub fn render(&mut self) -> Result<()> {
        let r = self.draw_frame();
        if r.as_ref().is_err_and(gfx::device_lost) {
            self.hide_now();
            self.drop_gfx();
        }
        r
    }

    fn draw_frame(&mut self) -> Result<()> {
        if self.gfx.is_none() || self.fonts.is_none() || self.fx.is_none() {
            return Ok(());
        }
        let now = Instant::now();
        // 쉬다가 (커서 깜빡임만) 다시 움직이면 첫 걸음은 지난번부터 흐른 시간이 아니라 한 프레임
        let dt = if self.moving { (now - self.last).as_secs_f32() } else { 1.0 / gfx::MAX_FPS as f32 };
        self.last = now;
        self.update_reveal(now);
        self.sel_y.step(dt);
        self.scroll_s.step(dt);
        if self.scroll_s.settled() {
            self.scroll_s.snap(self.scroll_s.target); // 정수 픽셀에서 멈춰야 글자가 선명함
        }
        self.retarget_segments();
        self.retarget_panel();
        self.open_s.step(dt);
        self.reveal.iter_mut().for_each(|r| r.step(dt));
        self.panel_h.step(dt);
        if self.shown.target() <= 0.0 && self.shown.done() {
            self.panel_h.snap(0.0); // 다 사라짐, 다음엔 새 높이에서 바로 나타남
        }
        if self.closing && self.close.done() {
            self.finish_hide();
            return Ok(());
        }

        // 화면에 그릴 결과: 보이는 범위 + 스크롤 중 미끄러져 들어오는 줄
        let pitch = self.px(lay::ROW_H);
        let off = self.scroll_s.value;
        let listed = matches!(self.sheet, Sheet::Results | Sheet::History);
        let n_rows = self.rows_of(self.sheet).len();
        let shown: Vec<usize> = if listed {
            let first = (off / pitch).floor().max(0.0) as usize;
            let end = ((off / pitch + n_rows.min(lay::ROWS) as f32).ceil() as usize).min(n_rows);
            (first..end).collect()
        } else {
            Vec::new()
        };
        let ids: Vec<Option<u32>> = shown.iter().map(|&j| self.rows_of(self.sheet)[j].song().map(|i| self.catalog.songs[i].id)).collect();
        let jackets: Vec<Option<ID2D1Bitmap1>> = ids.into_iter().map(|id| id.and_then(|id| self.jacket(id))).collect();

        let g = self.gfx.as_ref().unwrap();
        let f = self.fonts.as_ref().unwrap();
        let fx = self.fx.as_ref().unwrap();
        let s = self.scale;
        let bar = self.bar_rect();
        let sg = self.sheet_geom();
        let (panel, panel_a) = (sg.rect, sg.alpha);
        let has_panel = panel_a > 0.0 && panel.bottom > panel.top;
        let bar_r = self.px(lay::BAR_R);
        let pr = self.px(lay::PANEL_R);
        let (gx, _gy, gr) = self.bead_circle(BEAD_GEAR);
        let beads = [BEAD_HISTORY, BEAD_GEAR].map(|k| self.bead_now(k));
        // 열 땐 따로 페이드 인, 닫을 땐 그 순간의 불투명도에서 흐려짐
        let fade = 1.0 - self.close.value();
        let open_t = Ease::OUT.apply((self.shown_at.elapsed().as_secs_f32() / OPEN_FADE).min(1.0)) * fade;
        // 닫을 땐 전부 한 덩어리로 바 가운데 쪽으로 줄어듦
        let k = 1.0 - (1.0 - CLOSE_SCALE) * (1.0 - fade);
        let (ox, oy) = (self.px(lay::MARGIN + lay::W / 2.0), self.px(lay::MARGIN + lay::BAR_H / 2.0));
        let shrink = windows_numerics::Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: ox * (1.0 - k), M32: oy * (1.0 - k) };

        unsafe {
            let dc = &g.dc;
            let target = dc.GetTarget()?;
            let _home = Retarget(dc, target.clone()); // 마스크와 빛은 다른 데 기록함, 끝나면 창으로 되돌림

            // 1) 모양 마스크: 바 + 두 버튼은 액체처럼 늘어나며 떨어지고, 패널은 선명함
            let mask_b = g.brush(white(1.0))?;
            let bar_h = bar.bottom - bar.top;
            let cl = dc.CreateCommandList()?;
            dc.SetTarget(&cl);
            let d = Drawing::begin(dc, &shrink);
            dc.FillRoundedRectangle(&rounded(bar, bar_h.min(bar_r * 2.0) / 2.0), &mask_b);
            for (k, &(bx, by, brr)) in beads.iter().enumerate() {
                if self.reveal[k].value > 0.02 {
                    dc.FillEllipse(&circle(bx, by, brr), &mask_b);
                }
            }
            d.end()?;
            cl.Close()?;
            gfx::set_input_image(&fx.goo_blur, 0, &cl.cast()?);
            let pl = dc.CreateCommandList()?;
            dc.SetTarget(&pl);
            let d = Drawing::begin(dc, &shrink);
            if has_panel {
                dc.FillRoundedRectangle(&rounded(panel, pr), &g.brush(white(panel_a))?);
            }
            d.end()?;
            pl.Close()?;
            gfx::set_input_image(&fx.sheets, 1, &pl.cast()?);

            // 2) 테두리 선의 빛: 시트마다 알림과 같은 그라데이션 하나 (`Gfx::rim_gradient`)
            let light = dc.CreateCommandList()?;
            dc.SetTarget(&light);
            let d = Drawing::begin(dc, &shrink);
            let pad = self.px(4.0);
            let split = (bar.bottom + panel.top) / 2.0;
            let lb = g.rim_gradient(bar.top, bar.bottom, 1.0)?;
            dc.FillRectangle(&rect(bar.left - pad, bar.top - pad, gx + gr + pad, split), &lb);
            if has_panel {
                let pb = g.rim_gradient(panel.top, panel.bottom, panel_a)?;
                dc.FillRectangle(&rect(panel.left - pad, split, panel.right + pad, panel.bottom + pad), &pb);
            }
            d.end()?;
            light.Close()?;
            gfx::set_input_image(&fx.rim_light, 0, &light.cast()?);

            // 3) 합성: 그림자, 판, 테두리 빛 순 (효과 출력은 이미 제자리라 변환 없이 그림)
            dc.SetTarget(&target);
            let d = Drawing::begin(dc, &IDENTITY);
            dc.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
            let lp_all = D2D1_LAYER_PARAMETERS1 {
                contentBounds: rect(-1e6, -1e6, 1e6, 1e6),
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: IDENTITY,
                opacity: open_t,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            dc.PushLayer(&lp_all, None);
            for img in [fx.shadow.output()?, fx.plate.GetOutput()?, fx.rim_light.GetOutput()?] {
                dc.DrawImage(&img, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            }
            dc.SetTransform(&shrink);
            // 닫을 땐 글과 줄이 판보다 먼저 흐려짐, 뒤의 게임이 또렷이 비치기 전에 사라짐
            if self.closing {
                let lp_content = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: rect(-1e6, -1e6, 1e6, 1e6),
                    geometricMask: std::mem::ManuallyDrop::new(None),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: IDENTITY,
                    opacity: ((fade - CLOSE_TEXT_GONE) / (1.0 - CLOSE_TEXT_GONE)).clamp(0.0, 1.0),
                    opacityBrush: std::mem::ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                dc.PushLayer(&lp_content, None);
            }

            // 4) 검색 바 내용
            let cy = (bar.top + bar.bottom) / 2.0;
            let text_b = g.brush(TEXT)?;
            let sub_b = g.brush(SUB)?;
            let round = g.factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES1 { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, ..Default::default() },
                None,
            )?;
            let ix = bar.left + self.px(24.0);
            let (lx, ly, lr) = (ix - self.px(1.5), cy - self.px(1.5), self.px(6.0));
            let handle = vec![gfx::pt(ix + self.px(3.0), cy + self.px(3.0)), gfx::pt(ix + self.px(7.5), cy + self.px(7.5))];
            dc.DrawGeometry(&g.strokes(&[(gfx::pt(lx, ly), lr)], &[handle])?, &sub_b, 1.5 * s, &round);

            // 곡 이동 때 고를 채보 "4B · HD", 보여주기만 함 (설정은 톱니 버튼이나 Tab으로 엶)
            // 레벨 조건 검색 중엔 실제로 적용될 모드·난이도
            let cr = self.cond_rect();
            let (cm, cd) = match Filter::parse(&self.searched).and_then(|f| f.level).filter(|_| self.view == View::Results) {
                Some(q) => (q.mode.unwrap_or(self.mode_choice), q.diff),
                None => (self.mode_choice, self.diff_choice),
            };
            let cond = format!("{} · {}", cm.label(), cd.label());
            let cl_ = g.layout_centered(&cond, &f.small, cr.right - cr.left, cr.bottom - cr.top)?;
            dc.DrawTextLayout(gfx::pt(cr.left, cr.top), &cl_, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);

            let (tx, _) = self.text_origin();
            let tw = (cr.left - self.px(8.0) - tx).max(10.0);
            let q = self.query();
            if q.is_empty() {
                let ph = g.brush(FAINT)?;
                let l = g.layout("곡 검색 · 초성 · 작곡가", &f.placeholder, tw, bar_h)?;
                l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                // 커서가 첫 글자를 파고들지 않게 살짝 오른쪽에
                dc.DrawTextLayout(gfx::pt(tx + self.px(2.0), bar.top), &l, &ph, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            }
            // 글은 가로로 자르지 않고 입력 칸 밖을 잘라 그림, 조합 중인 글자는 캐럿 자리에 밑줄
            let l = g.layout_fresh(&q, &f.input, 100_000.0, bar_h)?;
            l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let c = self.edit.caret();
            if !self.preedit.is_empty() {
                let range = DWRITE_TEXT_RANGE { startPosition: s_to_u16(&q[..c]) as u32, length: s_to_u16(&self.preedit) as u32 };
                l.SetUnderline(true, range)?;
            }
            // 캐럿 (조합 중이면 조합 글자 뒤)이 입력 칸 안에 오게 가로로 밈
            let caret_x = text_x(&l, &q, c + self.preedit.len());
            let room = tw - self.px(3.0);
            let mut scroll = self.text_scroll;
            if caret_x - scroll > room {
                scroll = caret_x - room;
            } else if caret_x < scroll {
                scroll = caret_x;
            }
            self.text_scroll = scroll.clamp(0.0, (text_x(&l, &q, q.len()) - room).max(0.0));
            let ox = tx - self.text_scroll;
            dc.PushAxisAlignedClip(&rect(tx - self.px(2.0), bar.top, tx + tw, bar.bottom), D2D1_ANTIALIAS_MODE_ALIASED);
            let selection = self.edit.selection().filter(|_| self.preedit.is_empty());
            if let Some((a, b)) = selection {
                let sb = g.brush(white(FILL_STRONG))?;
                let (x0, x1) = (ox + text_x(&l, &q, a), ox + text_x(&l, &q, b));
                dc.FillRoundedRectangle(&rrect(x0, cy - self.px(11.0), x1, cy + self.px(11.0), self.px(3.0)), &sb);
            }
            dc.DrawTextLayout(gfx::pt(ox, bar.top), &l, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            let blink_on = ((now - self.caret_t0).as_millis() / CARET_BLINK_MS as u128).is_multiple_of(2);
            if blink_on && self.preedit.is_empty() && selection.is_none() && !self.closing {
                let cx = ox + caret_x + self.px(1.0);
                let caret = g.brush(CARET)?;
                dc.FillRectangle(&rect(cx, cy - self.px(10.0), cx + (1.5 * s).round(), cy + self.px(10.0)), &caret);
            }
            dc.PopAxisAlignedClip();

            // 5) 히스토리·톱니 버튼, 아이콘은 버튼마다 따로 나타남 (`bead_shown`)
            // 그 보기가 열려 있으면 눌린 채, 아니면 호버할 때 밝아짐
            let ia = self.icon_alpha.value();
            let icons = [
                (BEAD_HISTORY, Hit::History, View::History, "\u{E81C}", &f.icon_history),
                (BEAD_GEAR, Hit::Gear, View::Settings, "\u{E713}", &f.icon),
            ];
            for (k, hit, view, glyph, icon) in icons {
                let (bx, by, brr) = beads[k];
                let ia = ia * self.bead_shown(k);
                if ia <= 0.01 {
                    continue;
                }
                let lit = self.hover == hit || self.view == view;
                if lit {
                    let hb = g.brush(white(if self.view == view { FILL_SELECTED } else { FILL_HOVER } * ia))?;
                    dc.FillEllipse(&circle(bx, by, brr), &hb);
                }
                let ink = if lit { TEXT } else { SUB };
                let ib = g.brush(with_alpha(ink, ia))?;
                if let Some(icf) = icon {
                    let il = g.layout_centered(glyph, icf, brr * 2.0, brr * 2.0)?;
                    dc.DrawTextLayout(gfx::pt(bx - brr, by - brr), &il, &ib, D2D1_DRAW_TEXT_OPTIONS_NONE);
                } else {
                    dc.DrawEllipse(&circle(bx, by, self.px(6.0)), &ib, 1.5 * s, None);
                }
            }

            // 6) 패널 내용 (움직이는 패널 모양으로 자름, 보기를 바꾸면 한 프레임에 바뀜)
            if has_panel {
                let geo = g.factory.CreateRoundedRectangleGeometry(&rounded(panel, pr))?;
                let mut lp = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: panel,
                    geometricMask: std::mem::ManuallyDrop::new(Some(geo.cast()?)),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: IDENTITY,
                    opacity: panel_a,
                    opacityBrush: std::mem::ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                dc.PushLayer(&lp, None);
                // 마스크에 대한 우리 참조 (구조체는 drop 안 함), PopLayer 뒤 이 블록이 끝날 때 해제
                // 중간에 `?`로 나가도 마찬가지
                let _mask = std::mem::ManuallyDrop::into_inner(std::mem::replace(&mut lp.geometricMask, std::mem::ManuallyDrop::new(None)));
                // 내용은 다 편 시트 자리에서 시트와 같이 비켜남
                let ct = self.panel_top();
                dc.SetTransform(&windows_numerics::Matrix3x2 { M32: shrink.M32 + k * sg.dy, ..shrink });
                let (l0, r0) = (panel.left + self.px(lay::INSET), panel.right - self.px(lay::INSET));
                let l0t = l0 - BEARING; // 글자 x, 잉크가 16 정렬선에 맞음

                if self.sheet == Sheet::Settings {
                    let sl = self.settings_layout();
                    let t = g.layout("검색 설정", &f.head, self.px(200.0), self.px(22.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.title), &t, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    // 화살표로 바꾸는 컨트롤은 (한 번 쓴 뒤로) 테두리 없이 라벨 밝기로만 표시
                    for (k, (label, y)) in [("버튼 모드", sl.labels[0]), ("난이도", sl.labels[1])].into_iter().enumerate() {
                        let ll = g.layout(label, &f.sub, self.px(200.0), self.px(18.0))?;
                        let lb = if self.kb_focus == Some(k) { &text_b } else { &sub_b };
                        dc.DrawTextLayout(gfx::pt(l0t, y), &ll, lb, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                    let thumb = |k: usize| {
                        let at = self.seg[k].value();
                        (at, at + 1.0)
                    };
                    draw_segments(g, f, sl.mode, &Mode::ALL.map(Mode::label), thumb(0), s)?;
                    draw_segments(g, f, sl.diff, &Diff::ALL.map(Diff::label), thumb(1), s)?;
                    let note_b = g.brush(FAINT)?;
                    let lt = g.layout("내 곡 목록", &f.item_bold, self.px(200.0), self.px(20.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.list_title), &lt, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    let label = if self.owned.is_some() { "다시 인식" } else { "목록 인식" };
                    draw_button(g, &f.item, sl.rescan, label, matches!(self.hover, Hit::Rescan) || self.kb_focus == Some(2), s)?;
                    let nl2 = g.layout("DLC 구매나 곡 해금 후 다시 인식해 주세요", &f.sub, r0 - l0, self.px(18.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.list_note), &nl2, &note_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    // 단축키: 새 조합을 기다리는 동안 버튼이 눌린 채, 못 받으면 아랫줄에 잠깐 이유
                    let ht = g.layout("검색창 단축키", &f.item_bold, self.px(200.0), self.px(20.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.hotkey_title), &ht, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    let waiting = self.hotkey_wait.is_some();
                    draw_button(g, &f.item, sl.hotkey, &self.hotkey_text(), waiting || self.hover == Hit::Hotkey || self.kb_focus == Some(3), s)?;
                    let (note, nb) = match self.hotkey_note.filter(|(_, at)| at.elapsed().as_secs_f32() < HOTKEY_NOTE_SECS) {
                        Some((why, _)) => (why, &text_b),
                        None if waiting => ("Esc를 누르면 취소돼요", &note_b),
                        None => ("선곡 화면에서 이 키로 검색창을 열어요", &note_b),
                    };
                    let nl3 = g.layout(note, &f.sub, sl.hotkey.left - self.px(12.0) - l0, self.px(18.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.hotkey_note), &nl3, nb, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    let kt = g.layout("검색어 유지", &f.item_bold, self.px(200.0), self.px(20.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.keep_title), &kt, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    let nl4 = g.layout("다시 열면 마지막 검색어가 남아 있어요", &f.sub, sl.keep.left - self.px(12.0) - l0, self.px(18.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.keep_note), &nl4, &note_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    let knob = (self.knob.value(), self.knob_tint.value(), self.knob_press.value());
                    draw_toggle(g, sl.keep, knob, self.hover == Hit::KeepQuery || self.kb_focus == Some(4), s)?;
                } else if let (Sheet::Card, Some((title, sub, button))) = (self.sheet, self.notice()) {
                    let top = ct;
                    let tw = r0 - l0 - self.px(lay::CARD_BTN_W + 12.0);
                    // 두 줄을 재서 카드 안에 한 덩어리로 가운데 정렬
                    let tl = g.layout(title, &f.title, tw, self.px(40.0))?;
                    let sl_ = g.layout(sub, &f.sub, tw, self.px(40.0))?;
                    let (mut tm, mut sm) = (DWRITE_TEXT_METRICS::default(), DWRITE_TEXT_METRICS::default());
                    tl.GetMetrics(&mut tm)?;
                    sl_.GetMetrics(&mut sm)?;
                    let gap = self.px(4.0);
                    let y0 = top + ((self.px(lay::CARD_H) - (tm.height + gap + sm.height)) / 2.0).round();
                    dc.DrawTextLayout(gfx::pt(l0t, y0), &tl, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    dc.DrawTextLayout(gfx::pt(l0t, y0 + tm.height + gap), &sl_, &sub_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    draw_button(g, &f.item_bold, self.card_button(), button, matches!(self.hover, Hit::CardButton), s)?;
                } else if listed && n_rows == 0 {
                    // 최근 곡이 있는데 이 목록에 하나도 없을 때도 따로 알림
                    let empty = match self.sheet {
                        Sheet::History if self.recent.is_empty() => "아직 고른 곡이 없어요",
                        Sheet::History => "이 목록에서 고른 곡이 아직 없어요",
                        _ => "일치하는 곡이 없어요",
                    };
                    let l = g.layout_centered(empty, &f.item, panel.right - panel.left, self.px(lay::ROW_H))?;
                    dc.DrawTextLayout(gfx::pt(panel.left, ct + self.px(lay::PAD)), &l, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                } else if listed {
                    let rows = self.rows_of(self.sheet);
                    // 선택 강조: 한 줄 높이, 모서리가 패널과 동심 (반지름 = PANEL_R - PAD)
                    let hl = g.brush(white(FILL_SELECTED))?;
                    let inset = self.px(lay::PAD);
                    let t = ct + self.sel_y.value;
                    dc.FillRoundedRectangle(&rrect(panel.left + inset, t, panel.right - inset, t + pitch, self.px(lay::PANEL_R - lay::PAD)), &hl);

                    let floor_b = g.brush(FAINT)?;
                    let jsz = self.px(32.0);
                    let jr = self.px(lay::PANEL_R - lay::PAD - 8.0);
                    // 줄은 줄 영역 안에서 미끄러짐 (멈추면 딱 보이는 줄만이라 잘림 없음)
                    let top = ct + self.row_y(0);
                    dc.PushAxisAlignedClip(&rect(panel.left, top, panel.right, top + pitch * n_rows.min(lay::ROWS) as f32), D2D1_ANTIALIAS_MODE_ALIASED);
                    for (k, &j) in shown.iter().enumerate() {
                        let row = rows[j];
                        let y = ct + (self.row_y(0) + pitch * j as f32 - off);
                        let jx = panel.left + inset + self.px(8.0);
                        let jy = y + (self.px(lay::ROW_H) - jsz) / 2.0;
                        if row == Row::Random {
                            // 재킷 자리에 셔플 아이콘
                            dc.FillRoundedRectangle(&rrect(jx, jy, jx + jsz, jy + jsz, jr), &g.brush(white(FILL_HOVER))?);
                            if let Some(icf) = &f.icon {
                                let il = g.layout_centered("\u{E8B1}", icf, jsz, jsz)?;
                                dc.DrawTextLayout(gfx::pt(jx, jy), &il, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                            }
                        } else if let Some(bmp) = &jackets[k] {
                            let bb = dc.CreateBitmapBrush(bmp, None, None)?;
                            let sz = bmp.GetPixelSize();
                            let sc = jsz / sz.width as f32;
                            bb.SetTransform(&windows_numerics::Matrix3x2 { M11: sc, M12: 0.0, M21: 0.0, M22: sc, M31: jx, M32: jy });
                            bb.SetInterpolationMode1(D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC);
                            dc.FillRoundedRectangle(&rrect(jx, jy, jx + jsz, jy + jsz, jr), &bb);
                        }
                        // 제목, 아랫줄, 레벨 칸의 (난이도, 레벨, 서열표)
                        let (title, sub, level) = match row {
                            Row::Random => ("무작위로 고르기", self.rolls.note(rows), None),
                            Row::Song(i) | Row::Chart(i, ..) => {
                                let song = &self.catalog.songs[i];
                                let (m, d) = match row {
                                    Row::Chart(_, m, d) => (m, d),
                                    _ => (self.mode_choice, self.diff_choice),
                                };
                                // BPM 조건 검색 결과는 DLC 대신 BPM
                                let sub = if self.bpm_rows && self.sheet == Sheet::Results {
                                    format!("{}  ·  BPM {}", song.composer, song.bpm)
                                } else {
                                    format!("{}  ·  {}", song.composer, song.dlc)
                                };
                                (song.name.as_str(), sub, song.level(m, d).map(|l| (d, l, song.floor(m, d))))
                            }
                        };
                        let tx = jx + jsz + self.px(10.0);
                        let lvl_r = panel.right - inset - self.px(8.0);
                        let tw = lvl_r - self.px(lay::LEVEL_W) - self.px(8.0) - tx;
                        let (title_y, sub_y) = (y + self.px(6.67), y + self.px(26.67));
                        let tl = g.layout(title, &f.title, tw, self.px(19.0))?;
                        dc.DrawTextLayout(gfx::pt(tx, title_y), &tl, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                        let sl_ = g.layout(&sub, &f.sub, tw, self.px(16.0))?;
                        dc.DrawTextLayout(gfx::pt(tx, sub_y), &sl_, &sub_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);

                        // 레벨 칸: 레벨 "HD 11"은 곡 이름과, 서열표 "13.4"는 아랫줄과 같은 글자 기준선
                        // 그 모드·난이도 패턴이 없는 곡은 비우고, 서열표가 없는 패턴은 아래만 비움
                        let Some((diff, lvl, floor)) = level else { continue };
                        let lx = lvl_r - self.px(lay::LEVEL_W);
                        let ll = g.layout(&format!("{} {lvl}", diff.label()), &f.item_bold, self.px(lay::LEVEL_W), self.px(20.0))?;
                        ll.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                        dc.DrawTextLayout(gfx::pt(lx, title_y + baseline(&tl) - baseline(&ll)), &ll, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        if let Some(fl) = floor {
                            let fl = g.layout(&format!("{fl:.1}"), &f.sub, self.px(lay::LEVEL_W), self.px(16.0))?;
                            fl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                            dc.DrawTextLayout(gfx::pt(lx, sub_y + baseline(&sl_) - baseline(&fl)), &fl, &floor_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        }
                    }
                    dc.PopAxisAlignedClip();
                }
                dc.SetTransform(&shrink);
                dc.PopLayer();
            }

            if self.closing {
                dc.PopLayer();
            }
            dc.PopLayer();
            d.end()?;
        }
        self.moving = self.animating();
        self.gfx.as_ref().unwrap().present()
    }
}

/// 레이아웃 첫 줄의 글자 기준선, 위 끝에서 잰 거리 (줄이 더 있어도 첫 줄은 채워짐)
fn baseline(l: &IDWriteTextLayout) -> f32 {
    let mut m = [DWRITE_LINE_METRICS::default()];
    let mut n = 0;
    let _ = unsafe { l.GetLineMetrics(Some(&mut m), &mut n) };
    m[0].baseline
}

/// 패널 버튼 (설정, 안내 카드): 둥근 채움 (호버·포커스·눌림이면 진하게)과 가운데 글
fn draw_button(g: &Gfx, font: &IDWriteTextFormat, r: D2D_RECT_F, label: &str, lit: bool, s: f32) -> Result<()> {
    unsafe {
        let fill = g.brush(white(if lit { FILL_STRONG } else { FILL_SELECTED }))?;
        g.dc.FillRoundedRectangle(&rounded(r, (8.0 * s).round()), &fill);
        let l = g.layout_centered(label, font, r.right - r.left, r.bottom - r.top)?;
        g.dc.DrawTextLayout(gfx::pt(r.left, r.top), &l, &g.brush(TEXT)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}

/// 토글: 꺼지면 옅은 트랙에 흐린 노브, 켜지면 밝은 트랙에 판 색 노브 (호버·포커스면 꺼진 트랙만 조금 밝게)
/// `knob`은 (자리 0 끔 .. 1 켬, 넘칠 수 있음, 켬 색 비율, 노브 세로 배율)
/// 눌린 노브는 높이 절반을 반지름으로 둬서 타원이 아니라 캡슐
fn draw_toggle(g: &Gfx, r: D2D_RECT_F, (at, tint, press): (f32, f32, f32), lit: bool, s: f32) -> Result<()> {
    unsafe {
        let h = r.bottom - r.top;
        let d = (lay::KNOB * s).round();
        let pad = (h - d) / 2.0;
        let travel = r.right - r.left - d - pad * 2.0;
        let mix = |a: f32, b: f32| a + (b - a) * tint;
        let off = if lit { FILL_STRONG } else { FILL_SELECTED };
        g.dc.FillRoundedRectangle(&rounded(r, h / 2.0), &g.brush(white(mix(off, TEXT.a)))?);
        let knob = color(mix(1.0, TINT.r), mix(1.0, TINT.g), mix(1.0, TINT.b), mix(SUB.a, 1.0));
        let (cx, cy) = (r.left + pad + d / 2.0 + travel * at, (r.top + r.bottom) / 2.0);
        let (rx, ry) = (d / 2.0, d / 2.0 * press);
        g.dc.FillRoundedRectangle(&rrect(cx - rx, cy - ry, cx + rx, cy + ry, ry), &g.brush(knob)?);
    }
    Ok(())
}

/// 세그먼트 컨트롤: 트랙 + 손잡이 (호버 덧칠 없음)
fn draw_segments(
    g: &Gfx,
    f: &Fonts,
    r: D2D_RECT_F,
    labels: &[&str],
    thumb: (f32, f32), // 움직이는 손잡이 양 끝, 칸 단위
    s: f32,
) -> Result<()> {
    unsafe {
        let dc = &g.dc;
        let rad = 9.0 * s;
        let track = g.brush(white(FILL_TRACK))?;
        dc.FillRoundedRectangle(&rounded(r, rad), &track);
        let n = labels.len();
        let w = (r.right - r.left) / n as f32;
        let inset = (3.0 * s).round();
        let (tl, tr) = (thumb.0.clamp(-0.2, n as f32 + 0.2), thumb.1.clamp(-0.2, n as f32 + 0.2));
        let tb = g.brush(white(FILL_STRONG))?;
        let (x0, x1) = ((r.left + w * tl + inset).max(r.left + inset), (r.left + w * tr - inset).min(r.right - inset));
        if x1 > x0 {
            dc.FillRoundedRectangle(&rrect(x0, r.top + inset, x1, r.bottom - inset, rad - inset), &tb);
        }
        for (i, label) in labels.iter().enumerate() {
            // 라벨 색은 손잡이를 따라감, 지금 손잡이가 이 칸을 얼마나 덮는지로 정함
            let cover = (tr.min(i as f32 + 1.0) - tl.max(i as f32)).clamp(0.0, 1.0);
            let mix = |a: f32, b: f32| a + (b - a) * cover;
            let col = white(mix(SUB.a, TEXT.a));
            let b = g.brush(col)?;
            let l = g.layout_centered(label, &f.item_bold, w, r.bottom - r.top)?;
            dc.DrawTextLayout(gfx::pt(r.left + w * i as f32, r.top), &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }
    Ok(())
}

unsafe fn ime_string(himc: HIMC, kind: IME_COMPOSITION_STRING) -> String {
    let bytes = ImmGetCompositionStringW(himc, kind, None, 0);
    if bytes <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (bytes as usize) / 2];
    ImmGetCompositionStringW(himc, kind, Some(buf.as_mut_ptr() as _), bytes as u32);
    String::from_utf16_lossy(&buf)
}
