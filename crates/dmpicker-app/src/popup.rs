//! 검색창: 너비 540 px 고정, 알림과 같은 반투명 판, 호버하면 나오는 히스토리·톱니 버튼,
//! 결과 자리에 뜨는 설정·히스토리 시트, 한글 IME, 키보드·마우스

use std::borrow::Cow;
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

use crate::anim::{open_scale, Ease, Spring, Tween, OPEN, OPEN_FADE, OPEN_KICK};
use crate::beam::{Beam, Kind, SPIN_SECS};
use crate::catalog::{Catalog, Song};
use crate::edit::{clean_paste, Editor};
use crate::gfx::{
    self, alpha_step, circle, color, rect, rounded, rrect, white, with_alpha, Drawing, Gfx, Retarget, BEARING, CARET, DISABLED,
    FAINT, FAVORITE, FILL_HOVER, FILL_SELECTED, FILL_STRONG, FILL_TRACK, PLATE_ALPHA, SUB, TEXT, TINT,
};
use crate::hotkey::Hotkey;
use crate::icons::Icon;
use crate::playlist::{self, Entry, Failure, Library, Pattern, Playlist, Recent, Reply};
use crate::sys;
use crate::update::{DataNotice, OUTDATED_TEXT};

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
/// 열기: 띄울 때 0에서 1로 (살짝 튐), 알림과 같음
const OPEN_SPRING: Spring = Spring::new(0.0, OPEN.0, OPEN.1);
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
/// 보기·탭·카드를 바꿀 때 옛 내용이 흐려지는 시간과 새 내용이 같은 자리에서 나타나는 시간 (`Crossfade`)
/// 둘이 같이 시작해서 빈 판이 안 보임
const SWAP_OUT_SECS: f32 = 0.18;
const SWAP_IN_SECS: f32 = 0.25;
/// 즐겨찾기 별 (`StarMotion`): 색과 채움은 켤 때도 끌 때도 이 시간에 바뀜, 눌림과 점 튀기는 켤 때만
const STAR_FILL_SECS: f32 = 0.15;
/// 눌림: 앞 30 %에 이 배율까지 줄었다 나머지에 돌아옴, 구간마다 넘쳤다 돌아옴
const STAR_POP: f32 = 0.82;
const STAR_POP_SECS: f32 = 0.35;
const STAR_POP_EASE: Ease = Ease(0.34, 1.96, 0.64, 1.0);
/// 튀는 점이 나타나고 멀어지는 곡선 (`Spark::at`)
const SPARK_EASE: Ease = Ease(0.0, 0.0, 0.58, 1.0);

/// 바 오른쪽 버튼 순서 (`bead_circle`)
const BEAD_HISTORY: usize = 0;
const BEAD_GEAR: usize = 1;
/// 두 버튼 시차: 나올 땐 히스토리 다음 톱니, 들어갈 땐 톱니 다음 히스토리
const BEAD_STAGGER: Duration = Duration::from_millis(70);

/// 논리 (96 dpi) 레이아웃, 검색창 전체 너비는 늘 `W`
mod lay {
    pub const W: f32 = 540.0;
    pub use crate::gfx::MARGIN; // 그림자 자리
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
    pub const PLAYLIST_LEVEL_W: f32 = 72.0; // 플레이리스트 결과는 모드까지, 가장 긴 "8B · MX 15"가 약 66
    pub const SETTINGS_H: f32 = 402.0; // 맨 아래 줄(버튼 높이 30) 아래 16 (= 오른쪽 여백)
    pub const TOGGLE_W: f32 = 42.0;
    pub const TOGGLE_H: f32 = 24.0;
    pub const KNOB: f32 = 18.0; // 둘레 3
    pub const CARD_H: f32 = 84.0;
    pub const CARD_BTN_W: f32 = 112.0;
    pub const COND_W: f32 = 52.0; // 글은 가운데, 오른쪽 끝이 16 정렬선
    pub const TEXT_FADE: f32 = 16.0; // 입력 칸 글이 가려진 쪽 가장자리를 흐리는 최대 폭
    /// 아이콘 상자, 256 단위 경로를 이 크기로 줄여 그림 (선 굵기 약 1.25)
    pub const ICON: f32 = 20.0;
    /// 키워드 칩 ×만 작게, 굵은 ×라 선 굵기 약 1
    pub const CHIP_X: f32 = 11.0;
    pub const PANEL_MAX: f32 = PAD * 2.0 + ROW_H * ROWS as f32; // 304
    /// 히스토리 맨 위 탭 띠: 위 8 + 세그먼트 32, 그 아래 8은 줄 목록의 PAD (6줄이면 344)
    pub const TABS_H: f32 = PAD + 32.0;
    /// 키워드 칩 줄 한도: 칩 위 88, 아래 안내 문구·여백 44를 빼고 창 높이에 드는 줄 (칩 28 + 간격 6)
    pub const CHIP_LINES: usize = ((PANEL_MAX.max(SETTINGS_H) - 88.0 - 44.0 + 6.0) / 34.0) as usize; // 8
    /// 플레이리스트 결과 아래 칸의 글 줄 높이와 줄 수 한도, 넘는 코멘트는 말줄임
    pub const NOTE_LINE: f32 = 19.0;
    pub const NOTE_LINES: usize = 5;
    /// 아래 칸 위아래 여백, 줄 높이 안 여백까지 더하면 글자 위아래가 16으로 같음 (= 양옆)
    pub const FOOT_PAD: f32 = 12.0;
}

struct Fonts {
    input: IDWriteTextFormat,
    placeholder: IDWriteTextFormat,
    title: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    head: IDWriteTextFormat,
    item: IDWriteTextFormat,
    item_bold: IDWriteTextFormat,
    item_regular: IDWriteTextFormat,
    small: IDWriteTextFormat,
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
    /// 플레이리스트 id와 고른 패턴 (무작위 줄이면 뽑힌 패턴)
    PlaylistPick(String, Pattern),
    Hide,
    Scan,
    /// 곡 데이터를 바로 다시 받음
    Retry,
    /// 새 버전 릴리스 페이지를 엶
    Release,
    /// 모드·난이도·토글 바뀜 (저장해야 함)
    Choice,
    /// 새 단축키를 받음, 등록해 보고 [`Popup::hotkey_result`]로 알려야 함
    Hotkey(Hotkey),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Results,
    Settings,
    /// 최근 고른 곡·플레이리스트와 즐겨찾기, 히스토리 버튼으로 엶
    History,
    /// 곡 하나의 내 키워드, 결과·히스토리 줄을 우클릭하거나 F2로 엶
    Keywords,
}

/// 히스토리 맨 위 탭, 세그먼트 칸 순서대로
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HistoryTab {
    Recent,
    Favorites,
}

/// 시트에 그리는 내용, 보이거나 나타나는 중에만 바뀌고 사라지는 동안엔 그대로 (`retarget_panel`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sheet {
    Settings,
    Card,
    /// 검색 결과 줄
    Results,
    /// 최근 고른 곡·플레이리스트 또는 즐겨찾기 줄
    History,
    /// 곡 줄과 키워드 칩
    Keywords,
}

/// 결과 자리에 뜨는 안내 카드, 모두 같은 높이 (`CARD_H`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Card {
    /// 받아 둔 곡 데이터가 없어 받는 중, 버튼 없음
    Loading,
    /// 곡 데이터를 받지 못함
    Failed,
    /// 받은 곡 데이터 형식을 이 버전이 못 읽음
    Outdated,
    /// 첫 인식 전
    Scan,
    /// 곡 이동 중 화면이 저장된 목록과 달랐음
    Stale,
    /// 새 버전이 나옴, 버전마다 한 번만
    Update,
}

/// 키워드 시트 상태와 닫으면 돌아갈 보기·입력·줄·스크롤
struct KeywordEdit {
    /// `songs` 인덱스, 색인을 다시 만들어도 그대로
    song: usize,
    from: View,
    /// 열기 전 입력, 시트에선 입력 칸에 키워드를 씀
    edit: Editor,
    row: Row,
    scroll: usize,
    /// 한 번 더 Backspace를 누르면 지울 칩
    remove: Option<usize>,
    /// 칩 아래 안내 문구 (입력 오류, 저장 실패)
    note: Option<&'static str>,
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
    /// 플레이리스트에서 고를 수 없는 항목 (`entries` 인덱스), 흐리게 보이기만 함
    Unavailable(usize),
    /// 히스토리의 플레이리스트 (`history_entries` 인덱스), 고르면 그 플레이리스트 결과를 엶
    Playlist(usize),
}

impl Row {
    fn song(self) -> Option<usize> {
        match self {
            Row::Song(i) | Row::Chart(i, ..) => Some(i),
            Row::Random | Row::Unavailable(_) | Row::Playlist(_) => None,
        }
    }

    /// 선택·호버·클릭이 되는 줄
    fn selectable(self) -> bool {
        !matches!(self, Row::Unavailable(_))
    }
}

/// 결과로 보고 있는 플레이리스트
struct PlaylistView {
    id: String,
    /// 받은 응답이나 사본, 둘 다 없으면 None
    data: Option<Playlist>,
    /// 줄 대신 보여 줄 실패 안내
    note: Option<&'static str>,
    /// 사본 없이 응답을 기다리는 동안 패널을 숨김
    waiting: bool,
    /// 사본 없이 응답을 기다리는 동안 고를 줄과 스크롤, 응답이 오면 그 줄을 고름
    restore: Option<(Row, usize)>,
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

/// 별 버튼에서 튀는 점 하나, 켤 때마다 새로 뽑음
#[derive(Clone, Copy, Debug)]
struct Spark {
    /// 나가는 방향 (rad, 아래가 양수)
    angle: f32,
    /// 끝까지 가는 거리 (DIP)
    dist: f32,
    /// 출발 전 기다리는 시간과 나타났다 사라지는 시간 (초)
    delay: f32,
    secs: f32,
    /// 지름 (DIP)
    size: f32,
    /// 끝 배율
    end: f32,
}

impl Spark {
    /// 시작에서 `t`초 뒤의 (거리 비율, 배율, 불투명도), 안 보이면 None
    /// 앞 20 %에 나타나며 거리의 25 %까지, 나머지에 끝까지 가며 줄고 흐려짐
    fn at(&self, t: f32) -> Option<(f32, f32, f32)> {
        let u = (t - self.delay) / self.secs;
        if u <= 0.0 || u >= 1.0 {
            return None;
        }
        Some(if u < 0.2 {
            let k = SPARK_EASE.apply(u / 0.2);
            (0.25 * k, 0.4 + 0.6 * k, k)
        } else {
            let k = SPARK_EASE.apply((u - 0.2) / 0.8);
            (0.25 + 0.75 * k, 1.0 + (self.end - 1.0) * k, 1.0 - k)
        })
    }
}

/// 키워드 시트의 별 버튼 모션, 켜고 끌 때마다 지금 채움에서 새로 시작 (`toggle_sheet_favorite`)
struct StarMotion {
    at: Instant,
    /// 시작할 때 채움 (0 꺼짐 .. 1 켜짐), 목표는 `on`
    from: f32,
    on: bool,
    /// 켤 때만
    sparks: Vec<Spark>,
}

impl StarMotion {
    fn new(from: f32, on: bool, rng: &mut Rng) -> Self {
        let mut u = || (rng.next() >> 40) as f32 / (1u64 << 24) as f32;
        // 켤 때마다 점 8개를 둘레에 흩어 뽑음
        let sparks = if on {
            (0..8)
                .map(|i| Spark {
                    angle: (i as f32 * 45.0 + (u() * 2.0 - 1.0) * 16.0).to_radians(),
                    dist: 22.0 * (0.68 + u() * 0.5),
                    delay: u() * 0.07,
                    secs: 0.6 * (0.78 + u() * 0.44),
                    size: 2.8 * (0.6 + u() * 0.8),
                    end: 0.35 + u() * 0.4,
                })
                .collect()
        } else {
            Vec::new()
        };
        Self { at: Instant::now(), from, on, sparks }
    }

    /// 시작에서 흐른 시간 (초), 아래 값들은 이 시간 `t`로 셈
    fn elapsed(&self) -> f32 {
        self.at.elapsed().as_secs_f32()
    }

    /// 채움 0 .. 1, 끌 때도 같은 시간에 빠짐
    fn fill(&self, t: f32) -> f32 {
        let to = self.on as u8 as f32;
        self.from + (to - self.from) * Ease::OUT.apply((t / STAR_FILL_SECS).min(1.0))
    }

    /// 아이콘 배율, 켤 때만 눌렸다 돌아옴
    fn pop(&self, t: f32) -> f32 {
        let t = t / STAR_POP_SECS;
        if !self.on || t >= 1.0 {
            1.0
        } else if t < 0.3 {
            1.0 + (STAR_POP - 1.0) * STAR_POP_EASE.apply(t / 0.3)
        } else {
            STAR_POP + (1.0 - STAR_POP) * STAR_POP_EASE.apply((t - 0.3) / 0.7)
        }
    }

    /// 채움·눌림·점이 다 끝남
    fn done(&self, t: f32) -> bool {
        let pop = if self.on { STAR_POP_SECS } else { 0.0 };
        let sparks = self.sparks.iter().map(|s| s.delay + s.secs).fold(0.0, f32::max);
        t >= STAR_FILL_SECS.max(pop).max(sparks)
    }
}

/// 겹쳐 바꾸기: 지난 프레임에 그린 내용이 흐려지는 동안 새 내용이 같은 자리에서 나타남
struct Crossfade {
    /// 지금 내용 보임 0 .. 1
    shown: Tween,
    /// 지난 프레임에 그린 내용 (다 편 시트 자리, 변환 없음)
    last: Option<ID2D1CommandList>,
    /// 흐려지는 옛 내용과 불투명도, 연달아 바꿔도 하나씩 마저 사라짐
    old: Vec<(ID2D1CommandList, Tween)>,
}

impl Crossfade {
    fn new() -> Self {
        Self { shown: Tween::new(1.0), last: None, old: Vec::new() }
    }

    /// 지난 프레임 내용은 보이던 만큼에서 흐려 보내고 새 내용은 0부터 나타냄
    fn swap(&mut self) {
        if let Some(list) = self.last.take() {
            let mut fade = Tween::new(self.shown.value());
            fade.go(0.0, SWAP_OUT_SECS, Ease::DECEL);
            self.old.push((list, fade));
        }
        self.shown.snap(0.0);
        self.shown.go(1.0, SWAP_IN_SECS, Ease::DECEL);
    }

    /// 겹치지 않고 지금 내용만 바로 다 보임
    fn reset(&mut self) {
        self.shown.snap(1.0);
        self.last = None;
        self.old.clear();
    }

    fn animating(&self) -> bool {
        !self.shown.done() || !self.old.is_empty()
    }

    fn retire(&mut self) {
        self.old.retain(|(_, fade)| !fade.done());
    }

    /// 옛 내용들 위에 지금 내용 `now`를 그림, 다 보이는 내용은 레이어 없이
    unsafe fn draw(&self, dc: &ID2D1DeviceContext, now: &ID2D1CommandList) {
        for (list, a) in self.old.iter().map(|(list, fade)| (list, fade.value())).chain([(now, self.shown.value())]) {
            let a = alpha_step(a);
            if a <= 0.0 {
                continue;
            }
            if a < 1.0 {
                dc.PushLayer(&gfx::layer(a, None), None);
            }
            dc.DrawImage(list, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            if a < 1.0 {
                dc.PopLayer();
            }
        }
    }
}

/// `draw`가 그리는 것을 단위 변환으로 기록함, 그릴 땐 지금 변환으로 그 자리에 다시 그림
unsafe fn record(dc: &ID2D1DeviceContext, draw: impl FnOnce() -> Result<()>) -> Result<ID2D1CommandList> {
    let list = dc.CreateCommandList()?;
    dc.SetTarget(&list);
    let d = Drawing::begin(dc, &IDENTITY);
    draw()?;
    d.end()?;
    list.Close()?;
    Ok(list)
}

/// 무작위 줄이 뽑는 줄: 고를 수 있는 곡이나 패턴
fn picks(rows: &[Row]) -> impl Iterator<Item = Row> + '_ {
    rows.iter().copied().filter(|r| matches!(r, Row::Song(_) | Row::Chart(..)))
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
    Toggle(Toggle),
    CardButton,
    Row(usize),
    /// 히스토리 맨 위 탭의 한 칸
    Tab(HistoryTab),
    /// 키워드 시트의 별 (즐겨찾기) 버튼
    Favorite,
    /// 키워드 시트의 완료 버튼
    Done,
    /// i번째 키워드 칩의 × 버튼
    RemoveKeyword(usize),
}

/// 설정 시트의 토글, 값이 `toggles`의 자리
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Toggle {
    KeepQuery,
    ShowDlc,
}

/// 토글 하나의 노브·색·눌림, 서로 다른 속도로 움직임
struct ToggleMotion {
    knob: Tween,  // 노브 자리 0 끔 .. 1 켬 (살짝 넘침)
    tint: Tween,  // 색 0 끔 .. 1 켬
    press: Tween, // 노브 세로 배율, 누르는 동안 줄어듦
}

impl ToggleMotion {
    fn new() -> Self {
        Self { knob: Tween::new(0.0), tint: Tween::new(0.0), press: Tween::new(1.0) }
    }

    fn snap(&mut self, on: bool) {
        let v = on as u8 as f32;
        self.knob.snap(v);
        self.tint.snap(v);
        self.press.snap(1.0);
    }

    fn go(&mut self, on: bool) {
        let v = on as u8 as f32;
        self.knob.go(v, KNOB_SECS, KNOB_EASE);
        self.tint.go(v, QUICK_SECS, QUICK_EASE);
    }

    fn done(&self) -> bool {
        [self.knob, self.tint, self.press].iter().all(|t| t.done())
    }
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
    dlc_title: f32,
    dlc: D2D_RECT_F, // 토글
    dlc_note: f32,
}

/// 물리 px 기준 키워드 시트: 클릭 영역과 각 글줄의 위쪽
struct KeywordLayout {
    /// 완료 버튼 왼쪽의 별 버튼
    fav: D2D_RECT_F,
    done: D2D_RECT_F,
    label: f32,
    chips_top: f32,
    chips: Vec<(D2D_RECT_F, D2D_RECT_F)>, // 칩과 그 안의 × 버튼, 창에 안 들어가는 칩은 빠짐
    note: f32,
    height: f32, // 시트 높이
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
    /// 받아 둔 곡 데이터가 없을 때의 상태 (앱이 넘겨줌)
    data_notice: Option<DataNotice>,
    /// 이번 열기의 새 버전 안내 설명, 열 때마다 앱이 정함
    version_notice: Option<String>,
    /// 새 버전 안내 카드를 그림, 앱이 꺼내서 기록 (`take_version_seen`)
    version_seen: bool,
    /// 설정 시트 머리 줄 문구, true면 새 버전 안내라 조금 밝게
    status_line: Option<(String, bool)>,
    /// 카드 시트에 그리는 카드, 시트처럼 보이거나 나타나는 중에만 바뀜
    card_now: Option<Card>,
    /// 검색창 단축키 (앱이 등록하고 저장함)
    pub hotkey: Hotkey,
    /// 새 단축키를 기다리는 중이면 Some(대기 동안 떼어 둔 IME 입력 컨텍스트)
    hotkey_wait: Option<HIMC>,
    /// 새 조합을 못 받은 이유와 그때
    hotkey_note: Option<(&'static str, Instant)>,
    pub mode_choice: Mode,
    pub diff_choice: Diff,
    /// 최근 고른 곡·플레이리스트와 사본, 즐겨찾기
    library: Library,
    /// 플레이리스트 주소를 검색했을 때만 Some
    playlist: Option<PlaylistView>,
    /// 받아 올 플레이리스트 id, 앱이 꺼내서 작업 스레드로 넘김 (`take_playlist_request`)
    playlist_request: Option<String>,
    /// 다시 열 때 지난 검색어와 결과 선택을 남김 (앱이 불러오고 저장함)
    pub keep_query: bool,
    /// 결과 아랫줄에 DLC도 표시 (앱이 불러오고 저장함)
    pub show_dlc: bool,
    /// 결과 보기에서 닫을 때 고른 줄과 스크롤, 다음 열기에서만 씀
    kept_row: Option<(Row, usize)>,
    /// 입력한 글, 캐럿과 선택 (조합 중인 글자는 `preedit`에 따로, 캐럿 자리에 그림)
    edit: Editor,
    preedit: String,
    /// WM_CHAR로 먼저 온 UTF-16 앞 조각 (뒤 조각과 합쳐 한 글자)
    high_surrogate: Option<u16>,
    /// 검색 결과
    results: Vec<Row>,
    /// 결과가 BPM 조건 검색이라 아랫줄에 DLC 표시와 상관없이 BPM
    bpm_rows: bool,
    /// 히스토리의 마지막 탭, 다시 열어도 그대로
    history_tab: HistoryTab,
    /// 히스토리 보기의 줄, 열 때마다 고른 탭의 목록으로 채움
    history: Vec<Row>,
    /// 표시 중인 항목 사본, 닫히는 동안 최근 기록이 재정렬돼도 그대로
    history_entries: Vec<Recent>,
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
    kb_focus: Option<usize>, // 설정 키보드 포커스: 0 모드, 1 난이도, 2 다시 인식, 3 단축키, 4 검색어 유지, 5 DLC 표시
    toggle_down: Option<Toggle>, // 누른 토글 (떼면 바뀜)
    keyword_edit: Option<KeywordEdit>,
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
    seg: [Tween; 3],        // 세그먼트 손잡이 [모드, 난이도, 히스토리 탭]: 왼쪽 끝, 칸 단위
    sheet_swap: Crossfade,  // 시트 내용, 보기나 카드가 바뀌면 겹쳐 바꿈
    list_swap: Crossfade,   // 히스토리 줄 목록, 탭이 바뀌면 겹쳐 바꿈 (탭은 제자리)
    star: Option<StarMotion>, // 키워드 시트 별 버튼 모션, 시트를 닫으면 버림
    toggles: [ToggleMotion; 2], // 검색어 유지, DLC 표시
    hover_since: Option<Instant>,
    leave_since: Option<Instant>,
    in_zone: bool, // 커서가 두 버튼을 드러내는 영역에 있음
    tracking: bool,
    shown_at: Instant,
    last: Instant,
    moving: bool, // 직전 프레임이 애니메이션 중이었음
    caret_t0: Instant,
    text_scroll: f32, // 입력 칸을 가로로 민 거리 px, 캐럿이 보이게 (`render`)
    text_return: Option<(f32, f32)>, // 버튼이 들어가기 시작할 때의 (민 거리, 바 진행), 그동안 글이 바와 같이 돌아옴
    dragging: bool,   // 입력 칸을 누른 채 끄는 중 (선택)
    last_mouse: Option<(i32, i32)>, // 마지막 마우스 이동의 클라이언트 좌표
    // 리소스
    jackets: HashMap<u32, Option<ID2D1Bitmap1>>,
    fonts: Option<Fonts>,
    fx: Option<Effects>,
    beams: Option<Beams>,
}

/// 합성기에 올렸으면 빛 셋: 열 때 바를 한 바퀴 도는 빛, 목록 인식 카드 아래를 오가는 흰 빛,
/// 무작위로 고르기 줄 아래를 오가는 여러 색 빛
struct Beams {
    open: Beam,
    card: Beam,
    random: Beam,
}

impl Beams {
    fn all(&self) -> [&Beam; 3] {
        [&self.open, &self.card, &self.random]
    }
}

/// 그릴 시트 (`sheet_geom`), 판정과 그리기가 같이 씀
struct SheetGeom {
    rect: D2D_RECT_F,
    /// 시트와 내용의 불투명도
    alpha: f32,
    /// 다 편 자리에서 비켜난 거리 (px, 위가 음수), 내용도 같이 움직임
    dy: f32,
}

/// 플레이리스트 결과 줄 목록 아래 칸 (`foot_of`), 높이·줄 수 계산과 그리기가 같이 씀
struct Foot {
    /// 고른 항목의 효과·목표 점수
    meta: Option<String>,
    /// 고른 항목의 코멘트, 없으면 플레이리스트 이름과 곡 수
    note: String,
    /// `note`가 코멘트면 글자색, 이름·곡 수면 보조색
    comment: bool,
    /// `note`의 줄 수, 최대 `NOTE_LINES`
    lines: usize,
}

impl Foot {
    /// 줄 목록 끝부터 패널 아래까지 (DIP): 위 12, 효과 줄 16 + 간격 4, 글 줄, 맨 아래 12
    fn height(&self) -> f32 {
        let meta = if self.meta.is_some() { 20.0 } else { 0.0 };
        lay::FOOT_PAD + meta + lay::NOTE_LINE * self.lines as f32 + lay::FOOT_PAD
    }
}

/// 아래 칸 `foot`을 빼고 창 높이 (설정 시트 402)에 드는 줄 수, 아래 칸이 없으면 `ROWS`
/// 코멘트가 5줄에서 잘려서 효과 줄이 있어도 5줄은 남음
fn page_above(foot: Option<&Foot>) -> usize {
    let Some(foot) = foot else { return lay::ROWS };
    let room = lay::PANEL_MAX.max(lay::SETTINGS_H) - lay::PAD - foot.height();
    ((room / lay::ROW_H) as usize).min(lay::ROWS)
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

/// 입력 칸 (왼쪽, 오른쪽) 가장자리를 흐릴 폭, 글이 가려진 쪽만 가려진 만큼 `max`까지, 캐럿을 넘어 들어오진 않음
/// `field`는 그라데이션을 까는 클립의 (왼쪽, 오른쪽) 끝, `text`는 글의 (왼쪽, 오른쪽) 끝
/// `caret`은 (조합 글자 앞, 뒤) x라 조합 중이면 조합 글자도 안 흐림, x는 모두 클라이언트 px
fn edge_fades(field: (f32, f32), text: (f32, f32), caret: (f32, f32), max: f32) -> (f32, f32) {
    let left = (field.0 - text.0).min(caret.0 - field.0);
    let right = (text.1 - field.1).min(field.1 - caret.1);
    (left.clamp(0.0, max), right.clamp(0.0, max))
}

/// 버튼이 들어가는 동안 입력 칸을 민 거리, 들어가기 시작할 때의 거리 `s0`를 바 진행 `r` (시작 `r0`)에 비례해 줄임
/// 바가 넘쳐 `r`이 0 아래로 가면 글도 같이 넘침, 캐럿 x `caret`이 보이는 폭 `room` 안에 오게 자름
fn return_scroll(s0: f32, r0: f32, r: f32, caret: f32, room: f32) -> f32 {
    (s0 * r / r0).clamp(caret - room, caret)
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
            data_notice: None,
            version_notice: None,
            version_seen: false,
            status_line: None,
            card_now: None,
            hotkey: Hotkey::DEFAULT,
            hotkey_wait: None,
            hotkey_note: None,
            mode_choice: Mode::B4,
            diff_choice: Diff::NM,
            library: Library::default(),
            playlist: None,
            playlist_request: None,
            keep_query: true,
            show_dlc: false,
            kept_row: None,
            edit: Editor::default(),
            preedit: String::new(),
            high_surrogate: None,
            results: Vec::new(),
            bpm_rows: false,
            history_tab: HistoryTab::Recent,
            history: Vec::new(),
            history_entries: Vec::new(),
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
            toggle_down: None,
            keyword_edit: None,
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
            seg: [Tween::new(0.0); 3],
            sheet_swap: Crossfade::new(),
            list_swap: Crossfade::new(),
            star: None,
            toggles: [ToggleMotion::new(), ToggleMotion::new()],
            hover_since: None,
            leave_since: None,
            in_zone: false,
            tracking: false,
            shown_at: now,
            last: now,
            moving: false,
            caret_t0: now,
            text_scroll: 0.0,
            text_return: None,
            dragging: false,
            last_mouse: None,
            jackets: HashMap::new(),
            fonts: None,
            fx: None,
            beams: None,
        }
    }

    /// 닫힌 동안 새 목록으로 교체, 인덱스로 남긴 선택은 곡 id로 다시 찾음
    pub fn replace_catalog(
        &mut self,
        mut next: Catalog,
        changed_jackets: &HashSet<u32>,
        owned: Option<HashSet<u32>>,
    ) -> std::result::Result<(), Box<Catalog>> {
        if self.visible {
            return Err(Box::new(next));
        }
        self.restore_keyword_query();
        let remap = |row| match row {
            Row::Song(i) => self.catalog.songs.get(i).and_then(|s| next.pick(s.id, owned.as_ref())).map(Row::Song),
            Row::Chart(i, mode, diff) => {
                let i = self.catalog.songs.get(i).and_then(|s| next.pick(s.id, owned.as_ref()))?;
                next.songs.get(i)?.level(mode, diff).map(|_| Row::Chart(i, mode, diff))
            }
            Row::Random | Row::Unavailable(_) => Some(row),
            Row::Playlist(_) => None,
        };
        let kept_row = self.kept_row.and_then(|(row, scroll)| remap(row).map(|row| (row, scroll)));
        let restore = self.playlist.as_ref().and_then(|p| p.restore).and_then(|(row, scroll)| remap(row).map(|row| (row, scroll)));
        next.inherit_keywords(&mut self.catalog);
        self.catalog = next;
        self.owned = owned;
        self.retain_recent();
        self.kept_row = kept_row;
        if let Some(p) = &mut self.playlist {
            p.restore = restore;
        }
        self.rolls = Rolls::default();
        self.view = View::Results;
        self.sheet = Sheet::Results;
        self.sel_of = Sheet::Results;
        self.fill_history();
        let query = self.query();
        self.results = if query.trim().is_empty() {
            Vec::new()
        } else if let Some(id) = playlist::id(&query) {
            self.playlist.as_ref().filter(|p| p.id == id).and_then(|p| p.data.as_ref())
                .map(|data| self.playlist_rows(data)).unwrap_or_default()
        } else {
            self.playlist = None;
            self.playlist_request = None;
            self.find(&query)
        };
        self.bpm_rows = self.catalog.parse_filter(&query).is_some_and(|f| f.bpm.is_some());
        self.searched.clear();
        self.reset_rows();
        self.panel_h.snap(0.0);
        self.shown.snap(0.0);
        self.sheet_swap.reset();
        self.list_swap.reset();
        self.hover = Hit::None;
        self.jackets.retain(|id, _| !changed_jackets.contains(id));
        Ok(())
    }

    /// 받아 둔 곡 데이터가 없던 첫 받기, 열려 있어도 바로 새 목록으로 바꾸고 치던 검색어로 다시 찾음
    /// 빈 목록이라 옮길 선택·재킷이 없음, `owned`는 새 제목으로 다시 계산한 이번 열기의 고를 수 있는 곡
    pub fn fill_catalog(&mut self, mut next: Catalog, owned: Option<HashSet<u32>>) {
        next.inherit_keywords(&mut self.catalog);
        self.catalog = next;
        self.owned = owned;
        self.retain_recent();
        self.kept_row = None;
        self.rolls = Rolls::default();
        self.jackets.clear();
        self.fill_history();
        self.searched.clear();
        self.search_now();
        self.retarget_panel();
    }

    /// 새 목록에 없는 곡은 최근 기록에서 뺌, 플레이리스트는 그대로
    fn retain_recent(&mut self) {
        let ids: HashSet<_> = self.catalog.songs.iter().map(|song| song.id).collect();
        self.library.recent.retain(|entry| match entry {
            Recent::Song(id) => ids.contains(id),
            Recent::Playlist { .. } => true,
        });
    }

    /// 업데이트 상태, 다시 그려야 하면 true
    /// 받아 둔 곡 데이터가 없을 때의 안내, 이번 열기의 새 버전 안내 설명, 설정 머리 줄
    pub fn set_updates(&mut self, data: Option<DataNotice>, version: Option<String>, status: Option<(String, bool)>) -> bool {
        if (data, &version, &status) == (self.data_notice, &self.version_notice, &self.status_line) {
            return false;
        }
        self.data_notice = data;
        self.version_notice = version;
        self.status_line = status;
        if self.visible {
            self.retarget_panel();
        }
        true
    }

    /// 지난번 꺼낸 뒤 새 버전 안내 카드를 그렸는지
    pub fn take_version_seen(&mut self) -> bool {
        std::mem::take(&mut self.version_seen)
    }

    pub fn has_recognition(&self) -> bool {
        self.owned.is_some()
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
                || self.sheet_swap.animating()
                || self.list_swap.animating()
                || self.star.as_ref().is_some_and(|m| !m.done(m.elapsed()))
                || self.toggles.iter().any(|t| !t.done())
                || self.hover_since.is_some()
                || self.leave_since.is_some())
    }

    /// 결과 대신 보여 주는 안내 카드
    /// 곡 데이터가 없으면 결과도 없어서 검색어가 있어도 보임, 나머지는 검색어가 비었을 때만
    /// 첫 인식 전, 목록이 다름, 새 버전 순
    fn card(&self) -> Option<Card> {
        if self.view != View::Results {
            return None;
        }
        match self.data_notice {
            Some(DataNotice::Loading) => return Some(Card::Loading),
            Some(DataNotice::Failed) => return Some(Card::Failed),
            Some(DataNotice::Outdated) => return Some(Card::Outdated),
            None => {}
        }
        if !self.searched.trim().is_empty() {
            None
        } else if self.owned.is_none() {
            Some(Card::Scan)
        } else if self.stale {
            Some(Card::Stale)
        } else {
            self.version_notice.as_ref().map(|_| Card::Update)
        }
    }

    /// 카드의 (제목, 설명, 버튼), 버튼이 없으면 글만
    fn card_text(&self, card: Card) -> (&'static str, &str, Option<&'static str>) {
        match card {
            Card::Loading => ("곡 데이터를 받고 있어요", "잠시만 기다려 주세요", None),
            Card::Failed => ("곡 데이터를 받지 못했어요", "인터넷 연결을 확인해 주세요", Some("다시 시도")),
            Card::Outdated => ("앱을 업데이트해 주세요", OUTDATED_TEXT, Some("릴리즈 페이지")),
            Card::Scan => ("곡 목록을 한 번 읽어올게요", "읽는 동안 잠시 손을 쉬어 주세요", Some("목록 인식 시작")),
            Card::Stale => ("저장된 곡 목록과 화면이 달라요", "계정이나 DLC가 바뀌었다면 다시 읽어 주세요", Some("다시 인식")),
            Card::Update => ("앱을 업데이트해 주세요", self.version_notice.as_deref().unwrap_or_default(), Some("릴리즈 페이지")),
        }
    }

    /// 카드 버튼을 누르거나 Enter
    fn card_action(&self) -> Action {
        match self.card() {
            Some(Card::Scan | Card::Stale) => Action::Scan,
            Some(Card::Failed) => Action::Retry,
            Some(Card::Outdated | Card::Update) => Action::Release,
            Some(Card::Loading) | None => Action::None,
        }
    }

    /// 새 버전 안내 카드가 이번 프레임에 보이는지, 기록은 `presented`에서
    fn version_card_visible(&self) -> bool {
        self.sheet == Sheet::Card
            && self.card_now == Some(Card::Update)
            && self.sheet_geom().alpha > 0.0
            && self.panel_h.value > 0.0
            && alpha_step(self.sheet_swap.shown.value()) > 0.0
    }

    // ---------------------------------------------------------------- 열기 / 닫기

    /// (숨긴) 창을 게임 위에 크기 맞춰 놓고 그 모니터용 그리기 리소스를 만듦
    /// 못 만들면 false
    fn prepare(&mut self) -> bool {
        let game = sys::game_rect();
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
        self.beams = None;
        self.gfx = None;
        self.fonts = None;
        self.fx = None;
        self.jackets.clear();
        self.sheet_swap.reset();
        self.list_swap.reset();
    }

    /// 그리기 리소스를 시작할 때 미리 만들어 첫 열기가 기다리지 않게 함
    pub fn warm_up(&mut self) {
        self.prepare();
    }

    /// 검색창을 띄움, `owned`는 보이는 목록에서 고를 수 있는 곡 (첫 인식 전엔 None)
    /// `stale`이면 곡 이동 중 화면이 저장된 목록과 달랐음
    pub fn show(&mut self, owned: Option<HashSet<u32>>, stale: bool) {
        self.restore_keyword_query();
        if !self.prepare() {
            return;
        }
        if let Some(g) = &self.gfx {
            g.retune(self.hwnd); // 게임이 (그래서 우리도) 다른 모니터로 옮겼을 수 있음
        }
        self.owned = owned;
        self.stale = stale;
        self.version_notice = None; // 이번 열기의 안내는 앱이 연 뒤 정함 (`set_updates`)

        self.dragging = false;
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
        self.reset_query();
        self.retarget_panel();
        self.snap_segments();
        self.toggle_down = None;
        self.toggles[Toggle::KeepQuery as usize].snap(self.keep_query);
        self.toggles[Toggle::ShowDlc as usize].snap(self.show_dlc);
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
        if let Some(Err(e)) = self.beams.as_ref().map(|b| b.open.once(SPIN_SECS)) {
            eprintln!("beams: {e:?}");
        }
    }

    /// 다 사라진 빛의 움직임을 멈춤 (주기 타이머에서 부름)
    pub fn settle(&self) {
        for b in self.beams.iter().flat_map(Beams::all) {
            b.settle();
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
        // 검색이 아직 새 글을 따라잡지 못했으면 지난 결과의 선택은 안 남김
        self.kept_row = if self.keep_query && self.view == View::Results && self.listing() && self.query() == self.searched {
            self.results.get(self.selected).copied().map(|row| (row, self.scroll))
        } else {
            None
        };
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
        for b in self.beams.iter().flat_map(Beams::all) {
            b.stop();
        }
        self.restore_keyword_query();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    // ---------------------------------------------------------------- 리소스

    fn init_resources(&mut self) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        let s = self.scale;
        self.fonts = Some(Fonts {
            input: g.ui_format(18.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            placeholder: g.ui_format(17.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            title: g.ui_format(15.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            sub: g.ui_format(12.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            head: g.ui_format(14.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            item: g.ui_format(13.0 * s, DWRITE_FONT_WEIGHT_MEDIUM)?,
            item_bold: g.ui_format(13.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            item_regular: g.ui_format(13.0 * s, DWRITE_FONT_WEIGHT_NORMAL)?,
            small: g.ui_format(11.0 * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
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

        // 빛을 못 만들면 빛 없이 그대로
        let beams = g.scene().map(|sc| -> Result<Beams> {
            let px = |v: f32| v * s;
            Ok(Beams {
                open: Beam::new(sc, Kind::Spin, px(lay::W), px(lay::BAR_H), px(lay::BAR_R), s)?,
                card: Beam::new(sc, Kind::Line { colorful: false }, px(lay::W), px(lay::CARD_H), px(lay::PANEL_R), s)?,
                random: Beam::new(sc, Kind::Line { colorful: true }, px(lay::W - 2.0 * lay::PAD), px(lay::ROW_H), px(lay::PANEL_R - lay::PAD), s)?,
            })
        });
        self.beams = beams.and_then(|b| b.inspect_err(|e| eprintln!("beams: {e:?}")).ok());
        Ok(())
    }

    fn jacket(&mut self, id: u32) -> Option<ID2D1Bitmap1> {
        if let Some(b) = self.jackets.get(&id) {
            return b.clone();
        }
        let path = self.catalog.jacket_dir.join(format!("{id}.webp"));
        let bmp = dmpicker_update::store::read_limited(&path, dmpicker_update::MAX_JACKET_BYTES).ok().and_then(|bytes| {
            dmpicker_update::archive::validate_webp(&bytes).ok()?;
            image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP).ok()
        }).and_then(|im| {
            let im = im.to_rgba8();
            self.gfx.as_ref()?.bitmap_from_rgba(im.width(), im.height(), im.as_raw()).ok()
        });
        self.jackets.insert(id, bmp.clone());
        bmp
    }

    // ---------------------------------------------------------------- 위치 계산

    fn bar_rect(&self) -> D2D_RECT_F {
        let m = self.px(lay::MARGIN);
        let w = self.px(lay::W) - self.px(lay::BTN_D + lay::BTN_GAP) * self.bar_reveal();
        let h = self.px(lay::BAR_H);
        let (sx, sy) = open_scale(self.open_s.value);
        let (cx, cy) = (m + w / 2.0, m + h / 2.0);
        rect(cx - w * sx / 2.0, cy - h * sy / 2.0, cx + w * sx / 2.0, cy + h * sy / 2.0)
    }

    /// 바가 버튼에 내준 진행, 버튼 하나당 1 (튀어도 조금만 넘침), 바 폭과 글이 돌아오는 거리가 같이 씀
    fn bar_reveal(&self) -> f32 {
        self.reveal.iter().map(|s| s.value.clamp(-0.15, 1.15)).sum()
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
    /// 불투명도는 1/64 단위 (`alpha_step`)
    fn sheet_geom(&self) -> SheetGeom {
        let alpha = alpha_step(self.shown.value());
        let dy = -SHEET_RISE * self.scale * (1.0 - alpha);
        let top = self.panel_top() + dy;
        SheetGeom { rect: rect(self.px(lay::MARGIN), top, self.px(lay::MARGIN + lay::W), top + self.panel_h.value.max(0.0)), alpha, dy }
    }

    /// 보여 줄 게 있으면 그 높이로 나타나고 (다 사라져 있었으면 그 높이에서 바로) 시트 내용을 바꿈
    /// 없으면 높이와 내용을 그대로 둔 채 사라짐, 다 사라지면 높이 0 (`render`)
    /// 나타나다 사라지거나 그 반대로 바뀌어도 지금 불투명도·자리에서 이어짐
    /// 보기나 카드가 바뀌면 반 넘게 보이던 시트는 겹쳐 바꾸고 (`Crossfade`) 반도 안 보였으면 한 프레임에 바꿈, 높이는 같이 움직임
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
        let card = if sheet == Sheet::Card { self.card() } else { self.card_now };
        if sheet != self.sheet || card != self.card_now {
            if self.sheet_geom().alpha >= 0.5 {
                self.sheet_swap.swap();
            } else {
                self.sheet_swap.reset();
            }
            // 탭을 바꾸며 흐려지던 줄 목록은 옛 시트 내용에 같이 들어 있음
            self.list_swap.reset();
        }
        self.card_now = card;
        self.sheet = sheet;
    }

    /// 지금 보기가 시트에 그릴 내용
    fn sheet_now(&self) -> Sheet {
        match self.view {
            View::Settings => Sheet::Settings,
            View::History => Sheet::History,
            View::Keywords => Sheet::Keywords,
            View::Results if self.card().is_some() => Sheet::Card,
            View::Results => Sheet::Results,
        }
    }

    /// 시트 `s`의 줄
    fn rows_of(&self, s: Sheet) -> &[Row] {
        match s {
            Sheet::History => &self.history,
            Sheet::Keywords => &[],
            _ => &self.results,
        }
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
            // DLC 표시도 같은 묶음 (검색어 유지에서 24), 토글은 버튼 줄(356..386) 가운데
            dlc_title: y(354.0),
            dlc: rect(r - self.px(lay::TOGGLE_W), y(371.0 - lay::TOGGLE_H / 2.0), r, y(371.0 + lay::TOGGLE_H / 2.0)),
            dlc_note: y(374.0),
        }
    }

    /// 키워드 시트: 결과 줄과 같은 곡 줄, 라벨, 줄바꿈하며 흐르는 칩, 안내 문구
    /// 그리기·판정·시트 높이가 같이 씀
    fn keyword_layout(&self) -> KeywordLayout {
        let top = self.panel_top();
        let y = |v: f32| top + self.px(v);
        // 완료 버튼은 레벨 칸 자리: 오른쪽 끝을 맞추고 줄 가운데, 높이 30에 글 좌우로 14
        let done_r = self.px(lay::MARGIN + lay::W) - self.px(lay::PAD) - self.px(8.0);
        let done_w = (self.measure("완료", |f| &f.item) + self.px(28.0)).round();
        let cy = y(lay::PAD) + self.px(lay::ROW_H) / 2.0;
        let done = rect(done_r - done_w, cy - self.px(15.0), done_r, cy + self.px(15.0));
        // 별 버튼은 완료 왼쪽 8에 30 x 30, 같은 줄 가운데
        let fav_r = done.left - self.px(8.0);
        let fav = rect(fav_r - self.px(30.0), done.top, fav_r, done.bottom);
        // 곡 줄 아래 8에 라벨 (줄 높이 16), 그 아래 8부터 칩 (높이 28, 세로 간격 6)
        // 안내 문구는 칩 아래 12 (줄 높이 16), 맨 아래 여백은 양옆과 같은 16
        let (h, gap) = (self.px(28.0), self.px(6.0));
        let chips_top = y(88.0);
        let words = self.keyword_song().map_or(&[][..], |s| &s.user_keys[..]);
        let flow = self.chip_flow(words.iter().map(String::as_str));
        // 줄 한도를 넘는 칩은 안 그림, 추가할 땐 막으니 파일을 직접 고친 경우뿐
        let lines = flow.last().map_or(1, |&(.., line)| line + 1).min(lay::CHIP_LINES);
        let chips = flow
            .into_iter()
            .take_while(|&(.., line)| line < lay::CHIP_LINES)
            .map(|(x, cw, line)| {
                let t = chips_top + (h + gap) * line as f32;
                let (bx, by) = (x + cw - self.px(24.0), t + ((h - self.px(20.0)) / 2.0).round());
                (rect(x, t, x + cw, t + h), rect(bx, by, bx + self.px(20.0), by + self.px(20.0)))
            })
            .collect();
        let chips_bottom = chips_top + (h + gap) * lines as f32 - gap;
        let note = chips_bottom + self.px(12.0);
        let bottom = if self.keyword_note().is_some() { note + self.px(16.0) } else { chips_bottom };
        KeywordLayout { fav, done, label: y(lay::PAD + lay::ROW_H + 8.0), chips_top, chips, note, height: bottom + self.px(lay::INSET) - top }
    }

    /// 히스토리 탭 (세그먼트): 패널 안쪽 8에 높이 32, 양 끝이 줄 강조와 같음
    fn tabs_rect(&self) -> D2D_RECT_F {
        let top = self.panel_top();
        let (l, r) = (self.px(lay::MARGIN) + self.px(lay::PAD), self.px(lay::MARGIN + lay::W) - self.px(lay::PAD));
        rect(l, top + self.px(lay::PAD), r, top + self.px(lay::TABS_H))
    }

    /// 시트 `s`의 줄 목록 위 탭 띠 높이, 히스토리에만 있음
    fn tabs_h(&self, s: Sheet) -> f32 {
        if s == Sheet::History { self.px(lay::TABS_H) } else { 0.0 }
    }

    /// 시트 `s`의 첫 줄 위쪽 (패널 위에서), 히스토리는 탭 띠 아래
    fn list_top(&self, s: Sheet) -> f32 {
        self.tabs_h(s) + self.px(lay::PAD)
    }

    /// 칩 자리: 16 정렬선 사이를 흐르며 줄바꿈, 칩마다 (왼쪽, 너비, 줄)
    /// 시트 배치 (`keyword_layout`)와 추가할 자리 검사가 같이 씀
    fn chip_flow<'a>(&self, words: impl IntoIterator<Item = &'a str>) -> Vec<(f32, f32, usize)> {
        let (l, r) = (self.px(lay::MARGIN + lay::INSET), self.px(lay::MARGIN + lay::W - lay::INSET));
        let gap = self.px(6.0);
        let (mut x, mut line) = (l, 0);
        words
            .into_iter()
            .map(|w| {
                // 글 왼쪽 10, 글 뒤 4 띄우고 20 x 20 × 버튼, 오른쪽 4, 한 줄보다 긴 글은 말줄임
                let cw = (self.px(10.0) + self.measure(w, |f| &f.item).ceil() + self.px(28.0)).min(r - l);
                if x > l && x + cw > r {
                    line += 1;
                    x = l;
                }
                let at = (x, cw, line);
                x += cw + gap;
                at
            })
            .collect()
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

    /// 지금 보기에 한 번에 보이는 줄 자리 수, 플레이리스트 결과는 아래 칸을 뺀 만큼
    /// 높이·스크롤·마우스 판정·그리기가 같이 씀
    fn page(&self) -> usize {
        page_above(self.foot_of(self.sheet_now()).as_ref())
    }

    fn visible_rows(&self) -> usize {
        self.rows().len().min(self.page())
    }

    /// 지금 보기의 패널 높이, 보여 줄 게 없으면 0 (줄이 없으면 문구 한 줄)
    fn panel_target(&self) -> f32 {
        match self.sheet_now() {
            Sheet::Settings => self.px(lay::SETTINGS_H),
            Sheet::Keywords => self.keyword_layout().height,
            Sheet::Card => self.px(lay::CARD_H),
            Sheet::Results if self.playlist_waiting() => 0.0,
            _ if !self.listing() => 0.0,
            s => {
                // 플레이리스트 결과는 줄 목록 밑에 아래 칸이 붙음, 고른 줄이 바뀌면 높이도 따라감
                // 히스토리는 줄 목록 위에 탭 띠
                let foot = self.foot_of(s);
                let n = self.rows().len().min(page_above(foot.as_ref()));
                self.tabs_h(s) + self.px(lay::PAD + lay::ROW_H * n.max(1) as f32 + foot.map_or(lay::PAD, |f| f.height()))
            }
        }
    }

    /// 플레이리스트 결과 아래 칸, 검색어를 지워 결과가 사라지는 동안에도 그대로
    /// 효과·목표와 코멘트는 고를 수 있는 항목을 골랐을 때만
    fn foot_of(&self, s: Sheet) -> Option<Foot> {
        if s != Sheet::Results || self.results.is_empty() {
            return None;
        }
        let data = self.playlist_data()?;
        let entry = match self.results.get(self.selected) {
            Some(Row::Chart(..)) => self.playlist_entry(self.selected),
            _ => None,
        };
        let meta = entry.and_then(|e| {
            let effect = e.effect.as_deref().filter(|t| !t.trim().is_empty());
            // 목표 점수는 소수점 뒤 0 없이 (100, 99.5, 99.25)
            let target = e.target_score.map(|t| format!("목표 {t}%"));
            match (effect, target) {
                (Some(effect), Some(target)) => Some(format!("{effect}  ·  {target}")),
                (Some(effect), None) => Some(effect.to_owned()),
                (None, target) => target,
            }
        });
        let comment = entry.and_then(|e| e.note.as_deref()).map(str::trim).filter(|n| !n.is_empty());
        let note = comment.map_or_else(|| format!("{}  ·  {}곡", data.name, data.entries.len()), str::to_owned);
        let lines = self.foot_layout(&note).and_then(|l| {
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe { l.GetMetrics(&mut m) }.ok().map(|_| m.lineCount as usize)
        });
        let lines = lines.unwrap_or_else(|| note.lines().count()).clamp(1, lay::NOTE_LINES);
        Some(Foot { meta, note, comment: comment.is_some(), lines })
    }

    /// 아래 칸 글: 16 정렬선 사이에서 줄바꿈, 줄 높이 19 (늘어난 만큼 글자 위아래에 반씩)
    /// `NOTE_LINES`줄을 넘으면 마지막 줄 끝에 말줄임, 줄 수 (`lineCount`)는 가려진 줄까지 셈
    fn foot_layout(&self, text: &str) -> Option<IDWriteTextLayout> {
        let (g, f) = (self.gfx.as_ref()?, self.fonts.as_ref()?);
        let (l, r) = (self.px(lay::MARGIN + lay::INSET), self.px(lay::MARGIN + lay::W - lay::INSET));
        let lh = lay::NOTE_LINE * self.scale;
        // 반 줄 여유를 둬야 소수점 오차로 마지막 줄이 안 잘림
        let layout = g.layout(text, &f.item, r - l, lh * (lay::NOTE_LINES as f32 + 0.5)).ok()?;
        unsafe {
            // 캐시된 레이아웃이라 처음 한 번만 바꿈 (형식은 한 줄, 줄 간격은 글꼴 기본)
            if layout.GetWordWrapping() != DWRITE_WORD_WRAPPING_EMERGENCY_BREAK {
                // 버퍼가 줄 수보다 작으면 첫 줄도 안 채워 줌
                let mut n = 0;
                let _ = layout.GetLineMetrics(None, &mut n);
                let mut m = vec![DWRITE_LINE_METRICS::default(); n.max(1) as usize];
                layout.GetLineMetrics(Some(&mut m), &mut n).ok()?;
                layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, lh, m[0].baseline + (lh - m[0].height) / 2.0).ok()?;
                layout.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK).ok()?;
            }
        }
        Some(layout)
    }

    /// 고른 줄이 보이게 스크롤을 맞춤, 아래 칸 높이가 바뀌어 보이는 줄 수가 달라져도 빈 자리 없이
    fn fit_scroll(&mut self) {
        let page = self.page();
        let max = self.rows().len().saturating_sub(page);
        let s = self.scroll.min(self.selected.min(max)).max((self.selected + 1).saturating_sub(page));
        if s != self.scroll {
            self.set_scroll(s);
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
        if self.view == View::Keywords {
            let kl = self.keyword_layout();
            if inside(&kl.fav, x, y) {
                return Hit::Favorite;
            }
            if inside(&kl.done, x, y) {
                return Hit::Done;
            }
            // × 버튼은 작아서 칩 오른쪽 끝까지, 위아래는 칩 높이만큼
            if let Some(i) = kl.chips.iter().position(|(c, b)| inside(&rect(b.left, c.top, c.right, c.bottom), x, y)) {
                return Hit::RemoveKeyword(i);
            }
            return Hit::Inside;
        }
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
            for (k, toggle) in [(sl.keep, Toggle::KeepQuery), (sl.dlc, Toggle::ShowDlc)] {
                if inside(&rect(k.left - self.px(6.0), k.top - self.px(3.0), k.right, k.bottom + self.px(3.0)), x, y) {
                    return Hit::Toggle(toggle);
                }
            }
            return Hit::Inside;
        }
        if let Some(card) = self.card() {
            if self.card_text(card).2.is_some() && inside(&self.card_button(), x, y) {
                return Hit::CardButton;
            }
            return Hit::Inside;
        }
        // 히스토리 탭은 칸마다 반씩
        if self.view == View::History {
            let r = self.tabs_rect();
            if inside(&r, x, y) {
                return Hit::Tab(if x < (r.left + r.right) / 2.0 { HistoryTab::Recent } else { HistoryTab::Favorites });
            }
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
        let s = self.sheet_now();
        let top = self.panel_top() + self.list_top(s);
        if y < top || y >= top + pitch * self.visible_rows() as f32 {
            return None;
        }
        let i = ((y - top + off) / pitch) as usize;
        (i < self.rows().len()).then_some(i)
    }

    // ---------------------------------------------------------------- 상태 변경

    /// `i`번째 줄 곡의 키워드 시트를 엶, 고를 수 있는 곡 줄에서만 (설정·안내 카드에선 안 엶)
    fn open_keywords(&mut self, i: usize) -> bool {
        if !matches!(self.sheet_now(), Sheet::Results | Sheet::History) || !self.listing() {
            return false;
        }
        let Some(&row) = self.rows().get(i) else { return false };
        let Some(song) = row.song() else { return false };
        self.complete_composition();
        self.end_drag();
        self.release_toggle();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
        self.keyword_edit = Some(KeywordEdit {
            song,
            from: self.view,
            edit: std::mem::take(&mut self.edit),
            row,
            scroll: self.scroll,
            remove: None,
            note: None,
        });
        self.clear_input();
        self.view = View::Keywords;
        self.hover = Hit::None;
        self.retarget_panel();
        true
    }

    /// 키워드 시트의 곡 (키워드는 `user_keys`)
    fn keyword_song(&self) -> Option<&Song> {
        self.keyword_edit.as_ref().map(|k| &self.catalog.songs[k.song])
    }

    /// 키워드 시트의 곡이 즐겨찾기인지
    fn sheet_favorite(&self) -> bool {
        self.keyword_song().is_some_and(|s| self.library.is_favorite(s.id))
    }

    /// 키워드 시트의 곡을 즐겨찾기에 넣거나 빼고, 바뀐 상태를 돌려줌
    /// 별 모션은 지금 보이는 채움에서 새로 시작, 켤 때마다 점을 새로 뽑음
    fn toggle_sheet_favorite(&mut self) -> bool {
        let Some(id) = self.keyword_song().map(|s| s.id) else { return false };
        let on = self.library.toggle_favorite(id);
        let from = self.star.as_ref().map_or(!on as u8 as f32, |m| m.fill(m.elapsed()));
        self.star = Some(StarMotion::new(from, on, &mut self.rng));
        on
    }

    /// 별 버튼의 (채움 0 .. 1, 아이콘 배율), 모션이 없으면 지금 상태 그대로
    fn star_now(&self) -> (f32, f32) {
        match &self.star {
            Some(m) => {
                let t = m.elapsed();
                (m.fill(t), m.pop(t))
            }
            None => (self.sheet_favorite() as u8 as f32, 1.0),
        }
    }

    /// 한 번 더 Backspace를 누르면 지울 칩
    fn keyword_remove_mark(&self) -> Option<usize> {
        self.keyword_edit.as_ref().and_then(|k| k.remove)
    }

    fn keyword_note(&self) -> Option<&'static str> {
        self.keyword_edit.as_ref().and_then(|k| k.note)
    }

    /// 칩 아래 문구를 바꾸고 지울 칩 표시는 풂, 문구가 바뀌면 시트 높이도 따라감
    fn set_keyword_note(&mut self, note: Option<&'static str>) {
        let Some(k) = &mut self.keyword_edit else { return };
        k.remove = None;
        if std::mem::replace(&mut k.note, note) != note {
            self.retarget_panel();
        }
    }

    /// 입력 칸을 비움, 조합 중인 글자와 가로로 민 거리까지
    fn clear_input(&mut self) {
        self.edit.clear();
        self.preedit.clear();
        self.high_surrogate = None;
        self.seen.clear();
        self.ended = None;
        self.text_scroll = 0.0;
        self.input_changed();
    }

    /// 입력을 검사하고 저장, 되면 입력 칸만 비워 이어서 넣을 수 있게 함
    fn try_add_keyword(&mut self, input: &str) -> bool {
        let Some(song) = self.keyword_song() else { return false };
        let word = input.trim();
        let norm = dmpicker_search::normalize(word);
        let why = if norm.is_empty() {
            Some("글자나 숫자를 넣어 주세요")
        } else if self.catalog.parse_filter(word).is_some() || self.catalog.has_tag_syntax(word) {
            Some("sc15나 bpm150 같은 조건 검색어는 키워드로 쓸 수 없어요")
        } else if song.user_keys.iter().any(|k| dmpicker_search::normalize(k) == norm) {
            Some("이미 있는 키워드예요")
        } else if self.chip_flow(song.user_keys.iter().map(String::as_str).chain([word])).last().is_some_and(|&(.., line)| line >= lay::CHIP_LINES) {
            // 줄 한도를 넘은 칩은 안 보여서 ×로도 Backspace 표시로도 못 지움
            Some("키워드를 더 넣을 자리가 없어요")
        } else {
            None
        };
        if why.is_some() {
            self.set_keyword_note(why);
            return false;
        }
        let mut words = song.user_keys.clone();
        words.push(word.to_owned());
        let saved = self.save_keywords(words);
        if saved {
            self.clear_input();
        }
        saved
    }

    /// `i`번째 키워드를 지움
    fn delete_keyword(&mut self, i: usize) -> bool {
        let Some(song) = self.keyword_song() else { return false };
        let mut words = song.user_keys.clone();
        if i >= words.len() {
            return false;
        }
        words.remove(i);
        self.save_keywords(words)
    }

    /// 바꾼 키워드 목록을 저장, 못 하면 이유를 보여 주고 칩은 그대로
    fn save_keywords(&mut self, words: Vec<String>) -> bool {
        let Some(k) = &self.keyword_edit else { return false };
        let saved = self.catalog.save_keywords(k.song, words);
        self.set_keyword_note(saved.err());
        self.retarget_panel();
        // 칩이 당겨지거나 새로 생겨 커서 아래 칩이 바뀔 수 있음
        if let Some((x, y)) = self.last_mouse.filter(|_| self.tracking) {
            self.hover = self.hit(x as f32, y as f32);
        }
        saved.is_ok()
    }

    /// 빈 칸에서 두 번 눌러 마지막 칩 삭제, 누른 채 반복되는 키는 안 셈
    fn keyword_backspace(&mut self, repeat: bool) {
        if repeat {
            return;
        }
        if let Some(i) = self.keyword_remove_mark() {
            self.delete_keyword(i);
        } else if let Some(i) = self.keyword_song().and_then(|s| s.user_keys.len().checked_sub(1)) {
            self.set_keyword_note(None);
            if let Some(k) = &mut self.keyword_edit {
                k.remove = Some(i);
            }
        }
    }

    /// 열기 전 입력과 보기로 되돌림, 쓰던 키워드는 버림
    /// 검색창을 닫거나 다시 열 때도 불림, 쓰던 키워드가 검색어로 남지 않게
    fn restore_keyword_query(&mut self) -> Option<(Row, usize)> {
        let k = self.keyword_edit.take()?;
        self.star = None;
        self.clear_input();
        self.edit = k.edit;
        self.seen = self.edit.text().to_owned();
        self.view = k.from;
        Some((k.row, k.scroll))
    }

    /// 원래 보기로 돌아가 바뀐 키워드로 다시 검색, 고치던 줄을 고름
    fn close_keywords(&mut self) {
        if self.keyword_edit.is_none() {
            return;
        }
        // 쓰던 키워드의 조합 중인 글자는 검색어로 안 넘어가게 버림
        self.preedit.clear();
        self.notify_composition(CPS_CANCEL);
        let Some((row, scroll)) = self.restore_keyword_query() else { return };
        // 시트에서 즐겨찾기를 바꿨을 수 있음
        if self.view == View::History && self.history_tab == HistoryTab::Favorites {
            self.fill_history();
        }
        // 플레이리스트 결과는 키워드와 상관없어서 다시 받지 않음
        if playlist::id(&self.query()).is_none() {
            self.searched.clear();
        }
        self.search_now();
        self.retarget_panel();
        self.restore_row(row, scroll);
        self.hover = Hit::None;
    }

    /// 키워드 시트를 여닫는 키와 시트 안의 키, 처리했으면 true
    /// 시트에서 입력 편집 키는 아래 공통 처리로 넘김
    fn keyword_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, alt: bool, shift: bool, repeat: bool) -> bool {
        // F2나 메뉴 키 (Apps, Shift+F10), 시트에선 F2만 닫음
        let open_key = !ctrl && !alt && ((!shift && matches!(vk, VK_F2 | VK_APPS)) || (shift && vk == VK_F10));
        if self.view != View::Keywords {
            if open_key {
                self.open_keywords(self.selected);
            }
            return open_key;
        }
        let empty = self.edit.text().is_empty() && self.preedit.is_empty();
        // Backspace 사이에 다른 키를 누르면 지울 칩 표시를 풂
        if vk != VK_BACK || !empty {
            if let Some(k) = &mut self.keyword_edit {
                k.remove = None;
            }
        }
        // Ctrl+D는 별 버튼과 같음, 누른 채 반복되는 키는 안 셈
        if ctrl && !alt && !shift && vk == VIRTUAL_KEY(b'D' as u16) {
            if !repeat {
                self.toggle_sheet_favorite();
            }
        } else if vk == VK_ESCAPE || (open_key && vk == VK_F2) {
            self.close_keywords();
        } else if vk == VK_RETURN {
            self.complete_composition();
            let q = self.query();
            if q.trim().is_empty() {
                self.close_keywords();
            } else {
                self.try_add_keyword(&q);
            }
        } else if vk == VK_BACK && empty {
            self.keyword_backspace(repeat);
        } else {
            // 시트에선 입력 편집만, 보기 전환·모드 바꾸기·곡 고르기는 막음
            return alt || matches!(vk, VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_APPS | VK_F10 | VK_F2);
        }
        true
    }

    /// 보여 줄 내용이 있으면 그 내용으로 바꾸고, 없으면 직전 내용 그대로 사라짐 (`retarget_panel`)
    /// 히스토리는 열 때마다 고른 탭으로 다시 채움, 키워드 시트는 닫고 넘어감
    fn set_view(&mut self, v: View) {
        if self.view == View::Keywords {
            self.close_keywords();
        }
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
        if v == View::Results && self.listing() {
            self.fit_scroll();
            self.scroll_s.snap(self.scroll_s.target);
            self.retarget_selection(true);
        }
    }

    fn toggle_settings(&mut self) {
        self.set_view(if self.view == View::Settings { View::Results } else { View::Settings });
    }

    fn toggle_history(&mut self) {
        self.set_view(if self.view == View::History { View::Results } else { View::History });
    }

    /// 히스토리 탭을 바꾸면 목록과 선택을 처음부터 맞춤
    /// 탭은 제자리, 줄 목록만 겹쳐 바꾸고 높이는 같이 움직임
    fn set_history_tab(&mut self, tab: HistoryTab) {
        if self.history_tab == tab {
            return;
        }
        self.history_tab = tab;
        self.list_swap.swap();
        self.fill_history();
        self.retarget_panel();
    }

    /// 버튼이 눌린 채 보이는 보기, 히스토리에서 연 키워드 시트는 히스토리 안의 한 단계라 히스토리
    fn bead_view(&self) -> View {
        match &self.keyword_edit {
            Some(k) if self.view == View::Keywords => k.from,
            _ => self.view,
        }
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

    /// 입력 글이나 캐럿을 바꾸면 바를 따라 돌아오던 동작을 끝내고 캐럿을 다시 보임
    fn input_changed(&mut self) {
        self.text_return = None;
        self.caret_t0 = Instant::now();
    }

    /// 입력한 글이 바뀜: 글은 바로 보이고 결과는 입력이 멈춘 뒤에 바뀜 (`TIMER_SEARCH`)
    /// 검색어가 비어도 기다림, Enter·Esc·화살표로 글자를 확정하면 IME가 먼저 조합을 끝내고
    /// (글에서 그 글자가 빠짐) 그다음에 입력으로 넘겨주기 때문
    fn requery(&mut self, edit: Edit) {
        self.input_changed();
        // 키워드 입력은 검색하지 않고 안내 문구만 지움
        if self.view == View::Keywords {
            self.set_keyword_note(None);
            return;
        }
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
        if self.view == View::Keywords {
            return false; // 키워드를 쓰는 동안 결과는 그대로
        }
        let q = self.query();
        if q == self.searched {
            return false;
        }
        // 검색어를 지우면 사라지는 동안 지난 결과를 시트에 남겨 둠
        if !q.trim().is_empty() {
            if let Some(id) = playlist::id(&q) {
                self.begin_playlist(id);
            } else {
                self.playlist = None;
                self.playlist_request = None;
                self.results = self.find(&q);
            }
            self.bpm_rows = self.catalog.parse_filter(&q).is_some_and(|f| f.bpm.is_some());
            if self.sel_of == Sheet::Results {
                self.reset_rows();
            }
        }
        self.searched = q;
        self.retarget_panel();
        true
    }

    /// 히스토리와 플레이리스트 사본을 `root`에서 읽음 (앱이 켤 때 부름)
    pub fn load_history(&mut self, root: std::path::PathBuf) {
        self.library = Library::load(root);
    }

    /// 이동을 시작한 줄만 히스토리에 남김, 보고 있는 플레이리스트면 사본도 씀
    /// 곡 검색에서 고른 곡은 플레이리스트의 마지막 패턴을 안 바꿈
    pub fn remember_pick(&mut self, entry: Recent) {
        let shown = self.playlist.as_ref().filter(|p| entry.playlist_id() == Some(&p.id));
        self.library.remember(entry, shown.and_then(|p| p.data.as_ref()));
    }

    /// 검색은 요청만 남기고 작업 스레드는 앱이 시작함
    pub fn take_playlist_request(&mut self) -> Option<String> {
        self.playlist_request.take()
    }

    /// 플레이리스트 주소 검색: 받아 둔 내용이나 사본은 바로 보여 주고 없으면 응답까지 숨김
    /// 어느 쪽이든 새로 받아 오게 요청
    fn begin_playlist(&mut self, id: &str) {
        let shown = self.playlist.take().filter(|p| p.id == id).and_then(|p| p.data);
        let data = shown.or_else(|| self.library.copies.get(id).cloned());
        self.results = data.as_ref().map(|p| self.playlist_rows(p)).unwrap_or_default();
        let waiting = data.is_none();
        self.playlist = Some(PlaylistView { id: id.into(), data, note: None, waiting, restore: None });
        self.playlist_request = Some(id.into());
    }

    /// 고를 수 있는 패턴이면 그 줄: 내 목록에 있는 곡이고 그 모드·난이도 패턴이 있음
    fn pattern_row(&self, p: Pattern) -> Option<Row> {
        let i = self.catalog.pick(p.title, self.owned.as_ref())?;
        self.catalog.songs[i].level(p.mode, p.diff).map(|_| Row::Chart(i, p.mode, p.diff))
    }

    /// 플레이리스트 순서대로 항목마다 한 줄, 고를 수 있는 줄이 있으면 맨 위에 무작위 줄
    fn playlist_rows(&self, data: &Playlist) -> Vec<Row> {
        let rows: Vec<Row> = data
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| self.pattern_row(e.pattern).unwrap_or(Row::Unavailable(i)))
            .collect();
        let random = rows.iter().any(|r| r.selectable()).then_some(Row::Random);
        random.into_iter().chain(rows).collect()
    }

    /// 작업 스레드의 응답, 사본은 히스토리에 있는 플레이리스트만 갱신
    /// 지금 입력한 주소의 응답만 결과에 넣고, 고르던 줄을 새 목록에서 다시 고름
    pub fn playlist_reply(&mut self, reply: Reply) {
        match &reply.result {
            Ok(data) => self.library.update(data),
            Err(Failure::NotFound) => {
                self.library.forget(&reply.id);
                self.fill_history();
                if self.view == View::History {
                    self.retarget_panel();
                }
                if let Some(p) = self.playlist.as_mut().filter(|p| p.id == reply.id) {
                    p.data = None;
                }
            }
            Err(_) => {}
        }
        // 키워드 시트 뒤에 숨은 검색도 이 플레이리스트면 결과를 바꿔 둠, 시트를 닫을 때 고르던 줄을 다시 고름
        let query = self.keyword_edit.as_ref().map_or_else(|| self.query(), |k| k.edit.text().to_owned());
        if playlist::id(&query) != Some(reply.id.as_str()) {
            return;
        }
        let Some(p) = self.playlist.as_mut().filter(|p| p.id == reply.id) else { return };
        let restore = p.restore.or_else(|| {
            let row = self.results.get(self.selected).copied().filter(|r| r.selectable());
            row.filter(|_| self.sel_of == Sheet::Results).map(|r| (r, self.scroll))
        });
        match reply.result {
            Ok(data) => {
                self.results = self.playlist_rows(&data);
                self.playlist = Some(PlaylistView { id: reply.id, data: Some(data), note: None, waiting: false, restore: None });
            }
            Err(error) => {
                // 404면 위에서 내용을 지웠고, 그 밖의 실패는 사본이 있으면 그대로 보여 줌
                if error == Failure::NotFound {
                    self.results.clear();
                }
                p.note = p.data.is_none().then_some(error.note());
                p.waiting = false;
            }
        }
        if self.view == View::Keywords {
            return;
        }
        // 기다리는 동안 숨긴 패널은 아직 결과 시트가 아닐 수 있어 시트를 먼저 맞춘 뒤 줄을 고름
        self.retarget_panel();
        if self.sel_of == Sheet::Results {
            self.reset_rows();
            if let Some((row, scroll)) = restore {
                self.restore_row(row, scroll);
            }
        }
    }

    /// 히스토리의 플레이리스트 줄: 주소를 친 것처럼 결과로 열고 마지막으로 고른 패턴을 고름
    fn open_playlist_history(&mut self, i: usize) {
        let Some(Recent::Playlist { id, last }) = self.history_entries.get(i).cloned() else { return };
        self.complete_composition();
        self.clear_input();
        self.edit.insert(&playlist::url(&id));
        self.seen = self.query();
        self.view = View::Results;
        self.searched.clear();
        self.search_now();
        if let Some(row) = self.pattern_row(last) {
            self.restore_row(row, 0);
        }
    }

    /// 플레이리스트의 이름·전체 항목 수 (`entries.len()`), 사본이나 응답이 있어야 함
    fn playlist_data(&self) -> Option<&Playlist> {
        self.playlist.as_ref()?.data.as_ref()
    }

    /// 줄 자리에 보일 실패 안내, 사본으로 대신 보여 줄 수 있으면 None
    fn playlist_note(&self) -> Option<&'static str> {
        self.playlist.as_ref()?.note
    }

    /// 사본 없이 첫 응답을 기다리는 중, 그동안 결과 패널을 숨김
    fn playlist_waiting(&self) -> bool {
        self.playlist.as_ref().is_some_and(|p| p.waiting)
    }

    /// 플레이리스트 결과의 무작위 줄 아랫줄: 뽑을 곡 수, 다음에 뽑을 수 있는 곡이 적으면 남은 수 (이름은 아래 설명 줄에 나옴)
    fn playlist_random_note(&self) -> Option<String> {
        self.playlist_data()?;
        let n = picks(&self.results).count();
        let m = self.rolls.choices(&self.results).0.count();
        let left = if m < n { format!(" ({m}곡 남음)") } else { String::new() };
        Some(format!("{n}곡 중 하나{left}"))
    }

    /// 플레이리스트 결과를 보는 중 (바의 채보 글자를 숨김), 입력 중인 주소가 아니라 검색한 결과 기준
    fn playlist_results(&self) -> bool {
        self.view == View::Results && self.playlist.as_ref().is_some_and(|p| playlist::id(&self.searched) == Some(&p.id))
    }

    /// 그리는 시트가 플레이리스트 결과, 아래 칸처럼 검색어를 지워 사라지는 동안에도 그대로
    fn playlist_sheet(&self) -> bool {
        self.sheet == Sheet::Results && self.playlist_data().is_some()
    }

    /// 줄 오른쪽 레벨 칸 너비 (DIP), 플레이리스트 결과는 모드까지 들어가게 넓힘
    fn level_w(&self) -> f32 {
        if self.playlist_sheet() { lay::PLAYLIST_LEVEL_W } else { lay::LEVEL_W }
    }

    /// 줄 레벨 칸의 글과 서열표, 무작위 줄·히스토리의 플레이리스트 줄은 None
    /// 플레이리스트 결과는 바에 모드가 안 보이고 항목마다 달라서 "4B · SC 15"
    /// 고를 수 없는 줄도 항목의 패턴으로, 레벨을 모르면 "4B · SC"까지
    /// 그 밖엔 "SC 15", 그 모드·난이도 패턴이 없는 곡은 None
    fn level_text(&self, row: Row) -> Option<(String, Option<f32>)> {
        let (song, m, d) = match row {
            Row::Chart(i, m, d) => (Some(&self.catalog.songs[i]), m, d),
            Row::Song(i) => (Some(&self.catalog.songs[i]), self.mode_choice, self.diff_choice),
            Row::Unavailable(e) => {
                let p = self.playlist_data()?.entries.get(e)?.pattern;
                (self.unavailable_song(e), p.mode, p.diff)
            }
            Row::Random | Row::Playlist(_) => return None,
        };
        let level = song.and_then(|s| Some((s.level(m, d)?, s.floor(m, d))));
        match (self.playlist_sheet(), level) {
            (true, Some((l, fl))) => Some((format!("{} · {} {l}", m.label(), d.label()), fl)),
            (true, None) => Some((format!("{} · {}", m.label(), d.label()), None)),
            (false, level) => level.map(|(l, fl)| (format!("{} {l}", d.label()), fl)),
        }
    }

    /// 결과 `i`번째 줄의 플레이리스트 항목, 무작위 줄은 None
    fn playlist_entry(&self, i: usize) -> Option<&Entry> {
        if self.view != View::Results {
            return None;
        }
        let data = self.playlist_data()?;
        match *self.results.get(i)? {
            Row::Unavailable(entry) => data.entries.get(entry),
            Row::Chart(song, mode, diff) => {
                let pattern = Pattern { title: self.catalog.songs[song].id, mode, diff };
                data.entries.iter().find(|e| e.pattern == pattern)
            }
            _ => None,
        }
    }

    /// 고를 수 없는 줄도 곡 이름을 표시
    fn playlist_row_name(&self, i: usize) -> Option<String> {
        Some(self.song_name(self.playlist_entry(i)?.pattern.title))
    }

    /// 곡 id의 이름, 곡 데이터에 없는 곡은 번호
    fn song_name(&self, id: u32) -> String {
        match self.catalog.songs.iter().find(|s| s.id == id) {
            Some(s) => s.name.clone(),
            None => format!("알 수 없는 곡 ({id})"),
        }
    }

    /// 고를 수 없는 줄 `Row::Unavailable(e)`의 곡, 곡 데이터에 없으면 None (아랫줄 없이 빈 재킷 자리)
    fn unavailable_song(&self, e: usize) -> Option<&Song> {
        let title = self.playlist_data()?.entries.get(e)?.pattern.title;
        self.catalog.songs.iter().find(|s| s.id == title)
    }

    /// 곡 아랫줄에서 작곡가 뒤에 붙는 꼬리, BPM 조건 검색 결과는 늘 BPM, 아니면 DLC 표시를 켰을 때만 DLC
    fn sub_tail(&self, song: &Song, bpm: bool) -> Option<String> {
        if bpm {
            Some(format!("  ·  BPM {}", song.bpm))
        } else {
            self.show_dlc.then(|| format!("  ·  {}", song.dlc))
        }
    }

    /// 히스토리 `row`번째 줄의 플레이리스트: 주소 id, 마지막으로 고른 패턴, 사본
    fn history_playlist(&self, row: usize) -> Option<(&str, Pattern, Option<&Playlist>)> {
        let Row::Playlist(i) = *self.history.get(row)? else { return None };
        let Some(Recent::Playlist { id, last }) = self.history_entries.get(i) else { return None };
        Some((id.as_str(), *last, self.library.copies.get(id)))
    }

    /// 히스토리 플레이리스트 줄의 제목과 아랫줄 (곡 수, 마지막으로 고른 곡)
    /// 사본이 없으면 이름 대신 id, 곡 수는 뺌
    fn history_playlist_text(&self, row: usize) -> Option<(String, String)> {
        let (id, last, copy) = self.history_playlist(row)?;
        let last = format!("마지막 {}", self.song_name(last.title));
        Some(match copy {
            Some(p) => (p.name.clone(), format!("{}곡  ·  {last}", p.entries.len())),
            None => (id.to_owned(), last),
        })
    }

    /// 검색어로 찾은 줄: 조건 검색이면 무작위 줄과 맞는 줄 전부, 아니면 잘 맞는 곡
    /// 레벨이나 패턴 특성 조건이 있으면 맞는 패턴마다, BPM 조건만이면 곡 (설정의 모드·난이도로 이동)
    fn find(&self, q: &str) -> Vec<Row> {
        let Some(f) = self.catalog.parse_filter(q) else {
            // 특성 조건을 치다 만 글은 엉뚱한 곡 이름 결과 대신 빈 목록
            if self.catalog.has_tag_syntax(q) {
                return Vec::new();
            }
            return self.catalog.search(q, LIMIT, self.owned.as_ref()).into_iter().map(Row::Song).collect();
        };
        let mode = f.level.and_then(|l| l.mode).unwrap_or(self.mode_choice);
        let row = |(i, diff)| match diff {
            Some(d) => Row::Chart(i, mode, d),
            None => Row::Song(i),
        };
        let hits = self.catalog.filter(&f, mode, self.owned.as_ref());
        let random = (!hits.is_empty()).then_some(Row::Random);
        random.into_iter().chain(hits.into_iter().map(row)).collect()
    }

    /// 히스토리 줄: 고른 탭에서 지금 목록에 있는 곡과 플레이리스트
    fn fill_history(&mut self) {
        let owned = self.owned.as_ref();
        self.history = match self.history_tab {
            HistoryTab::Recent => {
                self.history_entries.clone_from(&self.library.recent);
                self.history_entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| match e {
                        Recent::Song(id) => self.catalog.pick(*id, owned).map(Row::Song),
                        Recent::Playlist { .. } => Some(Row::Playlist(i)),
                    })
                    .collect()
            }
            HistoryTab::Favorites => {
                self.history_entries.clear();
                self.library.favorites.iter().filter_map(|&id| self.catalog.pick(id, owned).map(Row::Song)).collect()
            }
        };
        if self.sel_of == Sheet::History {
            self.reset_rows();
        }
    }

    /// 다시 열 때 입력과 결과를 준비, 검색어 유지가 켜져 있으면 이번 목록에서 같은 줄을 찾음
    fn reset_query(&mut self) {
        // 닫을 때 조합 중이던 글자까지 보이던 그대로
        let kept = if self.keep_query { self.query() } else { String::new() };
        let row = self.kept_row.take().filter(|_| self.keep_query);
        self.clear_input();
        self.results.clear();
        self.sheet = Sheet::Results;
        // 닫히기 직전에 겹쳐 바꾸던 옛 내용이 새 결과 위에 남지 않게
        self.sheet_swap.reset();
        self.list_swap.reset();
        self.sel_of = Sheet::Results;
        self.searched.clear();
        self.reset_rows();
        self.view = View::Results;
        if !kept.is_empty() {
            self.edit.insert(&kept);
            self.edit.select_all();
            self.seen = kept;
            self.search_now();
            if let Some((row, scroll)) = row {
                self.restore_row(row, scroll);
            }
        }
    }

    /// 같은 줄을 다시 선택, 없으면 첫 줄, 스크롤은 선택 줄이 보이게만 맞춤
    fn restore_row(&mut self, row: Row, scroll: usize) {
        // 사본 없이 응답을 기다리는 플레이리스트는 줄이 아직 없어서 응답이 오면 다시 고름
        if self.view == View::Results {
            if let Some(p) = self.playlist.as_mut().filter(|p| p.data.is_none()) {
                p.restore = Some((row, scroll));
            }
        }
        self.selected = self.rows().iter().position(|r| *r == row).unwrap_or(0);
        self.scroll = scroll;
        self.fit_scroll();
        self.scroll_s.snap(self.scroll as f32 * self.px(lay::ROW_H));
        self.retarget_selection(true);
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
        self.view != View::Keywords && (!self.searched.trim().is_empty() || self.view == View::History)
    }

    /// 고르는 목록 (`sel_of`)의 `i`번째 줄 자리 위쪽, 패널 위에서
    fn row_y(&self, i: usize) -> f32 {
        self.list_top(self.sel_of) + self.px(lay::ROW_H) * i as f32
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
    /// 고를 수 없는 줄은 건너뜀, 목표 너머에 고를 줄이 없으면 목표에서 지금 줄 쪽으로 가장 가까운 줄
    fn move_sel(&mut self, d: i32) {
        if !self.listing() || self.rows().is_empty() {
            return;
        }
        let rows = self.rows();
        // 카드 시트에선 히스토리 줄 번호가 남아 있을 수 있어 목록 안으로 줄여서 셈
        let cur = self.selected.min(rows.len() - 1);
        let target = (cur as i32 + d).clamp(0, rows.len() as i32 - 1) as usize;
        let ok = |i: &usize| rows[*i].selectable();
        let ns = if d >= 0 {
            (target..rows.len()).find(ok).or_else(|| (cur + 1..target).rev().find(ok))
        } else {
            (0..=target).rev().find(ok).or_else(|| (target + 1..cur).find(ok))
        };
        let Some(ns) = ns else { return };
        if ns == self.selected {
            return;
        }
        self.selected = ns;
        self.fit_scroll();
        self.retarget_selection(false);
    }

    /// 휠: 목록을 `d`줄 스크롤, 강조는 자기 자리를 지키거나 커서 아래 줄로 감
    /// 그래서 선택은 늘 강조가 보여주는 줄, 사라지는 시트는 그대로 둠
    fn scroll_by(&mut self, d: i32, cursor: (f32, f32)) {
        if !self.listing() {
            return;
        }
        let page = self.page();
        let max = self.rows().len().saturating_sub(page) as i32;
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
        // 보이는 줄 안에서 가장 가까운 고를 수 있는 줄, 없으면 지금 선택 그대로
        let next = under.unwrap_or(ns + slot);
        let rows = self.rows();
        if let Some(next) = (ns..(ns + page).min(rows.len())).filter(|&i| rows[i].selectable()).min_by_key(|&i| i.abs_diff(next)) {
            self.selected = next;
        }
        self.fit_scroll();
        self.retarget_selection(false);
    }

    fn set_scroll(&mut self, s: usize) {
        self.scroll = s;
        self.scroll_s.target = s as f32 * self.px(lay::ROW_H);
    }

    /// `i`번째 줄을 고름, 무작위 줄이면 하나를 뽑음, 히스토리의 플레이리스트 줄이면 그 결과를 엶
    fn choose(&mut self, i: usize) -> Action {
        if !self.listing() {
            return Action::None; // 검색어를 지운 뒤 남은 결과
        }
        let row = match self.rows().get(i) {
            Some(Row::Random) => self.roll(),
            r => r.copied(),
        };
        if let Some(Row::Playlist(recent)) = row {
            self.open_playlist_history(recent);
            return Action::None;
        }
        if self.view == View::Results {
            if let (Some(Row::Chart(s, mode, diff)), Some(p)) = (row, &self.playlist) {
                return Action::PlaylistPick(p.id.clone(), Pattern { title: self.catalog.songs[s].id, mode, diff });
            }
        }
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

    fn toggle_value(&self, toggle: Toggle) -> bool {
        match toggle {
            Toggle::KeepQuery => self.keep_query,
            Toggle::ShowDlc => self.show_dlc,
        }
    }

    /// 토글 값을 바꿈, 노브와 색이 따로 움직임
    fn set_toggle(&mut self, toggle: Toggle, on: bool) {
        match toggle {
            Toggle::KeepQuery => self.keep_query = on,
            Toggle::ShowDlc => self.show_dlc = on,
        }
        self.toggles[toggle as usize].go(on);
    }

    /// 토글에서 손을 뗌, 노브가 원으로 돌아옴
    fn release_toggle(&mut self) {
        if let Some(toggle) = self.toggle_down.take() {
            self.toggles[toggle as usize].press.go(1.0, QUICK_SECS, QUICK_EASE);
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    /// 세그먼트마다 고른 칸: 모드, 난이도, 히스토리 탭
    fn segment_choices(&self) -> [usize; 3] {
        [self.mode_choice.index(), self.diff_choice.index(), self.history_tab as usize]
    }

    fn snap_segments(&mut self) {
        for (k, idx) in self.segment_choices().into_iter().enumerate() {
            self.seg[k].snap(idx as f32);
        }
    }

    /// 손잡이를 지금 선택으로 보냄, 한 덩어리로 처음엔 빠르게 미끄러지다 부드럽게 멈춤
    /// 라벨 색도 손잡이를 따라감
    fn retarget_segments(&mut self) {
        for (k, idx) in self.segment_choices().into_iter().enumerate() {
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

    /// 모드를 정하지 않은 레벨·패턴 특성 조건 검색은 설정 모드의 패턴이라 다시 찾음
    fn set_mode(&mut self, m: Mode) {
        self.mode_choice = m;
        if self.catalog.parse_filter(&self.searched).is_some_and(|f| f.uses_mode_setting()) {
            self.searched.clear();
            self.search_now();
        }
    }

    fn cycle_diff(&mut self, d: i32) {
        let i = (self.diff_choice.index() as i32 + d).rem_euclid(4) as usize;
        self.diff_choice = Diff::ALL[i];
    }

    /// 입력 칸에 글이 있음 (조합 중인 글자 포함)
    fn typed(&self) -> bool {
        !self.edit.text().is_empty() || !self.preedit.is_empty()
    }

    /// 두 버튼용 호버 머무름·떠남 시간 처리, 앞 버튼이 먼저 움직이고 다른 버튼은 시차 뒤에 따라감
    /// 설정·히스토리 보기 (히스토리에서 연 키워드 시트 포함)에선 늘 나옴
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
        if matches!(self.bead_view(), View::Settings | View::History) {
            want = true;
        }
        if self.closing {
            return;
        }
        if want == self.beads_out {
            return;
        }
        self.beads_out = want;
        // 들어갈 땐 지금 민 거리에서 바와 같이 돌아옴 (`return_scroll`)
        let r = self.bar_reveal();
        self.text_return = (!want && r > 0.0).then_some((self.text_scroll, r));
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
                self.set_keyword_note(None);
                // 선택이 있으면 먼저 지우고 그 자리에서 조합 (검색은 이어 오는 조합 메시지가 함)
                self.edit.delete_selection();
                self.input_changed();
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
                // 이 키로 생긴 글자는 버림, 받은 단축키 뒤에 오는 공백 같은 게 검색어로 안 들어가게
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
                // F10은 Alt 없이도 WM_SYSKEYDOWN으로 와서 메시지 종류 대신 Alt 눌림 비트(29)를 봄
                let alt = lp.0 & (1 << 29) != 0 || unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if self.keyword_key(vk, ctrl, alt, shift, lp.0 & (1 << 30) != 0) {
                    return Some((0, Action::None));
                }
                // 어디서나 되는 단축키
                if alt && vk == VIRTUAL_KEY(b'M' as u16) {
                    self.cycle_mode(if shift { -1 } else { 1 });
                    return Some((0, Action::Choice));
                }
                if alt && (vk == VK_LEFT || vk == VK_RIGHT) {
                    self.cycle_diff(if vk == VK_LEFT { -1 } else { 1 });
                    return Some((0, Action::Choice));
                }
                // 히스토리 탭은 왼쪽·오른쪽 방향키로 바꿈, 입력 중엔 캐럿을 옮기니 안 바꿈
                if self.view == View::History && !self.typed() && matches!(vk, VK_LEFT | VK_RIGHT) {
                    self.set_history_tab(if vk == VK_LEFT { HistoryTab::Recent } else { HistoryTab::Favorites });
                    return Some((0, Action::None));
                }
                if vk == VK_TAB {
                    self.toggle_settings();
                    return Some((0, Action::None));
                }
                // 입력 칸 편집 단축키는 어느 보기에서나, 조합 중인 글자는 먼저 확정
                if ctrl && !shift && !alt && matches!(vk.0 as u8, b'A' | b'C' | b'X' | b'V') {
                    self.complete_composition();
                    if vk.0 as u8 != b'C' {
                        self.set_keyword_note(None);
                    }
                    match vk.0 as u8 {
                        b'A' => {
                            self.edit.select_all();
                            self.input_changed();
                        }
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
                            self.kb_focus = Some(if self.kb_focus.is_some() { (f + 1).min(5) } else { 0 });
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
                                    self.set_toggle(Toggle::KeepQuery, d > 0);
                                    Action::Choice
                                }
                                5 if (d > 0) != self.show_dlc => {
                                    self.set_toggle(Toggle::ShowDlc, d > 0);
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
                            self.set_toggle(Toggle::KeepQuery, !self.keep_query);
                            Action::Choice
                        }
                        VK_RETURN if f == 5 => {
                            self.set_toggle(Toggle::ShowDlc, !self.show_dlc);
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
                        VK_RETURN if self.card().is_some() => self.card_action(),
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
                            self.move_sel(-(self.page() as i32));
                            Action::None
                        }
                        VK_NEXT => {
                            self.move_sel(self.page() as i32);
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
                            self.input_changed();
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
                    self.input_changed();
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
                    // 스크롤 중 목록 밖으로 나가는 줄엔 강조 자리가 없음, 고를 수 없는 줄은 호버해도 그대로
                    if i != self.selected && self.rows()[i].selectable() && (self.scroll..self.scroll + self.page()).contains(&i) {
                        self.selected = i;
                        // 아래 칸이 높아져 보이는 줄이 줄면 고른 줄이 가려질 수 있음
                        self.fit_scroll();
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
            WM_RBUTTONUP => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                if let Hit::Row(i) = self.hit(x, y) {
                    self.open_keywords(i);
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
                        self.input_changed();
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
                    Hit::Rescan => Action::Scan,
                    Hit::CardButton => self.card_action(),
                    Hit::Hotkey => {
                        self.kb_focus = None;
                        self.start_hotkey_wait();
                        Action::None
                    }
                    // 누르는 동안 노브가 눌리고, 토글 위에서 떼면 바뀜
                    Hit::Toggle(toggle) => {
                        self.kb_focus = None;
                        self.toggle_down = Some(toggle);
                        self.toggles[toggle as usize].press.go(PRESS_Y, PRESS_SECS, PRESS_EASE);
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        Action::None
                    }
                    Hit::Favorite => {
                        self.toggle_sheet_favorite();
                        Action::None
                    }
                    Hit::Tab(tab) => {
                        self.set_history_tab(tab);
                        Action::None
                    }
                    Hit::Done => {
                        self.close_keywords();
                        Action::None
                    }
                    Hit::RemoveKeyword(i) => {
                        self.delete_keyword(i);
                        Action::None
                    }
                    // 예전 검색어 결과가 보이던 줄을 누르면 대신 지금 결과를 띄움
                    Hit::Row(_) if self.search_now() => Action::None,
                    Hit::Row(i) if self.rows()[i].selectable() => {
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
            WM_LBUTTONUP if self.toggle_down.is_some() => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                let toggle = self.toggle_down.unwrap();
                let over = self.hit(x, y) == Hit::Toggle(toggle);
                self.release_toggle();
                if over {
                    self.set_toggle(toggle, !self.toggle_value(toggle));
                    return Some((0, Action::Choice));
                }
                Some((0, Action::None))
            }
            WM_CAPTURECHANGED => {
                self.dragging = false;
                // 다른 데로 캡처를 뺏기면 안 바꾸고 놓음
                if let Some(toggle) = self.toggle_down.take() {
                    self.toggles[toggle as usize].press.go(1.0, QUICK_SECS, QUICK_EASE);
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

    /// 빛을 그린 자리에 맞춤: 바 (열 때 도는 빛), 카드·무작위 줄은 보이면 켜고 아니면 끔
    /// 닫힐 때 줄어드는 변환과 페이드도 같이
    fn place_beams(
        &self,
        b: &Beams,
        shrink: &windows_numerics::Matrix3x2,
        bar: D2D_RECT_F,
        open_t: f32,
        card: Option<(D2D_RECT_F, f32)>,
        random: Option<(D2D_RECT_F, f32)>,
    ) -> Result<()> {
        if let Some(sc) = self.gfx.as_ref().and_then(Gfx::scene) {
            sc.set_transform(shrink)?;
        }
        b.open.place(bar, open_t)?;
        for (beam, want) in [(&b.card, card), (&b.random, random)] {
            if let Some((at, a)) = want {
                beam.place(at, open_t * a)?;
            }
            beam.set(want.is_some())?;
        }
        Ok(())
    }

    /// 줄의 아랫줄을 (`x`, `y`)부터 너비 `w`에 그림, 다 들어가면 꼬리까지 한 덩어리로
    /// 넘치면 꼬리는 다 보이게 두고 앞 글만 말줄임한 뒤 그 바로 뒤에 꼬리를 이음
    /// 서열표 기준선을 맞추게 맨 앞 레이아웃을 돌려줌
    fn draw_sub(&self, x: f32, y: f32, w: f32, text: &str, tail: Option<&str>, b: &ID2D1SolidColorBrush) -> Result<IDWriteTextLayout> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let draw = |text: &str, x: f32, w: f32| -> Result<IDWriteTextLayout> {
            let l = g.layout(text, &f.sub, w, self.px(16.0))?;
            unsafe { g.dc.DrawTextLayout(gfx::pt(x, y), &l, b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT) };
            Ok(l)
        };
        let Some(tail) = tail else { return draw(text, x, w) };
        let full = format!("{text}{tail}");
        if self.measure(&full, |f| &f.sub) <= w {
            return draw(&full, x, w);
        }
        let l = draw(text, x, w - self.measure(tail, |f| &f.sub))?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m)? };
        draw(tail, x + m.widthIncludingTrailingWhitespace, w - m.widthIncludingTrailingWhitespace)?;
        Ok(l)
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
        let foot = self.foot_of(self.sheet);
        let n_page = n_rows.min(page_above(foot.as_ref()));
        let shown: Vec<usize> = if listed {
            let first = (off / pitch).floor().max(0.0) as usize;
            let end = ((off / pitch + n_page as f32).ceil() as usize).min(n_rows);
            (first..end).collect()
        } else {
            Vec::new()
        };
        // 고를 수 없는 줄도 곡 데이터에 있는 곡이면 재킷
        let ids: Vec<Option<u32>> = shown
            .iter()
            .map(|&j| match self.rows_of(self.sheet)[j] {
                Row::Unavailable(e) => self.unavailable_song(e).map(|s| s.id),
                row => row.song().map(|i| self.catalog.songs[i].id),
            })
            .collect();
        let jackets: Vec<Option<ID2D1Bitmap1>> = ids.iter().map(|id| id.and_then(|id| self.jacket(id))).collect();
        let kw_jacket = self.keyword_song().map(|s| s.id).and_then(|id| self.jacket(id));

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
        let (mut sheet_drawn, mut list_drawn) = (None, None);

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

            let text_b = g.brush(TEXT)?;
            let sub_b = g.brush(SUB)?;
            let icon_box = self.px(lay::ICON);

            // 패널 내용은 다 편 시트 자리 기준
            let ct = self.panel_top();
            let (l0, r0) = (panel.left + self.px(lay::INSET), panel.right - self.px(lay::INSET));
            let l0t = l0 - BEARING; // 글자 x, 잉크가 16 정렬선에 맞음

            // 줄 목록 (결과·히스토리): 빈 목록 문구나 선택 강조·줄·아래 칸
            let draw_list = || -> Result<()> {
                let row0 = self.list_top(self.sheet);
                if n_rows == 0 {
                    // 기록이나 즐겨찾기가 있는데 이 목록에 하나도 없을 때도 따로 알림
                    let empty = match self.sheet {
                        Sheet::History if self.history_tab == HistoryTab::Favorites && self.library.favorites.is_empty() => {
                            Some("우클릭 창에서 별을 누른 곡이 여기에 모여요")
                        }
                        Sheet::History if self.history_tab == HistoryTab::Favorites => Some("즐겨찾기한 곡이 이 목록에 없어요"),
                        Sheet::History if self.library.recent.is_empty() => Some("아직 고른 곡이 없어요"),
                        Sheet::History => Some("이 목록에서 고른 곡이 아직 없어요"),
                        // 응답을 기다리느라 결과가 비어 사라지는 중이면 문구 없이
                        _ if self.playlist_waiting() => None,
                        // 플레이리스트는 못 불러온 이유
                        _ => Some(self.playlist_note().unwrap_or("일치하는 곡이 없어요")),
                    };
                    if let Some(empty) = empty {
                        let l = g.layout_centered(empty, &f.item, panel.right - panel.left, self.px(lay::ROW_H))?;
                        dc.DrawTextLayout(gfx::pt(panel.left, ct + row0), &l, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                    return Ok(());
                }
                let rows = self.rows_of(self.sheet);
                // 선택 강조: 한 줄 높이, 모서리가 패널과 동심 (반지름 = PANEL_R - PAD)
                // 고를 수 없는 줄엔 없음
                let inset = self.px(lay::PAD);
                if rows.get(self.selected).is_none_or(|r| r.selectable()) {
                    let hl = g.brush(white(FILL_SELECTED))?;
                    let t = ct + self.sel_y.value;
                    dc.FillRoundedRectangle(&rrect(panel.left + inset, t, panel.right - inset, t + pitch, self.px(lay::PANEL_R - lay::PAD)), &hl);
                }

                let floor_b = g.brush(FAINT)?;
                let off_b = g.brush(DISABLED)?;
                let jsz = self.px(32.0);
                let jr = self.px(lay::PANEL_R - lay::PAD - 8.0);
                let lvl_w = self.px(self.level_w());
                // 줄은 줄 영역 안에서 미끄러짐 (멈추면 딱 보이는 줄만이라 잘림 없음)
                let top = ct + row0;
                dc.PushAxisAlignedClip(&rect(panel.left, top, panel.right, top + pitch * n_page as f32), D2D1_ANTIALIAS_MODE_ALIASED);
                for (k, &j) in shown.iter().enumerate() {
                    let row = rows[j];
                    let y = ct + (row0 + pitch * j as f32 - off);
                    let jx = panel.left + inset + self.px(8.0);
                    let jy = y + (self.px(lay::ROW_H) - jsz) / 2.0;
                    // 고를 수 없는 줄은 제자리에 흐리게
                    let dim = matches!(row, Row::Unavailable(_));
                    match row {
                        // 재킷 자리에 아이콘: 무작위 줄은 셔플, 히스토리의 플레이리스트는 목록
                        Row::Random | Row::Playlist(_) => {
                            dc.FillRoundedRectangle(&rrect(jx, jy, jx + jsz, jy + jsz, jr), &g.brush(white(FILL_HOVER))?);
                            let icon = if row == Row::Random { Icon::Random } else { Icon::Playlist };
                            g.fill_icon(icon, jx + jsz / 2.0, jy + jsz / 2.0, icon_box, &text_b)?;
                        }
                        _ => match &jackets[k] {
                            Some(bmp) => draw_jacket(g, bmp, jx, jy, jsz, jr, if dim { DISABLED.a } else { 1.0 })?,
                            // 곡 데이터에 없는 곡은 빈 재킷 자리
                            None if dim && ids[k].is_none() => {
                                dc.FillRoundedRectangle(&rrect(jx, jy, jx + jsz, jy + jsz, jr), &g.brush(white(FILL_TRACK))?);
                            }
                            None => {}
                        },
                    }
                    // 제목, 아랫줄과 그 꼬리
                    let (title, sub, tail) = match row {
                        Row::Random => {
                            let sub = self.playlist_random_note().unwrap_or_else(|| self.rolls.note(rows));
                            (Cow::Borrowed("무작위로 고르기"), Cow::Owned(sub), None)
                        }
                        Row::Playlist(_) => {
                            let (title, sub) = self.history_playlist_text(j).unwrap_or_default();
                            (Cow::Owned(title), Cow::Owned(sub), None)
                        }
                        Row::Unavailable(e) => {
                            let name = self.playlist_row_name(j).unwrap_or_default();
                            let song = self.unavailable_song(e);
                            (Cow::Owned(name), Cow::Borrowed(song.map_or("", |s| s.composer.as_str())), song.and_then(|s| self.sub_tail(s, false)))
                        }
                        Row::Song(i) | Row::Chart(i, ..) => {
                            let song = &self.catalog.songs[i];
                            let tail = self.sub_tail(song, self.bpm_rows && self.sheet == Sheet::Results);
                            (Cow::Borrowed(song.name.as_str()), Cow::Borrowed(song.composer.as_str()), tail)
                        }
                    };
                    let (title_b, line_b) = if dim { (&off_b, &off_b) } else { (&text_b, &sub_b) };
                    let tx = jx + jsz + self.px(10.0);
                    let lvl_r = panel.right - inset - self.px(8.0);
                    let tw = lvl_r - lvl_w - self.px(8.0) - tx;
                    let (title_y, sub_y) = (y + self.px(6.67), y + self.px(26.67));
                    let tl = g.layout(&title, &f.title, tw, self.px(19.0))?;
                    dc.DrawTextLayout(gfx::pt(tx, title_y), &tl, title_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    let sl_ = self.draw_sub(tx, sub_y, tw, &sub, tail.as_deref(), line_b)?;

                    // 레벨 칸: 레벨 "HD 11"은 곡 이름과, 서열표 "13.4"는 아랫줄과 같은 글자 기준선
                    // 그 모드·난이도 패턴이 없는 곡은 비우고 (플레이리스트는 채보만), 서열표가 없는 패턴은 아래만 비움
                    // 고를 수 없는 줄은 서열표도 흐리게
                    let Some((label, floor)) = self.level_text(row) else { continue };
                    let lx = lvl_r - lvl_w;
                    let ll = g.layout(&label, &f.item_bold, lvl_w, self.px(20.0))?;
                    ll.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                    dc.DrawTextLayout(gfx::pt(lx, title_y + baseline(&tl) - baseline(&ll)), &ll, title_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    if let Some(fl) = floor {
                        let fl = g.layout(&format!("{fl:.1}"), &f.sub, lvl_w, self.px(16.0))?;
                        fl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                        let fb = if dim { &off_b } else { &floor_b };
                        dc.DrawTextLayout(gfx::pt(lx, sub_y + baseline(&sl_) - baseline(&fl)), &fl, fb, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                }
                dc.PopAxisAlignedClip();

                // 플레이리스트 결과 아래 칸: 줄 목록 아래 12부터 효과·목표 줄 (16 + 간격 4), 그 아래 코멘트
                // 코멘트가 없거나 무작위 줄을 고르면 이름과 곡 수를 보조 글자로
                if let Some(foot) = &foot {
                    let mut y = ct + (row0 + pitch * n_page as f32) + self.px(lay::FOOT_PAD);
                    if let Some(meta) = &foot.meta {
                        let ml = g.layout(meta, &f.sub, r0 - l0, self.px(16.0))?;
                        ml.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        dc.DrawTextLayout(gfx::pt(l0t, y), &ml, &floor_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                        y += self.px(20.0);
                    }
                    if let Some(nl) = self.foot_layout(&foot.note) {
                        let nb = if foot.comment { &text_b } else { &sub_b };
                        dc.DrawTextLayout(gfx::pt(l0t, y), &nl, nb, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    }
                }
                Ok(())
            };

            // 안내 카드: 제목과 설명을 한 덩어리로 가운데, 버튼은 오른쪽 16 정렬선 (없으면 글이 그 선까지)
            let draw_card = || -> Result<()> {
                let Some(card) = self.card_now else { return Ok(()) };
                let (title, sub, button) = self.card_text(card);
                let tw = r0 - l0 - if button.is_some() { self.px(lay::CARD_BTN_W + 12.0) } else { 0.0 };
                // 두 줄을 재서 카드 안에 한 덩어리로 가운데 정렬
                let tl = g.layout(title, &f.title, tw, self.px(40.0))?;
                let sl_ = g.layout(sub, &f.sub, tw, self.px(40.0))?;
                let (mut tm, mut sm) = (DWRITE_TEXT_METRICS::default(), DWRITE_TEXT_METRICS::default());
                tl.GetMetrics(&mut tm)?;
                sl_.GetMetrics(&mut sm)?;
                let gap = self.px(4.0);
                let y0 = ct + ((self.px(lay::CARD_H) - (tm.height + gap + sm.height)) / 2.0).round();
                dc.DrawTextLayout(gfx::pt(l0t, y0), &tl, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                dc.DrawTextLayout(gfx::pt(l0t, y0 + tm.height + gap), &sl_, &sub_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                if let Some(button) = button {
                    draw_button(g, &f.item_bold, self.card_button(), button, matches!(self.hover, Hit::CardButton), s)?;
                }
                Ok(())
            };
            // 시트 내용 (다 편 시트 자리), 히스토리는 제자리 탭 아래에 겹쳐 바꾸는 줄 목록 `list`
            let draw_sheet = |list: Option<&ID2D1CommandList>| -> Result<()> {
                if self.sheet == Sheet::Settings {
                    let sl = self.settings_layout();
                    let t = g.layout("검색 설정", &f.head, self.px(200.0), self.px(22.0))?;
                    dc.DrawTextLayout(gfx::pt(l0t, sl.title), &t, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    // 머리 줄 오른쪽: 곡 데이터 상태, 새 버전이 있으면 그 안내를 조금 밝게
                    // 오른쪽 정렬선에 붙이고 제목 오른쪽 12 앞에서 말줄임, 글자 기준선은 제목과 같음
                    if let Some((line, bright)) = &self.status_line {
                        let left = l0t + self.measure("검색 설정", |f| &f.head) + self.px(12.0);
                        let st = g.layout(line, &f.sub, (r0 - left).max(0.0), self.px(18.0))?;
                        st.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                        let sb = g.brush(if *bright { SUB } else { FAINT })?;
                        dc.DrawTextLayout(gfx::pt(left, sl.title + baseline(&t) - baseline(&st)), &st, &sb, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
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
                    draw_segments(g, f, sl.mode, &Mode::ALL.map(Mode::label), thumb(0), 9.0, s)?;
                    draw_segments(g, f, sl.diff, &Diff::ALL.map(Diff::label), thumb(1), 9.0, s)?;
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
                    // 토글 줄은 같은 모양: 제목, 토글 왼쪽 12까지 설명, 버튼 줄 가운데 토글
                    for (title, note, title_y, note_y, r, toggle, focus) in [
                        ("검색어 유지", "다시 열면 마지막 검색어가 남아 있어요", sl.keep_title, sl.keep_note, sl.keep, Toggle::KeepQuery, 4),
                        ("DLC 표시", "작곡가 옆에 DLC 이름을 표시해요", sl.dlc_title, sl.dlc_note, sl.dlc, Toggle::ShowDlc, 5),
                    ] {
                        let tl = g.layout(title, &f.item_bold, self.px(200.0), self.px(20.0))?;
                        dc.DrawTextLayout(gfx::pt(l0t, title_y), &tl, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        let nl = g.layout(note, &f.sub, r.left - self.px(12.0) - l0, self.px(18.0))?;
                        dc.DrawTextLayout(gfx::pt(l0t, note_y), &nl, &note_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        let m = &self.toggles[toggle as usize];
                        draw_toggle(g, r, (m.knob.value(), m.tint.value(), m.press.value()), self.hover == Hit::Toggle(toggle) || self.kb_focus == Some(focus), s)?;
                    }
                } else if let (Sheet::Keywords, Some(song)) = (self.sheet, self.keyword_song()) {
                    let kl = self.keyword_layout();
                    // 곡 줄: 결과 줄과 같은 재킷·제목·아랫줄 (선택 강조 없음), 레벨 칸 자리에 별과 완료 버튼
                    let y = ct + self.px(lay::PAD);
                    let jsz = self.px(32.0);
                    let (jx, jy) = (panel.left + self.px(lay::PAD) + self.px(8.0), y + (self.px(lay::ROW_H) - jsz) / 2.0);
                    if let Some(bmp) = &kw_jacket {
                        draw_jacket(g, bmp, jx, jy, jsz, self.px(lay::PANEL_R - lay::PAD - 8.0), 1.0)?;
                    }
                    let tx = jx + jsz + self.px(10.0);
                    let tw = kl.fav.left - self.px(8.0) - tx;
                    let tl = g.layout(&song.name, &f.title, tw, self.px(19.0))?;
                    dc.DrawTextLayout(gfx::pt(tx, y + self.px(6.67)), &tl, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    self.draw_sub(tx, y + self.px(26.67), tw, &song.composer, self.sub_tail(song, false).as_deref(), &sub_b)?;
                    draw_button(g, &f.item, kl.done, "완료", self.hover == Hit::Done, s)?;
                    // 별 버튼: 완료와 같은 바탕에 별, 꺼지면 테두리만 글자색, 켜지면 테두리도 노랑으로 바뀌고 노랑 채움을 겹침 (`StarMotion`)
                    // 켤 땐 가운데를 중심으로 눌렸다 돌아오고 버튼 가운데에서 점이 튐
                    draw_button_fill(g, kl.fav, self.hover == Hit::Favorite, s)?;
                    let (sx, sy) = ((kl.fav.left + kl.fav.right) / 2.0, (kl.fav.top + kl.fav.bottom) / 2.0);
                    let (on, pop) = self.star_now();
                    let on = alpha_step(on);
                    let mix = |a: f32, b: f32| a + (b - a) * on;
                    let ink = color(mix(TEXT.r, FAVORITE.r), mix(TEXT.g, FAVORITE.g), mix(TEXT.b, FAVORITE.b), mix(TEXT.a, FAVORITE.a));
                    g.fill_icon(Icon::StarOff, sx, sy, icon_box * pop, &g.brush(ink)?)?;
                    if on > 0.0 {
                        g.fill_icon(Icon::StarOn, sx, sy, icon_box * pop, &g.brush(with_alpha(FAVORITE, on))?)?;
                    }
                    if let Some(m) = &self.star {
                        let t = m.elapsed();
                        for sp in &m.sparks {
                            let Some((reach, scale, a)) = sp.at(t) else { continue };
                            let (dx, dy) = (sp.angle.cos() * sp.dist * reach * s, sp.angle.sin() * sp.dist * reach * s);
                            dc.FillEllipse(&circle(sx + dx, sy + dy, sp.size / 2.0 * scale * s), &g.brush(with_alpha(FAVORITE, alpha_step(a)))?);
                        }
                    }

                    // 라벨·빈 칸 문구·안내 문구는 줄 높이 안 가운데, 잉크가 16 정렬선에 맞음
                    let faint_b = g.brush(FAINT)?;
                    let text_line = |text: &str, font: &IDWriteTextFormat, top: f32, h: f32, b: &ID2D1SolidColorBrush| -> Result<()> {
                        let l = g.layout(text, font, r0 - l0, h)?;
                        l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        dc.DrawTextLayout(gfx::pt(l0t, top), &l, b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        Ok(())
                    };
                    text_line("내 키워드", &f.sub, kl.label, self.px(16.0), &sub_b)?;
                    if song.user_keys.is_empty() {
                        text_line("아직 없어요", &f.item_regular, kl.chips_top, self.px(28.0), &faint_b)?;
                    }
                    // 한 번 더 Backspace를 누르면 지울 칩은 진하게, ×는 호버하거나 지울 칩이면 밝게
                    let mark = self.keyword_remove_mark();
                    for (i, ((c, b), word)) in kl.chips.iter().zip(&song.user_keys).enumerate() {
                        let marked = mark == Some(i);
                        dc.FillRoundedRectangle(&rounded(*c, self.px(8.0)), &g.brush(white(if marked { FILL_STRONG } else { FILL_HOVER }))?);
                        let wx = c.left + self.px(10.0);
                        let wl = g.layout(word, &f.item, b.left - self.px(4.0) - wx, c.bottom - c.top)?;
                        wl.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        dc.DrawTextLayout(gfx::pt(wx, c.top), &wl, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                        let xb = if marked || self.hover == Hit::RemoveKeyword(i) { &text_b } else { &faint_b };
                        g.fill_icon(Icon::Remove, (b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0, self.px(lay::CHIP_X), xb)?;
                    }
                    if let Some(note) = self.keyword_note() {
                        text_line(note, &f.sub, kl.note, self.px(16.0), &sub_b)?;
                    }
                } else if self.sheet == Sheet::History {
                    let at = self.seg[2].value();
                    draw_segments(g, f, self.tabs_rect(), &["최근", "즐겨찾기"], (at, at + 1.0), lay::PANEL_R - lay::PAD, s)?;
                    if let Some(list) = list {
                        self.list_swap.draw(dc, list);
                    }
                } else if self.sheet == Sheet::Card {
                    draw_card()?;
                } else if listed {
                    draw_list()?;
                }
                Ok(())
            };

            // 3) 패널 내용 기록: 7)에서 이걸 그리고, 다음 프레임에 보기·카드·탭이 바뀌면 흐려 보냄 (`Crossfade`)
            // 히스토리 줄 목록은 탭만 바뀔 때 혼자 흐려 보내려고 먼저 따로 기록
            if has_panel {
                let list = if self.sheet == Sheet::History { Some(record(dc, draw_list)?) } else { None };
                sheet_drawn = Some(record(dc, || draw_sheet(list.as_ref()))?);
                list_drawn = list;
            }

            // 4) 합성: 그림자, 판, 테두리 빛 순 (효과 출력은 이미 제자리라 변환 없이 그림)
            dc.SetTarget(&target);
            let d = Drawing::begin(dc, &IDENTITY);
            dc.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
            dc.PushLayer(&gfx::layer(open_t, None), None);
            for img in [fx.shadow.output()?, fx.plate.GetOutput()?, fx.rim_light.GetOutput()?] {
                dc.DrawImage(&img, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            }
            dc.SetTransform(&shrink);
            // 닫을 땐 글과 줄이 판보다 먼저 흐려짐, 뒤의 게임이 또렷이 비치기 전에 사라짐
            if self.closing {
                dc.PushLayer(&gfx::layer(((fade - CLOSE_TEXT_GONE) / (1.0 - CLOSE_TEXT_GONE)).clamp(0.0, 1.0), None), None);
            }

            // 5) 검색 바 내용
            let cy = (bar.top + bar.bottom) / 2.0;
            // 입력 칸 앞: 검색은 돋보기, 키워드 입력은 같은 자리에 #
            let keywords = self.view == View::Keywords;
            let lead = if keywords { Icon::Keyword } else { Icon::Search };
            g.fill_icon(lead, bar.left + self.px(24.0), cy, icon_box, &sub_b)?;

            // 곡 이동 때 고를 채보 "4B · HD", 보여주기만 함 (설정은 톱니 버튼이나 Tab으로 엶)
            // 레벨 조건 검색 중엔 실제로 적용될 모드·난이도, 키워드 입력 중엔 안 보임
            // 플레이리스트 결과는 항목마다 채보가 정해져 있어 안 보임
            let cr = self.cond_rect();
            if !keywords && !self.playlist_results() {
                let cond = match self.catalog.parse_filter(&self.searched).filter(|_| self.view == View::Results) {
                    Some(Filter { level: Some(q), .. }) => format!("{} · {}", q.mode.unwrap_or(self.mode_choice).label(), q.diff.label()),
                    // 레벨 조건 없이 특성 조건이 있으면 난이도는 줄마다 달라서 모드만
                    Some(c) if !c.tags.is_empty() => self.mode_choice.label().to_owned(),
                    None if self.view == View::Results && self.catalog.has_tag_syntax(&self.searched) => self.mode_choice.label().to_owned(),
                    _ => format!("{} · {}", self.mode_choice.label(), self.diff_choice.label()),
                };
                let cl_ = g.layout_centered(&cond, &f.small, cr.right - cr.left, cr.bottom - cr.top)?;
                dc.DrawTextLayout(gfx::pt(cr.left, cr.top), &cl_, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }

            let (tx, _) = self.text_origin();
            let tw = (cr.left - self.px(8.0) - tx).max(10.0);
            let q = self.query();
            if q.is_empty() {
                let ph = g.brush(FAINT)?;
                let hint = if keywords { "추가할 키워드를 입력해 주세요" } else { "곡 · 초성 · 작곡가 · 디맥지지 플리" };
                let l = g.layout(hint, &f.placeholder, tw, bar_h)?;
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
            // 버튼이 들어가는 동안엔 바와 같이 돌아오고 (`return_scroll`), 다 들어가 멈추면 원래대로
            let caret_x = text_x(&l, &q, c + self.preedit.len());
            let end = text_x(&l, &q, q.len());
            let room = tw - self.px(3.0);
            if self.reveal.iter().all(Spring::settled) {
                self.text_return = None;
            }
            self.text_scroll = match self.text_return {
                Some((s0, r0)) => return_scroll(s0, r0, self.bar_reveal(), caret_x, room),
                None => {
                    let mut scroll = self.text_scroll;
                    if caret_x - scroll > room {
                        scroll = caret_x - room;
                    } else if caret_x < scroll {
                        scroll = caret_x;
                    }
                    scroll.clamp(0.0, (end - room).max(0.0))
                }
            };
            let ox = tx - self.text_scroll;
            let field = rect(tx - self.px(2.0), bar.top, tx + tw, bar.bottom);
            dc.PushAxisAlignedClip(&field, D2D1_ANTIALIAS_MODE_ALIASED);
            // 글이 가려진 쪽 가장자리는 잘린 끝 대신 흐리게, 캐럿은 빼고
            let (fl, fr) = edge_fades((field.left, field.right), (ox, ox + end), (ox + text_x(&l, &q, c), ox + caret_x), self.px(lay::TEXT_FADE));
            // 레이어 브러시에 대한 우리 참조 (있으면 레이어를 넣은 것), PopLayer 뒤 이 블록이 끝날 때 해제
            let edge_brush = if fl > 0.0 || fr > 0.0 {
                let w = field.right - field.left;
                let edge = |f: f32| if f > 0.0 { 0.0 } else { 1.0 };
                let stops = [(0.0, edge(fl)), (fl / w, 1.0), (1.0 - fr / w, 1.0), (1.0, edge(fr))]
                    .map(|(position, a)| D2D1_GRADIENT_STOP { position, color: white(a) });
                let ob = gfx::gradient(&g.dc, &stops, gfx::pt(field.left, 0.0), gfx::pt(field.right, 0.0))?;
                let mut lp = D2D1_LAYER_PARAMETERS1 { contentBounds: field, opacityBrush: std::mem::ManuallyDrop::new(Some(ob.into())), ..gfx::layer(1.0, None) };
                dc.PushLayer(&lp, None);
                std::mem::ManuallyDrop::into_inner(std::mem::replace(&mut lp.opacityBrush, std::mem::ManuallyDrop::new(None)))
            } else {
                None
            };
            let selection = self.edit.selection().filter(|_| self.preedit.is_empty());
            if let Some((a, b)) = selection {
                let sb = g.brush(white(FILL_STRONG))?;
                let (x0, x1) = (ox + text_x(&l, &q, a), ox + text_x(&l, &q, b));
                dc.FillRoundedRectangle(&rrect(x0, cy - self.px(11.0), x1, cy + self.px(11.0), self.px(3.0)), &sb);
            }
            dc.DrawTextLayout(gfx::pt(ox, bar.top), &l, &text_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            if edge_brush.is_some() {
                dc.PopLayer();
            }
            let blink_on = ((now - self.caret_t0).as_millis() / CARET_BLINK_MS as u128).is_multiple_of(2);
            if blink_on && self.preedit.is_empty() && selection.is_none() && !self.closing {
                let cx = ox + caret_x + self.px(1.0);
                let caret = g.brush(CARET)?;
                dc.FillRectangle(&rect(cx, cy - self.px(10.0), cx + (1.5 * s).round(), cy + self.px(10.0)), &caret);
            }
            dc.PopAxisAlignedClip();

            // 6) 히스토리·톱니 버튼, 아이콘은 버튼마다 따로 나타남 (`bead_shown`)
            // 그 보기가 열려 있으면 눌린 채 (`bead_view`), 아니면 호버할 때 밝아짐
            let ia = self.icon_alpha.value();
            let pressed = self.bead_view();
            let icons = [(BEAD_HISTORY, Hit::History, View::History, Icon::History), (BEAD_GEAR, Hit::Gear, View::Settings, Icon::Settings)];
            for (k, hit, view, icon) in icons {
                let (bx, by, brr) = beads[k];
                let ia = ia * self.bead_shown(k);
                if ia <= 0.01 {
                    continue;
                }
                let lit = self.hover == hit || pressed == view;
                if lit {
                    let hb = g.brush(white(if pressed == view { FILL_SELECTED } else { FILL_HOVER } * ia))?;
                    dc.FillEllipse(&circle(bx, by, brr), &hb);
                }
                let ink = if lit { TEXT } else { SUB };
                g.fill_icon(icon, bx, by, icon_box, &g.brush(with_alpha(ink, ia))?)?;
            }

            // 7) 패널 내용 (움직이는 패널 모양으로 자름, 보기·카드·탭을 바꾸면 겹쳐 바꿈)
            if let Some(drawn) = &sheet_drawn {
                // 마스크는 PopLayer 뒤 이 블록이 끝날 때 해제
                let mask: ID2D1Geometry = g.factory.CreateRoundedRectangleGeometry(&rounded(panel, pr))?.cast()?;
                dc.PushLayer(&D2D1_LAYER_PARAMETERS1 { contentBounds: panel, ..gfx::layer(panel_a, Some(&mask)) }, None);
                // 내용은 다 편 시트 자리에서 시트와 같이 비켜남
                dc.SetTransform(&windows_numerics::Matrix3x2 { M32: shrink.M32 + k * sg.dy, ..shrink });
                self.sheet_swap.draw(dc, drawn);
                dc.SetTransform(&shrink);
                dc.PopLayer();
            }

            if self.closing {
                dc.PopLayer();
            }
            dc.PopLayer();
            d.end()?;
        }
        if let Some(b) = &self.beams {
            // 목록 인식 카드 (첫 인식·다시 인식), 결과에서 선택된 무작위로 고르기 줄 (시트 내용과 같이 비켜나고 바뀌면 새 내용과 같이 나타남)
            let content_a = panel_a * alpha_step(self.sheet_swap.shown.value());
            let scan_card = matches!(self.card(), Some(Card::Scan | Card::Stale));
            let card = (self.sheet == Sheet::Card && scan_card && has_panel).then_some((panel, content_a));
            let list_top = self.panel_top() + sg.dy + self.list_top(self.sheet);
            let list_bottom = (list_top + pitch * n_page as f32).min(panel.bottom);
            let random = (self.sheet == Sheet::Results && has_panel && listed && self.rows_of(self.sheet).get(self.selected) == Some(&Row::Random))
                .then(|| {
                    let inset = self.px(lay::PAD);
                    let y = list_top + pitch * self.selected as f32 - off;
                    (rect(panel.left + inset, y, panel.right - inset, y + pitch), content_a)
                })
                .filter(|(at, _)| at.top >= list_top - 0.5 && at.bottom <= list_bottom + 0.5);
            if let Err(e) = self.place_beams(b, &shrink, bar, open_t, card, random) {
                eprintln!("beams: {e:?}");
            }
        }
        self.sheet_swap.last = sheet_drawn;
        self.list_swap.last = list_drawn;
        self.sheet_swap.retire();
        self.list_swap.retire();
        let seen = self.version_card_visible();
        self.moving = self.animating();
        let presented = self.gfx.as_ref().unwrap().present();
        self.presented(seen, presented)
    }

    /// 화면에 내보낸 프레임에서만 새 버전 안내를 본 걸로 침 (`take_version_seen`)
    fn presented(&mut self, seen: bool, result: Result<()>) -> Result<()> {
        if seen && result.is_ok() {
            self.version_seen = true;
        }
        result
    }
}

/// 레이아웃 첫 줄의 글자 기준선, 위 끝에서 잰 거리
/// 한 줄짜리 글에만 씀, 줄이 더 있으면 버퍼가 모자라 첫 줄도 안 채워지고 0
fn baseline(l: &IDWriteTextLayout) -> f32 {
    let mut m = [DWRITE_LINE_METRICS::default()];
    let mut n = 0;
    let _ = unsafe { l.GetLineMetrics(Some(&mut m), &mut n) };
    m[0].baseline
}

/// 재킷을 (`x`, `y`)에서 한 변 `size`로 줄여 둥근 사각형에 채움, 불투명도 `alpha`
fn draw_jacket(g: &Gfx, bmp: &ID2D1Bitmap1, x: f32, y: f32, size: f32, rad: f32, alpha: f32) -> Result<()> {
    unsafe {
        let bb = g.dc.CreateBitmapBrush(bmp, None, None)?;
        let sz = bmp.GetPixelSize();
        let sc = size / sz.width as f32;
        bb.SetTransform(&windows_numerics::Matrix3x2 { M11: sc, M12: 0.0, M21: 0.0, M22: sc, M31: x, M32: y });
        bb.SetInterpolationMode1(D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC);
        bb.SetOpacity(alpha);
        g.dc.FillRoundedRectangle(&rrect(x, y, x + size, y + size, rad), &bb);
    }
    Ok(())
}

/// 패널 버튼 (설정, 안내 카드, 키워드 완료): 둥근 채움 (`draw_button_fill`)과 가운데 글
fn draw_button(g: &Gfx, font: &IDWriteTextFormat, r: D2D_RECT_F, label: &str, lit: bool, s: f32) -> Result<()> {
    draw_button_fill(g, r, lit, s)?;
    unsafe {
        let l = g.layout_centered(label, font, r.right - r.left, r.bottom - r.top)?;
        g.dc.DrawTextLayout(gfx::pt(r.left, r.top), &l, &g.brush(TEXT)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}

/// 패널 버튼 바탕: 둥근 채움, 호버·포커스·눌림이면 진하게 (글 버튼과 키워드 시트의 별 버튼)
fn draw_button_fill(g: &Gfx, r: D2D_RECT_F, lit: bool, s: f32) -> Result<()> {
    unsafe {
        let fill = g.brush(white(if lit { FILL_STRONG } else { FILL_SELECTED }))?;
        g.dc.FillRoundedRectangle(&rounded(r, (8.0 * s).round()), &fill);
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

/// 세그먼트 컨트롤: 트랙 + 손잡이 (호버 덧칠 없음), 손잡이는 둘레 3 안쪽이라 모서리도 3 작음
fn draw_segments(
    g: &Gfx,
    f: &Fonts,
    r: D2D_RECT_F,
    labels: &[&str],
    thumb: (f32, f32), // 움직이는 손잡이 양 끝, 칸 단위
    rad: f32,          // 트랙 모서리 (DIP)
    s: f32,
) -> Result<()> {
    unsafe {
        let dc = &g.dc;
        let rad = rad * s;
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
