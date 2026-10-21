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
use crate::catalog::{Catalog, DlcPack, Song};
use crate::edit::{clean_paste, Editor};
use crate::gfx::{
    self, alpha_step, circle, color, rect, rounded, rrect, white, with_alpha, Drawing, Gfx, Retarget, BEARING, CARET, DISABLED,
    FAINT, FAVORITE, FILL_HOVER, FILL_SELECTED, FILL_STRONG, FILL_TRACK, SUB, TEXT, TINT,
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
/// 유리 효과 전환도 토글 색과 같이 이걸로 (알림 포함)
pub(crate) const QUICK_SECS: f32 = 0.18;
pub(crate) const QUICK_EASE: Ease = Ease(0.16, 1.0, 0.3, 1.0);
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
/// 보기·탭·카드를 바꿀 때 옛 내용에서 새 내용으로 섞이는 시간 (`Crossfade`)
const SWAP_SECS: f32 = 0.25;
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

/// 설정 시트 키보드 포커스 (`kb_focus`), 화살표로 오가는 위에서 아래 순서
const FOCUS_MODE: usize = 0;
const FOCUS_DIFF: usize = 1;
const FOCUS_EXCLUDE: usize = 2;
const FOCUS_RESCAN: usize = 3;
const FOCUS_HOTKEY: usize = 4;
const FOCUS_KEEP: usize = 5;
const FOCUS_DLC: usize = 6;
const FOCUS_GLASS: usize = 7;

/// 세그먼트 컨트롤 (`seg`) 자리
const SEG_MODE: usize = 0;
const SEG_DIFF: usize = 1;
const SEG_HISTORY: usize = 2;
const SEG_EXCLUDE: usize = 3;

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
    pub const PLAYLIST_LEVEL_W: f32 = 72.0; // 플레이리스트 결과·뽑은 기록은 모드까지, 가장 긴 "8B · MX 15"가 약 66
    pub const SETTINGS_H: f32 = 456.0; // 맨 아래 줄(410..440) 아래 16 (= 오른쪽 여백)
    /// 설정 시트 휠 한 칸, 내 곡 목록부터 아래 묶음끼리의 간격
    pub const SETTINGS_WHEEL: f32 = 54.0;
    /// 키워드 칩·결과 줄 수 한도 기준 높이 (설정 시트만 한 줄 더 김)
    pub const CONTENT_H: f32 = 402.0;
    /// 탭 아래 끝, 제목·요약은 다른 시트 첫 줄 띠(위 8, 높이 48)에 두고 탭은 그 아래 8부터 높이 32
    pub const EXCLUDE_TOP: f32 = 96.0;
    pub const DLC_ROW_H: f32 = 40.0;
    /// DLC 6.6줄, 곡 5.5줄이라 아래에 더 있는 줄이 걸쳐 보임
    pub const EXCLUDE_LIST_H: f32 = 264.0;
    /// 줄이 없을 때 목록 자리, 문구는 이 띠 가운데
    pub const EXCLUDE_EMPTY_H: f32 = 120.0;
    /// 설정 세그먼트 트랙 모서리, 히스토리·검색 제외 탭은 줄 강조와 같은 12
    pub const SEG_R: f32 = 9.0;
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
    /// 뽑은 기록 맨 위 띠: 위 8 + 무작위 줄 + 라벨 (위 8, 줄 높이 16), 그 아래 8은 줄 목록의 PAD
    /// 첫 줄이 키워드 칩처럼 패널 위 88에서 시작 (6줄이면 384, 설정 시트보다 낮음)
    pub const ROLLS_HEAD_H: f32 = PAD + ROW_H + 8.0 + 16.0;
    /// 키워드 칩 줄 한도: 칩 위 88, 아래 안내 문구·여백 44를 빼고 창 높이에 드는 줄 (칩 28 + 간격 6)
    pub const CHIP_LINES: usize = ((PANEL_MAX.max(CONTENT_H) - 88.0 - 44.0 + 6.0) / 34.0) as usize; // 8
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
    flood: ID2D1Effect,  // 판 색, 진하기는 유리 효과를 따라감
    flood_glass: std::cell::Cell<f32>, // 판 색에 넣은 유리 효과 정도
    rim_light: ID2D1Effect,
    _keep: Vec<ID2D1Effect>,
}

pub enum Action {
    None,
    /// 곡 id, 패턴 줄에서 골랐으면 그 (모드, 난이도) (없으면 설정대로)
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
    /// 설정에서 팩·곡별 검색을 켜거나 끔
    Exclude,
    /// 최근 고른 곡·플레이리스트와 즐겨찾기, 히스토리 버튼으로 엶
    History,
    /// 곡 하나의 내 키워드, 결과·히스토리 줄을 우클릭하거나 F2로 엶
    Keywords,
    /// 무작위 줄로 나온 곡, 결과·히스토리의 무작위 줄을 우클릭하거나 F2로 엶
    Rolls,
}

impl View {
    /// 입력 칸으로 시트 목록을 찾는 보기, 원래 입력·검색어는 `list_edit`에 보관
    fn searches_sheet(self) -> bool {
        matches!(self, Self::History | Self::Exclude)
    }
}

/// 히스토리 맨 위 탭, 세그먼트 칸 순서대로
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HistoryTab {
    Recent,
    Favorites,
}

impl HistoryTab {
    const ALL: [Self; 2] = [Self::Recent, Self::Favorites];
}

/// 검색 제외 시트 탭, 세그먼트 칸 순서대로
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExcludeTab {
    Dlc,
    Songs,
}

impl ExcludeTab {
    const ALL: [Self; 2] = [Self::Dlc, Self::Songs];

    fn row_height(self) -> f32 {
        match self {
            Self::Dlc => lay::DLC_ROW_H,
            Self::Songs => lay::ROW_H,
        }
    }
}

/// 시트에 그리는 내용, 보이거나 나타나는 중에만 바뀌고 사라지는 동안엔 그대로 (`retarget_panel`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sheet {
    Settings,
    /// 팩·곡의 검색 제외 목록
    Exclude,
    Card,
    /// 검색 결과 줄
    Results,
    /// 최근 고른 곡·플레이리스트 또는 즐겨찾기 줄
    History,
    /// 곡 줄과 키워드 칩
    Keywords,
    /// 무작위 줄과 나온 곡 줄
    Rolls,
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
    /// 업데이트할 때까지 빈 입력마다 표시
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

/// 시트 안에서 찾는 동안 원래 검색어와 결과 선택을 보관
struct SavedQuery {
    /// 열기 전 입력, 시트에선 입력 칸으로 그 시트 목록을 찾음
    edit: Editor,
    /// 원래 결과의 검색어, 그사이 키워드·모드·곡 목록이 바뀌면 비워서 돌아갈 때 다시 찾음
    searched: String,
    /// 고르던 결과 줄과 스크롤
    row: Option<(Row, usize)>,
}

/// 뽑은 기록 시트 상태와 닫으면 돌아갈 보기·입력·검색어
struct RollEdit {
    from: View,
    /// 열기 전 입력·검색어와 무작위 줄 자리, 닫으면 무작위 줄을 다시 고름
    saved: SavedQuery,
    /// 연 곳 목록의 줄, 이 안에서 나온 곡만 보여 줌
    /// 검색으로 좁혀도 초기화 범위는 이 목록 전체
    scope: Vec<Row>,
    /// 나온 곡 최근 순, 찾는 글이 있으면 맞는 줄만
    rows: Vec<Row>,
    /// 찾기 전 기록 수
    total: usize,
}

/// 결과 한 줄
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Row {
    /// 곡 (`songs` 인덱스), 설정의 모드·난이도로 이동
    Song(usize),
    /// 조건 검색·플레이리스트의 패턴, 그 모드·난이도로 이동
    Chart(usize, Mode, Diff),
    /// 조건 검색·플레이리스트 결과 중 무작위 하나
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

/// 섞을 내용들과 비율 (합 1, 다 편 시트 자리, 변환 없음)
type Parts = Vec<(ID2D1CommandList, f32)>;

/// 겹쳐 바꾸기: 지난 프레임에 보인 그림과 새 내용을 같은 자리에서 섞음
/// 비율대로 더해서 양쪽에 같은 글자는 섞는 동안에도 진하기 그대로
/// 보통 겹치기로 덮어 그리면 같은 글자가 50 ms쯤 69 %까지 옅어짐
struct Crossfade {
    /// 새 내용 비율 0 .. 1, 옛 그림은 나머지
    shown: Tween,
    /// 지난 프레임에 보인 내용들과 비율
    last: Parts,
    /// 옛 그림: 바꿀 때 보이던 내용들과 비율
    old: Parts,
}

impl Crossfade {
    fn new() -> Self {
        Self { shown: Tween::new(1.0), last: Vec::new(), old: Vec::new() }
    }

    /// 지난 프레임에 보인 그대로를 옛 그림으로 두고 새 내용은 0부터 섞어 넣음
    /// 한 프레임에 두 번 바뀌면 지난 그림은 이미 옛 그림이라 그대로 둠
    /// 옅은 내용부터 버린 합이 1/64 안일 때까지 버림
    /// 섞는 중에 연달아 바꿔도 몇 장만 남고, 남은 비율을 다시 나눠도 튐이 안 보임
    fn swap(&mut self) {
        if !self.last.is_empty() {
            let mut old = std::mem::take(&mut self.last);
            old.sort_by(|a, b| a.1.total_cmp(&b.1));
            let mut cut = 0.0;
            let faint = old.iter().take_while(|&&(_, a)| {
                cut += a;
                cut < 1.0 / 64.0
            });
            old.drain(..faint.count());
            let sum: f32 = old.iter().map(|&(_, a)| a).sum();
            old.iter_mut().for_each(|(_, a)| *a /= sum);
            self.old = old;
        }
        self.shown.snap(0.0);
        self.shown.go(1.0, SWAP_SECS, Ease::DECEL);
    }

    /// 겹치지 않고 지금 내용만 바로 다 보임
    fn reset(&mut self) {
        self.shown.snap(1.0);
        self.last.clear();
        self.old.clear();
    }

    fn animating(&self) -> bool {
        !self.shown.done()
    }

    fn retire(&mut self) {
        if self.shown.done() {
            self.old.clear();
        }
    }

    /// 이번 프레임에 보일 그림과 그 구성 (다음 `last`), 섞는 중이면 옛 내용들과 `now`를 비율대로 섞음
    /// 옛 그림이 없으면 빈 그림에서 나타남
    /// 섞는 효과는 프레임마다 새로 만듦, 섞은 줄 목록은 시트 옛 그림 안에 참조로 남아서 고쳐 쓰면 그것까지 바뀜
    unsafe fn compose(&self, g: &Gfx, now: ID2D1CommandList) -> Result<(ID2D1Image, Parts)> {
        let w = alpha_step(self.shown.value());
        if w >= 1.0 {
            return Ok((now.cast()?, vec![(now, 1.0)]));
        }
        let mut parts = Vec::new();
        if w > 0.0 {
            parts.push((now, w));
        }
        if self.old.is_empty() {
            parts.push((record(&g.dc, || Ok(()))?, 1.0 - w));
        } else {
            parts.extend(self.old.iter().map(|(l, a)| (l.clone(), a * (1.0 - w))));
        }
        // 지금까지 섞은 것에 다음 장을 (그 장 비율 / 누적 비율)로 섞어 감
        let (mut out, mut sum): (ID2D1Image, f32) = (parts[0].0.cast()?, parts[0].1);
        for (l, a) in &parts[1..] {
            sum += a;
            let mix = g.effect(&CLSID_D2D1CrossFade)?;
            // 무게 x 입력 0 + (1 - 무게) x 입력 1
            gfx::set_input_image(&mix, 0, &l.cast()?);
            gfx::set_input_image(&mix, 1, &out);
            gfx::set_f32(&mix, D2D1_CROSSFADE_PROP_WEIGHT.0 as u32, a / sum)?;
            out = mix.GetOutput()?;
        }
        Ok((out, parts))
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
    /// 뽑은 줄과 뽑은 순번, 지금 결과를 다 뽑으면 그 결과의 것만 지움
    picked: HashMap<Row, u64>,
    /// 마지막으로 매긴 순번, 기록을 지워도 안 줄어듦
    order: u64,
    /// 직전에 뽑은 줄, 새 바퀴의 첫 뽑기에서 뺌
    last: Option<Row>,
}

impl Rolls {
    /// `rows`에 든 줄의 이력만 지움, 처음부터 다시 뽑으니 직전 줄도 잊음
    fn reset(&mut self, rows: &[Row]) {
        self.picked.retain(|p, _| !rows.contains(p));
        self.last = None;
    }

    /// `rows`에 든 뽑은 줄, 최근에 뽑은 것부터
    fn recent(&self, rows: &[Row]) -> Vec<Row> {
        let mut picked: Vec<_> = self.picked.iter().filter(|(row, _)| rows.contains(row)).collect();
        picked.sort_unstable_by_key(|(_, order)| std::cmp::Reverse(**order));
        picked.into_iter().map(|(&row, _)| row).collect()
    }

    /// 다음에 뽑을 수 있는 줄과 새 바퀴 여부, 이력은 안 바꿈
    /// 안 뽑은 줄이 없으면 새 바퀴라 직전 줄만 뺌 (줄이 하나뿐이면 그것)
    fn choices<'a>(&'a self, rows: &'a [Row]) -> (impl Iterator<Item = Row> + 'a, bool) {
        let restart = picks(rows).all(|r| self.picked.contains_key(&r));
        let skip = if restart && picks(rows).nth(1).is_some() { self.last } else { None };
        let choices = picks(rows).filter(move |&r| if restart { Some(r) != skip } else { !self.picked.contains_key(&r) });
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
            self.picked.retain(|p, _| !rows.contains(p));
        }
        let row = choices[(r % choices.len() as u64) as usize];
        self.order += 1;
        self.picked.insert(row, self.order);
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
    /// 설정의 검색 제외 버튼, 첫 인식 전엔 안 걸림
    Exclude,
    ExcludeRow(usize),
    ExcludeButton(usize),
    DoneExclude,
    Hotkey,
    Toggle(Toggle),
    CardButton,
    Row(usize),
    /// 히스토리 맨 위 탭의 한 칸
    Tab(HistoryTab),
    /// 키워드 시트의 별 (즐겨찾기) 버튼
    Favorite,
    /// 검색 제외 시트 탭의 한 칸
    ExcludeTab(ExcludeTab),
    /// 키워드 시트의 완료 버튼
    Done,
    /// 뽑은 기록 시트의 초기화 버튼, 지울 기록이 있을 때만
    ClearRolls,
    /// 뽑은 기록 시트의 완료 버튼
    DoneRolls,
    /// i번째 키워드 칩의 × 버튼
    RemoveKeyword(usize),
}

/// 설정 시트의 토글, 값이 `toggles`의 자리
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Toggle {
    KeepQuery,
    ShowDlc,
    Glass,
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
    exclude: D2D_RECT_F,
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
    glass_title: f32,
    glass: D2D_RECT_F, // 토글
    glass_note: f32,
}

impl SettingsLayout {
    /// 묶음마다 (위, 아래): 라벨·제목 위부터 컨트롤이나 설명 글 아래까지
    /// 시트를 자르는 자리와 포커스 스크롤이 씀
    fn groups(&self, note_h: f32) -> [(f32, f32); 7] {
        [
            (self.labels[0], self.mode.bottom),
            (self.labels[1], self.diff.bottom),
            (self.list_title, self.rescan.bottom.max(self.list_note + note_h)),
            (self.hotkey_title, self.hotkey.bottom.max(self.hotkey_note + note_h)),
            (self.keep_title, self.keep.bottom.max(self.keep_note + note_h)),
            (self.dlc_title, self.dlc.bottom.max(self.dlc_note + note_h)),
            (self.glass_title, self.glass.bottom.max(self.glass_note + note_h)),
        ]
    }
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

/// 물리 px 기준 뽑은 기록 시트: 맨 위 무작위 줄의 두 버튼과 라벨 줄 위쪽, 줄 목록은 `list_top`부터
struct RollLayout {
    /// 완료 버튼 왼쪽의 초기화 버튼
    clear: D2D_RECT_F,
    done: D2D_RECT_F,
    label: f32,
}

enum ExcludeKind {
    Dlc(DlcPack),
    Song(usize),
}

impl ExcludeKind {
    fn song(&self) -> Option<usize> {
        match self {
            Self::Song(i) => Some(*i),
            Self::Dlc(_) => None,
        }
    }
}

struct ExcludeRow {
    kind: ExcludeKind,
    motion: ToggleMotion,
    removal: Option<ExcludeRemoval>,
}

struct ExcludeRemoval {
    elapsed: f32,
    collapse: bool,
}

impl ExcludeRemoval {
    const FADE: f32 = 0.15;
    const FOLD: f32 = 0.3;

    fn alpha(&self) -> f32 {
        (1.0 - self.elapsed / Self::FADE).max(0.0)
    }

    fn height(&self) -> f32 {
        if !self.collapse {
            return 1.0;
        }
        let t = (self.elapsed / Self::FOLD).min(1.0);
        if t < 0.5 { 1.0 - 4.0 * t.powi(3) } else { 4.0 * (1.0 - t).powi(3) }
    }

    fn done(&self) -> bool {
        self.elapsed >= if self.collapse { Self::FOLD } else { Self::FADE }
    }
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
    /// 새 버전 안내 설명, true면 한 번만 보이는 형식 안내
    version_notice: Option<(String, bool)>,
    /// 형식 안내 카드를 그림, 앱이 꺼내서 기록 (`take_schema_seen`)
    schema_seen: bool,
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
    /// 판 뒤로 흐린 게임 화면을 비침 (앱이 불러오고 저장함), 못 쓰는 PC에서도 값은 그대로 둠
    pub glass: bool,
    /// 윈도우 11이라 켤 수 있음 (`sys::glass_supported`)
    glass_ok: bool,
    /// 열 때 읽은 `sys::glass_paused`, 그동안은 설정이 켜져 있어도 판만 그림
    glass_paused: Option<sys::GlassPause>,
    /// 열 때 흐림 층이 없었음 (만들기 실패), 그동안 토글을 막음
    glass_failed: bool,
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
    /// 히스토리 줄을 찾은 글, 히스토리를 나가 시트가 사라지는 동안에도 그대로
    history_query: String,
    /// 히스토리·검색 제외 보기에서만 Some, 히스토리에서 연 시트도 포함
    list_edit: Option<SavedQuery>,
    /// `selected`·`scroll`이 가리키는 목록 (결과, 히스토리, 뽑은 기록), 보이는 목록이 바뀔 때만 첫 줄로
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
    kb_focus: Option<usize>, // 설정 키보드 포커스 (`FOCUS_*`)
    settings_room: f32, // 패널 위에서 기준 영역 아래 16까지 px, 설정 시트가 더 길면 줄여서 스크롤
    settings_scroll_s: Spring, // 설정 내용을 올린 px, 멈추면 정수
    settings_scrolling: bool, // 스크롤이 멈춘 프레임까지 참, 그동안 커서 아래 호버를 다시 봄
    toggle_down: Option<Toggle>, // 누른 토글 (떼면 바뀜)
    exclude_rows: Vec<ExcludeRow>,
    exclude_tab: ExcludeTab,
    /// 닫히는 동안 원래 검색어로 돌아가도 빈 목록 문구는 유지함
    exclude_filtered: bool,
    exclude_focus: Option<usize>, // 화살표로 고른 줄, 마우스를 움직이면 풀림
    exclude_down: Option<usize>, // 누른 줄 (떼면 바뀜)
    exclude_space: bool, // 마지막 줄을 빼서 포커스가 없어져도 뒤따르는 공백은 입력하지 않음
    exclude_scroll_s: Spring, // 끝 오프셋은 한 줄보다 작을 수 있어 px로 저장함
    keyword_edit: Option<KeywordEdit>,
    roll_edit: Option<RollEdit>,
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
    seg: [Tween; 4],        // 세그먼트 손잡이 [모드, 난이도, 히스토리 탭, 검색 제외 탭]: 왼쪽 끝, 칸 단위
    sheet_swap: Crossfade,  // 시트 내용, 보기나 카드가 바뀌면 겹쳐 바꿈
    list_swap: Crossfade,   // 히스토리·검색 제외 줄 목록, 탭이 바뀌면 겹쳐 바꿈 (탭은 제자리)
    star: Option<StarMotion>, // 키워드 시트 별 버튼 모션, 시트를 닫으면 버림
    toggles: [ToggleMotion; 3], // 검색어 유지, DLC 표시, 유리 효과
    glass_fade: Tween, // 유리 효과가 보이는 정도, 설정에서 바꾸면 토글 색과 같이 바뀜
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

/// 아래 칸 `foot`을 빼고 시트 높이 기준(`CONTENT_H` 402)에 드는 줄 수, 아래 칸이 없으면 `ROWS`
/// 코멘트가 5줄에서 잘려서 효과 줄이 있어도 5줄은 남음
fn page_above(foot: Option<&Foot>) -> usize {
    let Some(foot) = foot else { return lay::ROWS };
    let room = lay::PANEL_MAX.max(lay::CONTENT_H) - lay::PAD - foot.height();
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

/// 정규화한 검색어 `q`가 이름에 들어 있음, 검색어가 초성만이면 이름의 초성으로도 봄
fn name_matches(name: &str, q: &str) -> bool {
    // 기호만 친 검색어는 정규화하면 비어서 어느 이름에나 들어 있음
    if q.is_empty() {
        return false;
    }
    let name = dmpicker_search::normalize(name);
    let initials = || name.chars().map(|c| dmpicker_search::hangul::chosung_of(c).unwrap_or(c)).collect::<String>();
    name.contains(q) || (q.chars().all(dmpicker_search::hangul::is_compat_consonant) && initials().contains(q))
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

/// 세그먼트 컨트롤 `r`의 `n`칸 중 `x`에 있는 칸
fn segment_at(r: &D2D_RECT_F, x: f32, n: usize) -> usize {
    (((x - r.left) / (r.right - r.left)) * n as f32).clamp(0.0, (n - 1) as f32) as usize
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
            schema_seen: false,
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
            glass: true,
            glass_ok: sys::glass_supported(),
            glass_paused: None,
            glass_failed: false,
            kept_row: None,
            edit: Editor::default(),
            preedit: String::new(),
            high_surrogate: None,
            results: Vec::new(),
            bpm_rows: false,
            history_tab: HistoryTab::Recent,
            history: Vec::new(),
            history_entries: Vec::new(),
            history_query: String::new(),
            list_edit: None,
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
            settings_room: f32::INFINITY,
            settings_scroll_s: Spring::new(0.0, 600.0, 46.0),
            settings_scrolling: false,
            toggle_down: None,
            exclude_rows: Vec::new(),
            exclude_tab: ExcludeTab::Dlc,
            exclude_filtered: false,
            exclude_focus: None,
            exclude_down: None,
            exclude_space: false,
            exclude_scroll_s: Spring::new(0.0, 600.0, 46.0),
            keyword_edit: None,
            roll_edit: None,
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
            seg: [Tween::new(0.0); 4],
            sheet_swap: Crossfade::new(),
            list_swap: Crossfade::new(),
            star: None,
            toggles: [ToggleMotion::new(), ToggleMotion::new(), ToggleMotion::new()],
            glass_fade: Tween::new(0.0),
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
    /// 고를 수 있는 곡 (`owned`)은 비우고 다음 열기 (`show`)에서 새로 받음
    pub fn replace_catalog(&mut self, mut next: Catalog, changed_jackets: &HashSet<u32>) -> std::result::Result<(), Box<Catalog>> {
        if self.visible {
            return Err(Box::new(next));
        }
        self.restore_queries();
        let remap = |row| match row {
            Row::Song(i) => self.catalog.songs.get(i).and_then(|s| next.pick(s.id, None)).map(Row::Song),
            Row::Chart(i, mode, diff) => {
                let i = self.catalog.songs.get(i).and_then(|s| next.pick(s.id, None))?;
                next.songs.get(i)?.level(mode, diff).map(|_| Row::Chart(i, mode, diff))
            }
            Row::Random | Row::Unavailable(_) => Some(row),
            Row::Playlist(_) => None,
        };
        let kept_row = self.kept_row.and_then(|(row, scroll)| remap(row).map(|row| (row, scroll)));
        let restore = self.playlist.as_ref().and_then(|p| p.restore).and_then(|(row, scroll)| remap(row).map(|row| (row, scroll)));
        next.inherit(&mut self.catalog);
        self.catalog = next;
        self.owned = None;
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
        self.restore_roll_query();
        next.inherit(&mut self.catalog);
        self.catalog = next;
        self.owned = owned;
        self.retain_recent();
        self.kept_row = None;
        self.rolls = Rolls::default();
        self.jackets.clear();
        // 히스토리·검색 제외 시트 뒤에 숨은 원래 검색도 돌아갈 때 새 목록으로 다시 찾음
        if let Some(h) = &mut self.list_edit {
            h.searched.clear();
        }
        self.fill_history();
        self.searched.clear();
        if self.view == View::Exclude {
            self.fill_exclude();
        } else {
            self.search_now();
        }
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
    /// 받아 둔 곡 데이터가 없을 때의 안내, 새 버전 안내 설명, 설정 머리 줄
    pub fn set_updates(&mut self, data: Option<DataNotice>, version: Option<(String, bool)>, status: Option<(String, bool)>) -> bool {
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

    /// 지난번 꺼낸 뒤 형식 안내 카드를 그렸는지
    pub fn take_schema_seen(&mut self) -> bool {
        std::mem::take(&mut self.schema_seen)
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
                || !self.glass_fade.done()
                || !self.exclude_scroll_s.settled()
                || !self.settings_scroll_s.settled()
                || self.settings_scrolling
                || self.exclude_rows.iter().any(|r| r.removal.is_some() || !r.motion.done())
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
            Card::Update => ("앱을 업데이트해 주세요", self.version_notice.as_ref().map(|(text, _)| text.as_str()).unwrap_or_default(), Some("릴리즈 페이지")),
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

    /// 형식 안내 카드가 이번 프레임에 보이는지, 기록은 `presented`에서
    fn schema_card_visible(&self) -> bool {
        self.version_notice.as_ref().is_some_and(|(_, schema)| *schema)
            && self.sheet == Sheet::Card
            && self.card_now == Some(Card::Update)
            && self.sheet_geom().alpha > 0.0
            && self.panel_h.value > 0.0
            && alpha_step(self.sheet_swap.shown.value()) > 0.0
    }

    // ---------------------------------------------------------------- 열기 / 닫기

    fn glass_now(&self) -> bool {
        self.glass_ok && self.glass && self.glass_paused.is_none()
    }

    /// 유리 효과를 지금 못 쓰는 이유 (설정 시트 설명 글), 그동안 토글은 흐리고 안 눌림
    /// 저장된 값은 그대로 둠
    fn glass_unavailable(&self) -> Option<&'static str> {
        if !self.glass_ok {
            return Some("Windows 11에서 쓸 수 있어요");
        }
        match self.glass_paused {
            Some(sys::GlassPause::Transparency) => Some("Windows 투명 효과를 켜면 쓸 수 있어요"),
            Some(sys::GlassPause::Power) => Some("절전 모드를 끄면 쓸 수 있어요"),
            Some(sys::GlassPause::Unavailable) => Some("지금은 쓸 수 없어요"),
            None if self.glass_failed => Some("지금은 쓸 수 없어요"),
            None => None,
        }
    }

    /// 멈춘 스프링은 목표 값 그대로, 판 가장자리가 정수 px에 와야 1 px 테두리가 한 줄로 선명함
    /// 움직이는 동안엔 안 맞춤, 맞추면 1 px씩 계단처럼 튐
    fn rest_on_pixels(&mut self) {
        for sp in self.reveal.iter_mut().chain([&mut self.open_s, &mut self.panel_h]) {
            if sp.settled() {
                sp.snap(sp.target);
            }
        }
    }

    /// 판 뒤 흐림이 보이는 정도 (0..1), 흐림 층을 못 만들었으면 0
    fn glass_mix(&self) -> f32 {
        if self.gfx.as_ref().is_some_and(Gfx::has_glass) { glass_level(&self.glass_fade) } else { 0.0 }
    }

    /// (숨긴) 창을 게임 위에 크기 맞춰 놓고 그 모니터용 그리기 리소스를 만듦
    /// 못 만들면 false
    fn prepare(&mut self) -> bool {
        let game = sys::game_rect();
        let anchor = game.unwrap_or_else(sys::cursor_work_area);
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
        self.settings_room = (anchor.bottom - y) as f32 - self.panel_top() - self.px(lay::INSET);

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
        self.glass_paused = sys::glass_paused();
        self.restore_queries();
        if !self.prepare() {
            return;
        }
        self.glass_failed = self.gfx.as_ref().is_some_and(|g| !g.has_glass());
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
        for toggle in [Toggle::KeepQuery, Toggle::ShowDlc, Toggle::Glass] {
            self.toggles[toggle as usize].snap(self.toggle_value(toggle));
        }
        self.glass_fade.snap(self.glass_now() as u8 as f32);
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
        self.restore_queries();
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
        let flood = g.effect(&CLSID_D2D1Flood)?;
        set_plate(&flood, 0.0)?;
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

        self.fx = Some(Effects { goo_blur, sheets, shadow, plate, flood, flood_glass: std::cell::Cell::new(0.0), rim_light, _keep: vec![thresh, erode, ring] });

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
        if matches!(sheet, Sheet::Results | Sheet::History | Sheet::Rolls) && sheet != self.sel_of {
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
            // 탭을 바꾸며 섞이던 줄 목록은 옛 시트 그림에 같이 들어 있음
            self.list_swap.reset();
        }
        self.card_now = card;
        self.sheet = sheet;
    }

    /// 지금 보기가 시트에 그릴 내용
    fn sheet_now(&self) -> Sheet {
        match self.view {
            View::Rolls => Sheet::Rolls,
            View::Settings => Sheet::Settings,
            View::Exclude => Sheet::Exclude,
            View::History => Sheet::History,
            View::Keywords => Sheet::Keywords,
            View::Results if self.card().is_some() => Sheet::Card,
            View::Results => Sheet::Results,
        }
    }

    /// 시트 `s`의 줄
    fn rows_of(&self, s: Sheet) -> &[Row] {
        match s {
            Sheet::Rolls => self.roll_rows(),
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

    /// 바에 채보 "4B · HD"가 보임, 키워드 입력 중·뽑은 기록과 결과 화면의 플레이리스트 주소엔 안 보임
    /// 주소는 검색을 기다리지 않고 지금 글로 봄, 붙여 넣는 순간 입력 칸도 같이 넓어져 글이 나중에 안 튐
    fn shows_cond(&self) -> bool {
        match self.view {
            View::Keywords | View::Rolls => false,
            View::Results => playlist::id(self.edit.text()).is_none(),
            View::Settings | View::Exclude | View::History => true,
        }
    }

    /// 입력 칸 오른쪽 끝, 채보가 안 보이면 그 자리까지 써서 채보 글 끝과 같은 16 정렬선
    fn text_right(&self) -> f32 {
        if self.shows_cond() { self.cond_rect().left - self.px(8.0) } else { self.bar_rect().right - self.px(16.0) }
    }

    fn text_origin(&self) -> (f32, f32) {
        let b = self.bar_rect();
        (b.left + self.px(46.0), b.top)
    }

    /// 지금 스크롤한 자리, 그리기·판정이 같이 씀
    fn settings_layout(&self) -> SettingsLayout {
        self.settings_layout_at(self.panel_top() - self.settings_scroll_s.value)
    }

    fn settings_layout_at(&self, top: f32) -> SettingsLayout {
        let (l, r) = (self.px(lay::MARGIN + lay::INSET), self.px(lay::MARGIN + lay::W - lay::INSET));
        let y = |v: f32| top + self.px(v);
        // 제목은 다른 시트 첫 줄의 곡 제목 자리 (위 8 띠 안 6.67), 검색 제외로 겹쳐 바꿀 때 "검색" 글자가 그대로
        // 그 아래는 글자 기준 한 리듬 (글자는 텍스트 박스보다 ~2 아래): 라벨에서 컨트롤까지 8,
        // 컨트롤에서 다음 라벨까지 16, 묶음 사이 24 (구분선 없이 따로 묶음), 맨 아래 버튼 아래는 오른쪽 여백과 같은 16
        SettingsLayout {
            title: y(lay::PAD + 6.67),
            labels: [y(52.0), y(118.0)],
            mode: rect(l, y(72.0), r, y(104.0)),
            diff: rect(l, y(138.0), r, y(170.0)),
            list_title: y(192.0),
            rescan: rect(r - self.px(88.0), y(194.0), r, y(224.0)), // 두 줄 묶음의 가운데
            exclude: rect(r - self.px(184.0), y(194.0), r - self.px(96.0), y(224.0)),
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
            // 유리 효과도 같은 묶음 (DLC 표시에서 24), 토글은 버튼 줄(410..440) 가운데
            glass_title: y(408.0),
            glass: rect(r - self.px(lay::TOGGLE_W), y(425.0 - lay::TOGGLE_H / 2.0), r, y(425.0 + lay::TOGGLE_H / 2.0)),
            glass_note: y(428.0),
        }
    }

    /// 묶음 자리, 스크롤과 상관없이 시트 위 기준
    fn settings_groups(&self) -> [(f32, f32); 7] {
        // 설명 글줄 높이 14, 텍스트 상자의 아래 여백 제외
        self.settings_layout_at(0.0).groups(self.px(14.0))
    }

    /// 키보드 포커스 `focus`가 든 묶음, 다시 인식은 검색 제외와 같은 묶음
    fn settings_group(&self, focus: usize) -> (f32, f32) {
        self.settings_groups()[if focus <= FOCUS_EXCLUDE { focus } else { focus - 1 }]
    }

    /// 다 들어가면 다 편 높이, 아니면 묶음 가운데 절반 안에서 잘라 반쯤 보이는 줄로 아래가 더 있음을 알림
    /// 그 자리가 안 맞으면 위쪽 가장 가까운 묶음 가운데, 첫 묶음 가운데보다 줄이진 않음
    fn settings_height(&self) -> f32 {
        let full = self.px(lay::SETTINGS_H);
        if self.settings_room >= full {
            return full;
        }
        let groups = self.settings_groups();
        let room = self.settings_room.floor();
        let mut height = ((groups[0].0 + groups[0].1) / 2.0).floor();
        for (top, bottom) in groups {
            let quarter = (bottom - top) / 4.0;
            if room >= top + quarter && room <= bottom - quarter {
                return room.max(height);
            }
            let middle = ((top + bottom) / 2.0).floor();
            if middle <= room {
                height = middle;
            }
        }
        height
    }

    fn settings_scroll_max(&self) -> f32 {
        self.px(lay::SETTINGS_H) - self.settings_height()
    }

    /// 키워드 시트: 결과 줄과 같은 곡 줄, 라벨, 줄바꿈하며 흐르는 칩, 안내 문구
    /// 그리기·판정·시트 높이가 같이 씀
    fn keyword_layout(&self) -> KeywordLayout {
        let top = self.panel_top();
        let y = |v: f32| top + self.px(v);
        let done = self.done_button();
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

    /// 시트 맨 위 줄의 글 버튼: 오른쪽 끝을 `right`에 맞추고 줄 가운데, 높이 30에 글 좌우로 14
    fn head_button(&self, right: f32, label: &str) -> D2D_RECT_F {
        let w = (self.measure(label, |f| &f.item) + self.px(28.0)).round();
        let cy = self.panel_top() + self.px(lay::PAD) + self.px(lay::ROW_H) / 2.0;
        rect(right - w, cy - self.px(15.0), right, cy + self.px(15.0))
    }

    /// 키워드·뽑은 기록 시트의 완료 버튼, 레벨 칸 자리
    fn done_button(&self) -> D2D_RECT_F {
        self.head_button(self.px(lay::MARGIN + lay::W) - self.px(lay::PAD) - self.px(8.0), "완료")
    }

    /// 뽑은 기록 시트: 무작위 줄 오른쪽에 완료와 그 왼쪽 8에 초기화, 라벨은 키워드 시트 라벨 자리
    /// 그리기와 판정이 같이 씀
    fn roll_layout(&self) -> RollLayout {
        let done = self.done_button();
        let clear = self.head_button(done.left - self.px(8.0), "초기화");
        RollLayout { clear, done, label: self.panel_top() + self.px(lay::PAD + lay::ROW_H + 8.0) }
    }

    /// 히스토리 탭 (세그먼트): 패널 안쪽 8에 높이 32, 양 끝이 줄 강조와 같음
    fn tabs_rect(&self) -> D2D_RECT_F {
        let top = self.panel_top();
        let (l, r) = (self.px(lay::MARGIN) + self.px(lay::PAD), self.px(lay::MARGIN + lay::W) - self.px(lay::PAD));
        rect(l, top + self.px(lay::PAD), r, top + self.px(lay::TABS_H))
    }

    /// 시트 `s`의 줄 목록 위 띠 높이, 히스토리는 탭, 뽑은 기록은 무작위 줄과 라벨
    fn head_h(&self, s: Sheet) -> f32 {
        match s {
            Sheet::History => self.px(lay::TABS_H),
            Sheet::Rolls => self.px(lay::ROLLS_HEAD_H),
            _ => 0.0,
        }
    }

    /// 시트 `s`의 첫 줄 위쪽 (패널 위에서), 히스토리·뽑은 기록은 위 띠 아래
    fn list_top(&self, s: Sheet) -> f32 {
        self.head_h(s) + self.px(lay::PAD)
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
        rect(r - self.px(lay::CARD_BTN_W), cy - self.px(15.0), r, cy + self.px(15.0))
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
    /// 스크롤·키 이동·마우스 판정이 씀, 높이와 그리기는 그 시트의 아래 칸으로 `page_above`를 바로 부름
    fn page(&self) -> usize {
        page_above(self.foot_of(self.sheet_now()).as_ref())
    }

    fn visible_rows(&self) -> usize {
        self.rows().len().min(self.page())
    }

    /// 지금 보기의 패널 높이, 보여 줄 게 없으면 0 (줄이 없으면 문구 한 줄)
    fn panel_target(&self) -> f32 {
        match self.sheet_now() {
            Sheet::Settings => self.settings_height(),
            // 줄이 넘치면 판 끝까지, 모두 들어가면 마지막 줄 아래 8을 남김
            Sheet::Exclude => self.px(lay::EXCLUDE_TOP) + self.px(lay::PAD) + self.exclude_list_h() + self.exclude_bottom_pad(),
            Sheet::Keywords => self.keyword_layout().height,
            Sheet::Card => self.px(lay::CARD_H),
            Sheet::Results if self.playlist_waiting() => 0.0,
            _ if !self.listing() => 0.0,
            s => {
                // 플레이리스트 결과는 줄 목록 밑에 아래 칸이 붙음, 고른 줄이 바뀌면 높이도 따라감
                // 히스토리·뽑은 기록은 줄 목록 위에 띠
                let foot = self.foot_of(s);
                let n = self.rows().len().min(page_above(foot.as_ref()));
                self.head_h(s) + self.px(lay::PAD + lay::ROW_H * n.max(1) as f32 + foot.map_or(lay::PAD, |f| f.height()))
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
            return if x < self.text_right() { Hit::Text } else { Hit::Inside };
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
        if self.view == View::Exclude {
            if inside(&self.exclude_done(), x, y) {
                return Hit::DoneExclude;
            }
            let r = self.exclude_tabs_rect();
            if inside(&r, x, y) {
                return Hit::ExcludeTab(ExcludeTab::ALL[segment_at(&r, x, ExcludeTab::ALL.len())]);
            }
            return self.exclude_at(x, y).map_or(Hit::Inside, |i| {
                if self.exclude_rows[i].kind.song().is_some() && inside(&self.exclude_control_rect(i), x, y) {
                    Hit::ExcludeButton(i)
                } else {
                    Hit::ExcludeRow(i)
                }
            });
        }
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
        // 기록이 없으면 초기화는 흐리게 보이기만 함, 줄 목록은 아래 결과 줄 판정
        if self.view == View::Rolls {
            let rl = self.roll_layout();
            if self.can_clear_rolls() && inside(&rl.clear, x, y) {
                return Hit::ClearRolls;
            }
            if inside(&rl.done, x, y) {
                return Hit::DoneRolls;
            }
        }
        if self.view == View::Settings {
            // 더 긴 시트에서 판이 줄어드는 동안에도 그림은 보이는 칸까지만이라 그 아래는 안 눌림
            if y >= self.panel_top() + self.settings_height() {
                return Hit::Inside;
            }
            let sl = self.settings_layout();
            if inside(&sl.mode, x, y) {
                return Hit::ModeSeg(segment_at(&sl.mode, x, Mode::ALL.len()));
            }
            if inside(&sl.diff, x, y) {
                return Hit::DiffSeg(segment_at(&sl.diff, x, Diff::ALL.len()));
            }
            if self.owned.is_some() && inside(&sl.exclude, x, y) {
                return Hit::Exclude;
            }
            if inside(&sl.rescan, x, y) {
                return Hit::Rescan;
            }
            if inside(&sl.hotkey, x, y) {
                return Hit::Hotkey;
            }
            // 토글은 작아서 둘레를 조금 더, 위아래는 버튼 줄만큼
            for (k, toggle) in [(sl.keep, Toggle::KeepQuery), (sl.dlc, Toggle::ShowDlc), (sl.glass, Toggle::Glass)] {
                if toggle == Toggle::Glass && self.glass_unavailable().is_some() {
                    continue;
                }
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
        if self.view == View::History {
            let r = self.tabs_rect();
            if inside(&r, x, y) {
                return Hit::Tab(HistoryTab::ALL[segment_at(&r, x, HistoryTab::ALL.len())]);
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
    /// 무작위 줄이면 뽑은 기록 시트를 엶
    fn open_keywords(&mut self, i: usize) -> bool {
        if !matches!(self.sheet_now(), Sheet::Results | Sheet::History) || !self.listing() {
            return false;
        }
        let Some(&row) = self.rows().get(i) else { return false };
        if row == Row::Random {
            self.open_rolls();
            return true;
        }
        let Some(song) = row.song() else { return false };
        self.finish_input();
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

    /// 무작위 줄의 뽑은 기록 시트를 엶, 열기 전 입력·검색어는 보관하고 입력 칸으로 나온 곡을 찾음
    fn open_rolls(&mut self) {
        self.finish_input();
        self.roll_edit = Some(RollEdit {
            from: self.view,
            scope: self.rows().to_vec(),
            saved: SavedQuery {
                edit: std::mem::take(&mut self.edit),
                searched: std::mem::take(&mut self.searched),
                row: Some((Row::Random, self.scroll)),
            },
            rows: Vec::new(),
            total: 0,
        });
        self.clear_input();
        self.view = View::Rolls;
        self.fill_rolls();
        self.retarget_panel();
        self.hover = Hit::None;
    }

    /// 뽑은 기록 줄: 연 곳 목록에서 나온 곡, 최근 순
    /// 입력한 글이 있으면 그중 맞는 줄만
    fn fill_rolls(&mut self) {
        let Some(r) = &self.roll_edit else { return };
        let mut rows = self.rolls.recent(&r.scope);
        let total = rows.len();
        let q = self.query();
        if !q.trim().is_empty() {
            let ids: HashSet<_> = rows.iter().filter_map(|r| r.song()).map(|i| self.catalog.songs[i].id).collect();
            let hits: HashSet<_> = self.find_in(&q, Some(&ids)).into_iter().collect();
            let songs: HashSet<_> = hits.iter().filter_map(|r| r.song()).collect();
            // 이름 검색은 같은 곡의 기록을 남기고, 패턴 조건은 나온 패턴에만 적용함
            rows.retain(|r| match *r {
                Row::Song(i) => songs.contains(&i),
                Row::Chart(i, ..) => hits.contains(r) || hits.contains(&Row::Song(i)),
                _ => false,
            });
        }
        let r = self.roll_edit.as_mut().unwrap();
        r.rows = rows;
        r.total = total;
        self.reset_rows();
    }

    fn roll_rows(&self) -> &[Row] {
        self.roll_edit.as_ref().map_or(&[], |r| &r.rows)
    }

    /// 검색에 맞는 줄 수와 검색 전 기록 수
    fn roll_counts(&self) -> (usize, usize) {
        self.roll_edit.as_ref().map_or((0, 0), |r| (r.rows.len(), r.total))
    }

    /// 라벨 줄 오른쪽 곡 수, 찾는 중이면 맞는 수도
    /// 기록이 없으면 찾는 중이어도 "0곡"
    fn roll_count_text(&self) -> String {
        let (n, total) = self.roll_counts();
        if self.searched.trim().is_empty() || total == 0 { format!("{total}곡") } else { format!("{n} / {total}곡") }
    }

    /// 시트 맨 위 무작위 줄의 아랫줄, 연 곳 목록의 무작위 줄과 같은 문구
    fn roll_note(&self) -> String {
        let Some(r) = &self.roll_edit else { return String::new() };
        let note = self.playlist_random_note(&r.scope).filter(|_| r.from == View::Results);
        note.unwrap_or_else(|| self.rolls.note(&r.scope))
    }

    fn roll_empty_text(&self) -> Option<&'static str> {
        match self.roll_counts() {
            (_, 0) => Some("아직 나온 곡이 없어요"),
            (0, _) => Some("나온 곡 중에 맞는 곡이 없어요"),
            _ => None,
        }
    }

    fn can_clear_rolls(&self) -> bool {
        self.roll_counts().1 > 0
    }

    /// 연 곳 목록에서 나온 기록을 지움, 찾는 글로 좁혀 안 보이는 것까지
    fn clear_rolls(&mut self) {
        if !self.can_clear_rolls() {
            return;
        }
        self.rolls.reset(&self.roll_edit.as_ref().unwrap().scope);
        self.fill_rolls();
        self.retarget_panel();
    }

    /// 열기 전 입력·검색어와 보기로 되돌리고 무작위 줄 자리를 돌려줌, 찾던 글은 버림
    /// 검색창을 닫거나 다시 열 때, 곡 목록이 바뀔 때도 불림, 찾던 글이 검색어로 남지 않게
    fn restore_roll_query(&mut self) -> Option<(Row, usize)> {
        let r = self.roll_edit.take()?;
        let row = self.restore_saved_query(r.saved);
        self.view = r.from;
        row
    }

    /// 원래 보기로 돌아가 무작위 줄을 고름
    /// 시트를 열면 기다리던 검색이 꺼져서 친 글을 아직 안 찾았으면 지금 찾음
    fn close_rolls(&mut self) {
        if self.roll_edit.is_none() {
            return;
        }
        // 찾던 글의 조합 중인 글자는 원래 검색어에 안 붙게 버림
        self.preedit.clear();
        self.notify_composition(CPS_CANCEL);
        let Some((row, scroll)) = self.restore_roll_query() else { return };
        // 시트가 열린 동안 플레이리스트 응답으로 히스토리가 바뀔 수 있음
        if self.view == View::History {
            self.fill_history();
        } else {
            self.search_now();
        }
        self.retarget_panel();
        self.restore_row(row, scroll);
        self.hover = Hit::None;
    }

    /// 키워드 시트의 곡 (키워드는 `user_keys`)
    fn keyword_song(&self) -> Option<&Song> {
        self.keyword_edit.as_ref().map(|k| &self.catalog.songs[k.song])
    }

    /// 키워드 시트의 곡이 즐겨찾기인지
    fn sheet_favorite(&self) -> bool {
        self.keyword_song().is_some_and(|s| self.library.is_favorite(s.id))
    }

    /// 키워드 시트의 곡을 즐겨찾기에 넣거나 빼고 지금 상태를 돌려줌, 저장 못 하면 이유를 보여 주고 별은 그대로
    /// 별 모션은 지금 보이는 채움에서 새로 시작, 켤 때마다 점을 새로 뽑음
    fn toggle_sheet_favorite(&mut self) -> bool {
        let Some(id) = self.keyword_song().map(|s| s.id) else { return false };
        let on = match self.library.toggle_favorite(id) {
            Ok(on) => {
                // 다른 안내와 지우려고 고른 칩은 그대로 둠
                if self.keyword_note() == Some(playlist::FAVORITES_SAVE_FAILED) {
                    self.set_keyword_note(None);
                }
                on
            }
            Err(note) => {
                self.set_keyword_note(Some(note));
                return self.library.is_favorite(id);
            }
        };
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

    /// 지금 입력을 보관하기 전에 하던 입력을 끝냄
    /// 조합 중인 글자는 확정해 같이 보관하고, 끌기·누른 토글·기다리던 검색은 멈춤
    fn finish_input(&mut self) {
        self.complete_composition();
        self.end_drag();
        self.release_toggle();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
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
        // 히스토리 뒤에 숨은 원래 검색도 돌아갈 때 다시 찾음
        // 플레이리스트 결과는 키워드와 상관없어서 다시 받지 않음
        if let Some(h) = self.list_edit.as_mut().filter(|h| saved.is_ok() && playlist::id(h.edit.text()).is_none()) {
            h.searched.clear();
        }
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
        // 시트에서 키워드나 즐겨찾기를 바꿨을 수 있음
        if self.view == View::History {
            self.fill_history();
        } else {
            // 플레이리스트 결과는 키워드와 상관없어서 다시 받지 않음
            if playlist::id(&self.query()).is_none() {
                self.searched.clear();
            }
            self.search_now();
        }
        self.retarget_panel();
        self.restore_row(row, scroll);
        self.hover = Hit::None;
    }

    /// 키워드·뽑은 기록 시트를 여닫는 키와 시트 안의 키, 처리했으면 true
    /// 시트에서 입력 편집 키는 아래 공통 처리로 넘김
    fn keyword_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, alt: bool, shift: bool, repeat: bool) -> bool {
        // F2나 메뉴 키 (Apps, Shift+F10), 시트에선 F2만 닫음
        let open_key = !ctrl && !alt && ((!shift && matches!(vk, VK_F2 | VK_APPS)) || (shift && vk == VK_F10));
        if self.view == View::Rolls {
            if vk == VK_ESCAPE || (open_key && vk == VK_F2) {
                self.close_rolls();
                return true;
            }
            // 줄 이동·고르기도 공통 처리로 넘기고, 모드·난이도 단축키 (Alt)와 다른 여는 키만 막음
            return alt || open_key;
        }
        if self.view != View::Keywords {
            if open_key {
                self.open_keywords(self.selected);
            }
            return open_key;
        }
        let empty = !self.typed();
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
            // 시트에선 입력 편집만, 모드 바꾸기·곡 고르기는 막음
            return alt || matches!(vk, VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_APPS | VK_F10 | VK_F2);
        }
        true
    }

    /// 보여 줄 내용이 있으면 그 내용으로 바꾸고, 없으면 직전 내용 그대로 사라짐 (`retarget_panel`)
    /// 히스토리는 열 때마다 고른 탭으로 다시 채움, 키워드·뽑은 기록 시트는 닫고 넘어감
    /// 히스토리에선 입력 칸으로 그 탭을 찾고, 나가면 원래 입력·결과·고른 줄로 돌아감
    fn set_view(&mut self, v: View) {
        if self.view == View::Exclude && v != View::Exclude {
            self.release_exclude();
        }
        if self.view == View::Rolls {
            self.close_rolls();
        }
        if self.view == View::Keywords {
            self.close_keywords();
        }
        if self.view == v {
            return;
        }
        let from_list = self.view.searches_sheet();
        let restore = if from_list {
            // 찾던 글의 조합 중인 글자는 원래 검색어에 안 붙게 버림
            self.preedit.clear();
            self.notify_composition(CPS_CANCEL);
            self.restore_list_query()
        } else {
            None
        };
        if v.searches_sheet() {
            self.begin_list_search(restore);
        }
        // 설정에서 모은 휠 값이 다른 목록으로, 다른 목록 것이 설정으로 넘어가지 않게
        if (v == View::Settings || self.view == View::Settings) && self.settings_scroll_max() > 0.0 {
            self.wheel_acc = 0;
        }
        self.view = v;
        self.end_hotkey_wait();
        if v == View::Settings {
            let scrolled = self.settings_scroll_s.value != 0.0;
            self.settings_scroll_s.snap(0.0);
            self.settings_scrolling = false;
            // 내려 둔 채 나갔으면 커서 아래 컨트롤이 바뀌어서 호버를 다시 봄
            if scrolled {
                if let Some((x, y)) = self.last_mouse.filter(|_| self.tracking) {
                    self.hover = self.hit(x as f32, y as f32);
                }
            }
            // 히스토리를 다녀와도 포커스가 남음, 맨 위 칸에서 안 보이면 풂 (안 보이는 토글이 키로 바뀌지 않게)
            if self.kb_focus.is_some_and(|f| self.settings_group(f).1 > self.settings_height()) {
                self.kb_focus = None;
            }
        }
        if v == View::History {
            self.fill_history();
        } else if from_list {
            self.search_now();
        }
        if v == View::Results {
            self.kb_focus = None;
            // 키로 나간 경우: 커서가 떠날 때처럼 두 버튼이 들어감
            if !self.in_zone && self.leave_since.is_none() {
                self.leave_since = Some(Instant::now());
            }
        }
        self.retarget_panel();
        // 설정으로 가도 고르던 결과 줄이 남게 선택을 결과 목록 기준으로 되돌림
        // 히스토리 시트가 사라지기만 하면 강조가 탭 위로 튀지 않게 그대로 둠, 결과가 다시 나올 때 맞춤
        if from_list && !v.searches_sheet() && self.panel_target() > 0.0 {
            self.sel_of = Sheet::Results;
            self.reset_rows();
            if let Some((row, scroll)) = restore {
                self.restore_row(row, scroll);
            }
        }
        if v == View::Results && self.listing() {
            self.fit_scroll();
            self.scroll_s.snap(self.scroll_s.target);
            self.retarget_selection(true);
        }
    }

    /// 검색에서 뺀 팩 (앱이 불러오고 저장함)
    pub fn hidden_dlc(&self) -> &HashSet<String> {
        self.catalog.hidden_dlc()
    }

    /// 바뀌면 지금 결과·히스토리도 새 후보로 다시 만듦
    pub fn set_hidden_dlc(&mut self, hidden: HashSet<String>) {
        if self.catalog.hidden_dlc() == &hidden {
            return;
        }
        self.catalog.set_hidden_dlc(hidden);
        self.refresh_available();
    }

    pub fn hidden_songs(&self) -> &HashSet<u32> {
        self.catalog.hidden_songs()
    }

    pub fn set_hidden_songs(&mut self, hidden: HashSet<u32>) {
        if self.catalog.hidden_songs() == &hidden {
            return;
        }
        self.catalog.set_hidden_songs(hidden);
        self.refresh_available();
    }

    /// 검색어가 같아도 후보를 다시 계산함
    /// 플레이리스트는 다시 받지 않고 받아 둔 내용으로 줄만 다시 만듦
    fn refresh_available(&mut self) {
        let selected = self.rows().get(self.selected).copied();
        let scroll = self.scroll;
        let query = self.query();
        let result_query = self.list_edit.as_ref().map_or(query.as_str(), |h| h.edit.text());
        self.results = if result_query.trim().is_empty() {
            Vec::new()
        } else if playlist::id(result_query).is_some() {
            self.playlist_data().map(|data| self.playlist_rows(data)).unwrap_or_default()
        } else {
            self.find(result_query)
        };
        self.bpm_rows = self.catalog.parse_filter(result_query).is_some_and(|f| f.bpm.is_some());
        self.fill_history();
        if !self.view.searches_sheet() {
            self.searched = query;
        }
        self.reset_rows();
        if let Some(row) = selected.filter(|row| self.rows().contains(row)) {
            self.restore_row(row, scroll);
        }
        self.retarget_panel();
    }

    /// 고를 수 있는 곡이 든 팩, 첫 인식 전엔 없음
    fn owned_packs(&self) -> Vec<DlcPack> {
        self.owned.as_ref().map(|ids| self.catalog.dlc_packs(ids)).unwrap_or_default()
    }

    /// (지금 목록의 팩 수, 그중 꺼 둔 팩 수, 숨긴 곡 수), 찾는 글과 상관없이 전체 목록 기준
    /// 데이터에서 사라진 팩·곡은 설정에만 남기고 안내 숫자에선 뺌
    fn exclude_counts(&self) -> (usize, usize, usize) {
        let packs = self.owned_packs();
        let off = packs.iter().filter(|p| self.hidden_dlc().contains(&p.code)).count();
        let songs = self.catalog.songs.iter().filter(|s| self.hidden_songs().contains(&s.id)).count();
        (packs.len(), off, songs)
    }

    fn exclude_note(&self) -> String {
        let (packs, off, songs) = self.exclude_counts();
        if self.owned.is_none() || (off == 0 && songs == 0) {
            "DLC 구매나 곡 해금 후 다시 인식해 주세요".into()
        } else if off > 0 && off == packs {
            "DLC를 모두 검색에서 뺐어요".into()
        } else if off > 0 && songs > 0 {
            format!("DLC {off}개와 곡 {songs}개를 검색에서 뺐어요")
        } else if songs > 0 {
            format!("곡 {songs}개를 검색에서 뺐어요")
        } else {
            format!("DLC {off}개를 검색에서 뺐어요")
        }
    }

    fn exclude_summary(&self) -> String {
        let (total, off, songs) = self.exclude_counts();
        if self.exclude_tab == ExcludeTab::Songs {
            return if songs == 0 { "검색에서 뺀 곡이 없어요".into() } else { format!("곡 {songs}개를 검색에서 뺐어요") };
        }
        let on = total - off;
        if on == total {
            format!("{total}개 모두 검색해요")
        } else if on == 0 {
            format!("{total}개 모두 검색에서 뺐어요")
        } else {
            format!("{total}개 중 {on}개를 검색해요")
        }
    }

    fn exclude_empty_text(&self) -> Option<(&'static str, Option<&'static str>)> {
        if self.exclude_rows.iter().any(|r| r.removal.is_none()) {
            return None;
        }
        match (self.exclude_tab, self.exclude_filtered) {
            (ExcludeTab::Songs, false) => Some(("숨긴 곡이 없어요", Some("위 검색창에서 곡을 찾아 숨길 수 있어요"))),
            (ExcludeTab::Songs, true) => Some(("일치하는 곡이 없어요", None)),
            (ExcludeTab::Dlc, true) => Some(("일치하는 DLC가 없어요", None)),
            _ => None,
        }
    }

    /// 기다리던 검색을 끝내야 보관한 입력과 결과가 일치함
    fn open_exclude(&mut self) {
        if self.owned.is_none() || self.view == View::Exclude {
            return;
        }
        self.finish_input();
        self.search_now();
        self.set_view(View::Exclude);
        self.fill_exclude();
        self.hover = Hit::None;
    }

    /// 키로 열었으면 설정의 검색 제외 버튼에 포커스가 남음
    fn close_exclude(&mut self) {
        self.set_view(View::Settings);
        self.hover = Hit::None;
        self.wheel_acc = 0;
    }

    /// 검색 제외 시트에서 화살표로 고른 줄이 있으면 Space는 그 줄 토글, 없으면 찾는 글의 공백
    fn space_toggles_row(&self) -> bool {
        self.view == View::Exclude && self.exclude_focus.is_some()
    }

    /// 처리하지 않은 편집 키는 입력 칸의 공통 처리로 넘김
    fn exclude_key(&mut self, vk: VIRTUAL_KEY, repeat: bool) -> Option<Action> {
        if self.view != View::Exclude {
            return None;
        }
        if matches!(vk, VK_UP | VK_DOWN | VK_RETURN | VK_SPACE) {
            self.search_now();
        }
        if vk == VK_SPACE && !repeat {
            self.exclude_space = self.space_toggles_row();
        }
        Some(match vk {
            VK_ESCAPE => {
                self.close_exclude();
                Action::None
            }
            VK_UP | VK_DOWN => {
                self.move_exclude(if vk == VK_UP { -1 } else { 1 });
                Action::None
            }
            VK_RETURN | VK_SPACE if vk == VK_RETURN || self.space_toggles_row() || self.exclude_space => {
                if repeat {
                    Action::None
                } else if let Some(i) = self.exclude_focus {
                    self.toggle_exclude(i)
                } else {
                    self.move_exclude(0);
                    Action::None
                }
            }
            VK_PRIOR | VK_NEXT => Action::None,
            _ => return None,
        })
    }

    /// 두 칸 탭이 있는 보기 (히스토리, 검색 제외)에서 ←는 첫 칸, →는 둘째 칸으로, 탭이 없는 보기면 false
    fn arrow_tab(&mut self, right: bool) -> bool {
        match self.view {
            View::History => self.set_history_tab(HistoryTab::ALL[right as usize]),
            View::Exclude => self.set_exclude_tab(ExcludeTab::ALL[right as usize]),
            _ => return false,
        }
        true
    }

    /// 탭은 제자리, 요약과 줄 목록만 겹쳐 바꾸고 높이는 같이 움직임 (히스토리 탭과 같음)
    fn set_exclude_tab(&mut self, tab: ExcludeTab) {
        if self.exclude_tab == tab {
            return;
        }
        self.preedit.clear();
        self.notify_composition(CPS_CANCEL);
        self.clear_input();
        self.exclude_tab = tab;
        self.list_swap.swap();
        self.fill_exclude();
    }

    fn exclude_on(&self, kind: &ExcludeKind) -> bool {
        match kind {
            ExcludeKind::Dlc(pack) => !self.hidden_dlc().contains(&pack.code),
            ExcludeKind::Song(i) => !self.hidden_songs().contains(&self.catalog.songs[*i].id),
        }
    }

    fn fill_exclude(&mut self) {
        self.release_exclude();
        let query = self.query();
        let q = query.trim();
        self.exclude_filtered = !q.is_empty();
        let kinds: Vec<_> = match self.exclude_tab {
            ExcludeTab::Dlc => {
                let q = q.to_lowercase();
                self.owned_packs().into_iter().filter(|p| p.name.to_lowercase().contains(&q)).map(ExcludeKind::Dlc).collect()
            }
            ExcludeTab::Songs => {
                let songs = if q.is_empty() {
                    let mut songs: Vec<_> = self.catalog.songs.iter().enumerate()
                        .filter(|(_, s)| self.hidden_songs().contains(&s.id)).map(|(i, _)| i).collect();
                    songs.sort_by_cached_key(|&i| self.catalog.songs[i].name.to_lowercase());
                    songs
                } else {
                    self.catalog.search_owned(q, LIMIT, self.owned.as_ref())
                };
                songs.into_iter().map(ExcludeKind::Song).collect()
            }
        };
        self.exclude_rows = kinds.into_iter().map(|kind| {
            let mut motion = ToggleMotion::new();
            motion.snap(self.exclude_on(&kind));
            ExcludeRow { kind, motion, removal: None }
        }).collect();
        self.searched = query;
        self.exclude_focus = None;
        self.exclude_space = false;
        self.exclude_scroll_s.snap(0.0);
        self.wheel_acc = 0;
        self.retarget_panel();
    }

    fn toggle_exclude(&mut self, i: usize) -> Action {
        // 입력이 아직 목록에 반영되지 않았으면 지난 줄을 바꾸지 않음
        if self.search_now() {
            return Action::None;
        }
        let Some(row) = self.exclude_rows.get(i).filter(|r| r.removal.is_none()) else { return Action::None };
        let on = match &row.kind {
            ExcludeKind::Dlc(pack) => self.catalog.toggle_dlc(&pack.code),
            ExcludeKind::Song(song) => {
                let id = self.catalog.songs[*song].id;
                self.catalog.toggle_song(id)
            }
        };
        let row = &mut self.exclude_rows[i];
        // 곡 줄은 버튼이라 노브 없이 글 밝기만 바뀜
        if matches!(row.kind, ExcludeKind::Song(_)) {
            row.motion.tint.go(on as u8 as f32, QUICK_SECS, QUICK_EASE);
        } else {
            row.motion.go(on);
        }
        self.fit_exclude_scroll(i);
        if self.exclude_tab == ExcludeTab::Songs && on && !self.exclude_filtered {
            if self.exclude_down == Some(i) {
                self.release_exclude();
            }
            let collapse = self.exclude_rows.iter().filter(|r| r.removal.is_none()).count() > 1;
            self.exclude_rows[i].removal = Some(ExcludeRemoval { elapsed: 0.0, collapse });
            if self.exclude_focus == Some(i) {
                self.exclude_focus = (i + 1..self.exclude_rows.len()).find(|&j| self.exclude_rows[j].removal.is_none())
                    .or_else(|| (0..i).rev().find(|&j| self.exclude_rows[j].removal.is_none()));
            }
            self.hover = Hit::Inside;
        }
        self.refresh_available();
        Action::Choice
    }

    /// 설정 포커스 이동, 처음 누른 화살표는 첫 컨트롤로, 첫 인식 전엔 검색 제외 버튼을 건너뜀
    /// 유리 효과를 못 쓰면 그 토글엔 안 감
    fn move_settings(&mut self, d: i32) {
        let mut next = self.kb_focus.map_or(FOCUS_MODE, |i| (i as i32 + d).clamp(FOCUS_MODE as i32, FOCUS_GLASS as i32) as usize);
        if next == FOCUS_EXCLUDE && self.owned.is_none() {
            next = if d > 0 { FOCUS_RESCAN } else { FOCUS_DIFF };
        }
        if next == FOCUS_GLASS && self.glass_unavailable().is_some() {
            next = FOCUS_DLC;
        }
        self.focus_settings(next);
    }

    /// 포커스 묶음이 보이는 칸을 벗어나면 위아래 16 여유를 두고 따라감
    fn focus_settings(&mut self, focus: usize) {
        self.kb_focus = Some(focus);
        let (top, bottom) = self.settings_group(focus);
        let (offset, height) = (self.settings_scroll_s.target, self.settings_height());
        if top < offset {
            self.set_settings_scroll(top - self.px(16.0));
        } else if bottom > offset + height {
            self.set_settings_scroll(bottom + self.px(16.0) - height);
        }
    }

    /// 정수 px에서 멈춰야 글자가 선명함
    fn set_settings_scroll(&mut self, offset: f32) {
        self.settings_scroll_s.target = offset.round().clamp(0.0, self.settings_scroll_max());
    }

    fn scroll_settings(&mut self, rows: i32) {
        self.set_settings_scroll(self.settings_scroll_s.target + self.px(lay::SETTINGS_WHEEL) * rows as f32);
    }

    /// 멈추면 목표 정수 px에 딱 맞춤, 멈춘 프레임까지 움직인 걸로 쳐서 마지막 자리에서도 호버를 다시 봄
    fn step_settings_scroll(&mut self, dt: f32) {
        let moving = !self.settings_scroll_s.settled();
        self.settings_scroll_s.step(dt);
        if self.settings_scroll_s.settled() {
            self.settings_scroll_s.snap(self.settings_scroll_s.target);
        }
        self.settings_scrolling = moving;
    }

    /// 고른 줄이 보이게 스크롤
    fn move_exclude(&mut self, d: i32) {
        let active = |i: &usize| self.exclude_rows[*i].removal.is_none();
        let next = match self.exclude_focus {
            Some(i) if d > 0 => (i + 1..self.exclude_rows.len()).find(active).or(Some(i)),
            Some(i) if d < 0 => (0..i).rev().find(active).or(Some(i)),
            Some(i) => Some(i),
            None => (0..self.exclude_rows.len()).find(active),
        };
        let Some(i) = next else { return };
        self.exclude_focus = Some(i);
        self.hover = Hit::Inside;
        self.fit_exclude_scroll(i);
    }

    fn exclude_scroll_max(&self) -> f32 {
        (self.exclude_content_h() - self.exclude_list_h()).max(0.0).round()
    }

    fn set_exclude_scroll(&mut self, offset: f32) {
        self.exclude_scroll_s.target = offset.round().clamp(0.0, self.exclude_scroll_max());
    }

    fn scroll_exclude(&mut self, d: i32) {
        self.set_exclude_scroll(self.exclude_scroll_s.target + self.px(self.exclude_tab.row_height()) * d as f32);
    }

    fn fit_exclude_scroll(&mut self, i: usize) {
        let top = self.exclude_row_top(i);
        let bottom = top + self.exclude_row_h(i);
        let offset = self.exclude_scroll_s.target;
        let height = self.exclude_list_h();
        if top < offset {
            self.set_exclude_scroll(top);
        } else if bottom > offset + height {
            self.set_exclude_scroll(bottom - height);
        }
    }

    fn step_exclude_rows(&mut self, dt: f32) {
        if !self.exclude_rows.iter().any(|r| r.removal.is_some()) {
            return;
        }
        let before = self.panel_target();
        for row in &mut self.exclude_rows {
            if let Some(removal) = &mut row.removal {
                removal.elapsed += dt;
            }
        }
        for i in (0..self.exclude_rows.len()).rev() {
            if self.exclude_rows[i].removal.as_ref().is_some_and(ExcludeRemoval::done) {
                self.exclude_rows.remove(i);
                let shifted = |at: Option<usize>| at.filter(|&j| j != i).map(|j| j - usize::from(j > i));
                self.exclude_focus = shifted(self.exclude_focus);
                self.exclude_down = shifted(self.exclude_down);
                self.hover = match self.hover {
                    Hit::ExcludeRow(j) => shifted(Some(j)).map_or(Hit::Inside, Hit::ExcludeRow),
                    Hit::ExcludeButton(j) => shifted(Some(j)).map_or(Hit::Inside, Hit::ExcludeButton),
                    hit => hit,
                };
            }
        }
        self.set_exclude_scroll(self.exclude_scroll_s.target);
        self.exclude_scroll_s.value = self.exclude_scroll_s.value.min(self.exclude_scroll_max());
        if let Some(i) = self.exclude_focus {
            self.fit_exclude_scroll(i);
        }
        if self.view == View::Exclude && self.sheet == Sheet::Exclude {
            let target = self.panel_target();
            // 줄이 접힌 만큼 판도 같은 프레임에 줄여 빈 공간이 뒤늦게 따라오지 않게 함
            self.panel_h.value += target - before;
            self.panel_h.target = target;
        }
    }

    /// 내용이 스크롤로 움직이면 커서 아래를 다시 호버
    /// 검색 제외 목록은 화살표로 고른 동안 그대로, 설정은 스크롤한 뒤에만 (시트가 나타날 땐 커서를 움직여야 호버)
    fn refresh_scroll_hover(&mut self) {
        let follow = match self.view {
            View::Exclude => self.exclude_focus.is_none(),
            View::Settings => self.settings_scrolling,
            _ => false,
        };
        if let Some((x, y)) = self.last_mouse.filter(|_| follow && self.tracking) {
            self.hover = self.hit(x as f32, y as f32);
        }
    }

    fn release_exclude(&mut self) {
        if let Some(i) = self.exclude_down.take() {
            if matches!(self.exclude_rows[i].kind, ExcludeKind::Dlc(_)) {
                self.exclude_rows[i].motion.press.go(1.0, QUICK_SECS, QUICK_EASE);
            }
            unsafe { let _ = ReleaseCapture(); }
        }
    }

    /// 완료 버튼, 다른 시트와 같은 자리 (첫 줄 띠 가운데)
    fn exclude_done(&self) -> D2D_RECT_F {
        self.done_button()
    }

    /// 탭 (세그먼트): 히스토리 탭처럼 양 끝이 줄 강조와 같음, 첫 줄 띠 아래 8에 높이 32, 바로 아래부터 줄 목록
    fn exclude_tabs_rect(&self) -> D2D_RECT_F {
        let top = self.panel_top();
        rect(self.px(lay::MARGIN) + self.px(lay::PAD), top + self.px(lay::EXCLUDE_TOP - 32.0),
            self.px(lay::MARGIN + lay::W) - self.px(lay::PAD), top + self.px(lay::EXCLUDE_TOP))
    }

    /// 스크롤되는 목록은 판 끝에서 잘려야 안쪽에 빈 선이 생기지 않음
    fn exclude_clip(&self) -> D2D_RECT_F {
        let top = self.panel_top() + self.px(lay::EXCLUDE_TOP) + self.px(lay::PAD);
        let bottom = self.panel_top() + self.panel_h.value;
        rect(self.px(lay::MARGIN) + self.px(lay::PAD), top, self.px(lay::MARGIN + lay::W) - self.px(lay::PAD),
            if self.exclude_scroll_max() > 0.0 { bottom } else { (top + self.exclude_list_h()).min(bottom) })
    }

    fn exclude_row_h(&self, i: usize) -> f32 {
        self.px(self.exclude_tab.row_height()) * self.exclude_rows[i].removal.as_ref().map_or(1.0, ExcludeRemoval::height)
    }

    fn exclude_row_top(&self, i: usize) -> f32 {
        (0..i).map(|j| self.exclude_row_h(j)).sum()
    }

    fn exclude_content_h(&self) -> f32 {
        if self.exclude_rows.iter().all(|r| r.removal.is_some()) {
            0.0
        } else {
            self.exclude_row_top(self.exclude_rows.len())
        }
    }

    fn exclude_bottom_pad(&self) -> f32 {
        (self.px(lay::EXCLUDE_LIST_H) - self.exclude_content_h()).clamp(0.0, self.px(lay::PAD))
    }

    /// 다 편 패널에서 줄이 차지하는 높이, 최대 높이를 넘으면 스크롤 (줄이 없으면 빈 목록 문구 띠)
    fn exclude_list_h(&self) -> f32 {
        let height = self.exclude_content_h();
        if height == 0.0 { self.px(lay::EXCLUDE_EMPTY_H) } else { height.min(self.px(lay::EXCLUDE_LIST_H)) }
    }

    /// 판정(`exclude_at`)·스크롤·패널 높이와 같은 식, 한꺼번에 반올림하면 4/3 배율에서 아래 줄로 몇 px씩 밀림
    fn exclude_row_rect(&self, i: usize) -> D2D_RECT_F {
        let clip = self.exclude_clip();
        let top = clip.top + self.exclude_row_top(i) - self.exclude_scroll_s.value;
        rect(clip.left, top, clip.right, top + self.exclude_row_h(i))
    }

    fn exclude_control_rect(&self, i: usize) -> D2D_RECT_F {
        let row = self.exclude_row_rect(i);
        let right = self.px(lay::MARGIN + lay::W - lay::INSET);
        let cy = (row.top + row.bottom) / 2.0;
        let (w, half) = if self.exclude_rows[i].kind.song().is_some() {
            (self.px(88.0), self.px(30.0) / 2.0)
        } else {
            (self.px(lay::TOGGLE_W), self.px(lay::TOGGLE_H / 2.0))
        };
        rect(right - w, cy - half, right, cy + half)
    }

    /// 빠지는 줄은 누른 버튼 글자 그대로 흐려짐
    fn exclude_button_label(&self, i: usize) -> &'static str {
        let row = &self.exclude_rows[i];
        if row.removal.is_none() && self.exclude_on(&row.kind) { "숨기기" } else { "숨김 해제" }
    }

    fn exclude_empty_alpha(&self) -> f32 {
        1.0 - self.exclude_rows.iter().filter_map(|r| r.removal.as_ref()).map(ExcludeRemoval::alpha).fold(0.0, f32::max)
    }

    fn exclude_bands(&self) -> impl Iterator<Item = (usize, D2D_RECT_F)> + '_ {
        let clip = self.exclude_clip();
        let mut offset = 0.0;
        (0..self.exclude_rows.len()).map(move |i| {
            let top = clip.top + offset - self.exclude_scroll_s.value;
            let height = self.exclude_row_h(i);
            offset += height;
            (i, rect(clip.left, top, clip.right, top + height))
        })
    }

    fn exclude_at(&self, x: f32, y: f32) -> Option<usize> {
        let clip = self.exclude_clip();
        // 아래 경계는 안 그린 다음 줄의 첫 픽셀이라 뺌
        if !inside(&clip, x, y) || y >= clip.bottom {
            return None;
        }
        self.exclude_bands().find_map(|(i, r)| {
            (self.exclude_rows[i].removal.is_none() && y >= r.top && y < r.bottom).then_some(i)
        })
    }

    /// 목록 자리에 걸치는 줄
    fn exclude_shown(&self) -> Vec<usize> {
        let clip = self.exclude_clip();
        self.exclude_bands().filter_map(|(i, r)| (r.bottom > clip.top && r.top < clip.bottom).then_some(i)).collect()
    }

    /// 머리: 제목·완료 버튼과 탭, 탭마다 바뀌는 요약과 줄 목록은 따로 그림 (`draw_exclude_list`)
    /// 16 정렬선의 글자는 설정 시트처럼 잉크를 맞춤, 설정과 겹쳐 바꿀 때 같은 "검색" 글자가 안 번짐
    fn draw_exclude_head(&self) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let done = self.exclude_done();
        let left = self.px(lay::MARGIN + lay::INSET);
        let title = g.layout("검색 제외", &f.title, done.left - left, self.px(19.0))?;
        unsafe {
            g.dc.DrawTextLayout(gfx::pt(left - BEARING, self.panel_top() + self.px(lay::PAD + 6.67)), &title, &g.brush(TEXT)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        draw_button(g, &f.item, done, "완료", self.hover == Hit::DoneExclude, self.scale)?;
        let at = self.seg[SEG_EXCLUDE].value();
        draw_segments(g, f, self.exclude_tabs_rect(), &["DLC", "숨긴 곡"], (at, at + 1.0), lay::PANEL_R - lay::PAD, self.scale)
    }

    /// 탭마다 바뀌는 요약과 줄 목록, 줄이 없으면 빈 목록 띠 가운데에 문구
    /// `shown`은 목록 자리에 걸치는 줄과 그 곡 재킷 (`exclude_shown`)
    fn draw_exclude_list(&self, shown: &[(usize, Option<ID2D1Bitmap1>)]) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let dc = &g.dc;
        let left = self.px(lay::MARGIN + lay::INSET);
        let summary = g.layout(&self.exclude_summary(), &f.sub, self.exclude_done().left - left - self.px(12.0), self.px(18.0))?;
        unsafe {
            dc.DrawTextLayout(gfx::pt(left - BEARING, self.panel_top() + self.px(lay::PAD + 26.67)), &summary, &g.brush(FAINT)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        let clip = self.exclude_clip();
        unsafe { dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED) };
        let result = (|| -> Result<()> {
            if self.exclude_empty_text().is_some() {
                unsafe { dc.PushLayer(&gfx::layer(self.exclude_empty_alpha(), None), None) };
                let result = self.draw_exclude_empty(clip);
                unsafe { dc.PopLayer() };
                result?;
            }
            for (i, jacket) in shown {
                let row = &self.exclude_rows[*i];
                if let Some(removal) = &row.removal {
                    if removal.alpha() <= 0.0 { continue; }
                    unsafe {
                        dc.PushAxisAlignedClip(&self.exclude_row_rect(*i), D2D1_ANTIALIAS_MODE_ALIASED);
                        dc.PushLayer(&gfx::layer(removal.alpha(), None), None);
                    }
                    let result = self.draw_exclude_row(*i, jacket.as_ref());
                    unsafe {
                        dc.PopLayer();
                        dc.PopAxisAlignedClip();
                    }
                    result?;
                } else {
                    self.draw_exclude_row(*i, jacket.as_ref())?;
                }
            }
            Ok(())
        })();
        unsafe { dc.PopAxisAlignedClip() };
        result
    }

    /// 빈 목록 띠 가운데에 문구, 아랫줄이 있으면 두 줄 묶음을 가운데에 두고 사이 6
    fn draw_exclude_empty(&self, clip: D2D_RECT_F) -> Result<()> {
        let Some((title, sub)) = self.exclude_empty_text() else { return Ok(()) };
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let (w, h) = (clip.right - clip.left, self.px(lay::EXCLUDE_EMPTY_H));
        let gap = self.px(6.0);
        let lines = [Some((title, &f.item, SUB)), sub.map(|s| (s, &f.sub, FAINT))];
        let mut placed = Vec::new();
        for (text, font, ink) in lines.into_iter().flatten() {
            let l = g.layout(text, font, w, h)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe {
                l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                l.GetMetrics(&mut m)?;
            }
            placed.push((l, m.height, ink));
        }
        let total = placed.iter().map(|(_, lh, _)| lh).sum::<f32>() + gap * (placed.len() - 1) as f32;
        let mut y = clip.top + ((h - total) / 2.0).round();
        for (l, lh, ink) in placed {
            unsafe { g.dc.DrawTextLayout(gfx::pt(clip.left, y), &l, &g.brush(ink)?, D2D1_DRAW_TEXT_OPTIONS_NONE) };
            y += lh + gap;
        }
        Ok(())
    }

    /// 켜진 줄은 결과 줄과 같은 색, 꺼 둔 줄은 글이 다 흐리고 곡 재킷은 반투명, 그 사이는 토글 색을 따라 섞음
    fn draw_exclude_row(&self, i: usize, jacket: Option<&ID2D1Bitmap1>) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let dc = &g.dc;
        let row = &self.exclude_rows[i];
        let r = self.exclude_row_rect(i);
        let lit = row.removal.is_none() && self.exclude_focus.map_or(matches!(self.hover, Hit::ExcludeRow(j) | Hit::ExcludeButton(j) if i == j), |focus| focus == i);
        let on = alpha_step(row.motion.tint.value());
        let ink = |c: D2D1_COLOR_F| g.brush(white(FAINT.a + (c.a - FAINT.a) * on));
        let (title_b, sub_b, faint) = (ink(TEXT)?, ink(SUB)?, g.brush(FAINT)?);
        let control = self.exclude_control_rect(i);
        if lit {
            unsafe { dc.FillRoundedRectangle(&rounded(r, self.px(12.0)), &g.brush(white(FILL_HOVER))?) };
        }
        match &row.kind {
            ExcludeKind::Dlc(pack) => {
                let left = self.px(lay::MARGIN + lay::INSET);
                let count = format!("{}곡", pack.count);
                let count_w = self.measure(&count, |f| &f.sub).ceil();
                let name_w = self.measure(&pack.name, |f| &f.item_bold).ceil()
                    .min((control.left - self.px(24.0) - count_w - left).max(0.0));
                let name = g.layout(&pack.name, &f.item_bold, name_w, r.bottom - r.top)?;
                let count = g.layout(&count, &f.sub, count_w, r.bottom - r.top)?;
                unsafe {
                    name.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                    count.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                    dc.DrawTextLayout(gfx::pt(left - BEARING, r.top), &name, &title_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    dc.DrawTextLayout(gfx::pt(left - BEARING + name_w + self.px(8.0), r.top), &count, &faint, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
            ExcludeKind::Song(s) => {
                let song = &self.catalog.songs[*s];
                if let Some(bmp) = jacket {
                    draw_jacket(g, bmp, &self.jacket_slot(r.top), 0.5 + 0.5 * on)?;
                }
                self.draw_row_text(r.top, control.left - self.px(16.0), &song.name, &song.composer, self.sub_tail(song, false).as_deref(), (&title_b, &sub_b))?;
            }
        }
        if row.kind.song().is_some() {
            let pressed = self.exclude_focus == Some(i) || self.exclude_down == Some(i) || self.hover == Hit::ExcludeButton(i);
            draw_button(g, &f.item, control, self.exclude_button_label(i), pressed, self.scale)
        } else {
            let m = &row.motion;
            draw_toggle(g, control, (m.knob.value(), m.tint.value(), m.press.value()), lit, self.scale)
        }
    }

    fn toggle_settings(&mut self) {
        self.set_view(if matches!(self.view, View::Settings | View::Exclude) { View::Results } else { View::Settings });
    }

    fn toggle_history(&mut self) {
        self.set_view(if self.view == View::History { View::Results } else { View::History });
    }

    /// 히스토리·검색 제외 시트 안에서 찾기 시작: 지금 입력·검색어·고른 결과 줄을 보관하고 입력 칸을 비움
    /// 조합 중인 글자는 원래 검색어에 넣어 둠
    fn begin_list_search(&mut self, restore: Option<(Row, usize)>) {
        self.finish_input();
        self.list_edit = Some(SavedQuery {
            edit: std::mem::take(&mut self.edit),
            searched: std::mem::take(&mut self.searched),
            // 다른 목록 시트로 넘어가도 처음 고른 결과 줄을 이어 감
            // 사본 없이 응답을 기다리는 플레이리스트면 응답 뒤에 고르려던 줄
            row: restore
                .or_else(|| self.playlist.as_ref().and_then(|p| p.restore))
                .or_else(|| self.results.get(self.selected).copied().map(|row| (row, self.scroll))),
        });
        self.clear_input();
    }

    /// 시트를 열기 전 입력·검색어·결과 선택으로 돌아감
    /// 검색창을 닫거나 다시 열 때도 불림, 찾던 글이 검색어로 남지 않게
    fn restore_list_query(&mut self) -> Option<(Row, usize)> {
        let h = self.list_edit.take()?;
        self.restore_saved_query(h)
    }

    /// 시트·히스토리에서 찾던 글을 다 버리고 원래 입력·검색어로 되돌림
    fn restore_queries(&mut self) {
        // 히스토리에서 연 시트가 보관한 건 히스토리에서 찾던 글이라 먼저 되돌려야 원래 검색어가 남음
        self.restore_roll_query();
        self.restore_keyword_query();
        self.restore_list_query();
    }

    /// 보관한 입력·검색어로 되돌리고 그때 고르던 줄을 돌려줌
    fn restore_saved_query(&mut self, saved: SavedQuery) -> Option<(Row, usize)> {
        self.clear_input();
        self.edit = saved.edit;
        self.seen = self.edit.text().to_owned();
        self.searched = saved.searched;
        saved.row
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

    /// 버튼이 눌린 채 보이는 보기, 히스토리에서 연 키워드·뽑은 기록 시트는 히스토리 안의 한 단계라 히스토리
    fn bead_view(&self) -> View {
        if self.view == View::Exclude {
            return View::Settings;
        }
        if let Some(r) = &self.roll_edit {
            return r.from;
        }
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
        if typing && self.view == View::Exclude {
            self.exclude_focus = None;
        }
        if typing && self.view == View::Settings {
            // 설정에서 입력하면 그 글로 결과 화면으로 돌아감
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
        // 히스토리에선 결과는 그대로 두고 그 탭 안에서만 찾음
        // 검색어를 지우면 사라지는 동안 지난 결과를 시트에 남겨 둠
        if self.view == View::Rolls {
            self.fill_rolls();
        } else if self.view == View::Exclude {
            self.fill_exclude();
        } else if self.view == View::History {
            self.fill_history();
        } else if !q.trim().is_empty() {
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
            .filter(|(_, e)| !self.hidden_songs().contains(&e.pattern.title))
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
        // 히스토리·검색 제외나 키워드·뽑은 기록 시트 뒤에 숨은 검색도 이 플레이리스트면 결과를 바꿔 둠, 돌아갈 때 고르던 줄을 다시 고름
        let query = self.list_edit.as_ref().map(|h| &h.edit)
            .or_else(|| self.keyword_edit.as_ref().map(|k| &k.edit))
            .or_else(|| self.roll_edit.as_ref().map(|r| &r.saved.edit))
            .map_or_else(|| self.query(), |edit| edit.text().to_owned());
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
        if matches!(self.view, View::Keywords | View::Rolls) {
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
        // 보관한 원래 입력은 버리고 이 플레이리스트 주소로 바꿈
        self.list_edit = None;
        self.clear_input();
        self.edit.insert(&playlist::url(&id));
        self.seen = self.query();
        self.view = View::Results;
        self.kb_focus = None;
        // 커서가 버튼 영역 밖이면 떠날 때처럼 두 버튼이 들어감 (`set_view`와 같음)
        if !self.in_zone && self.leave_since.is_none() {
            self.leave_since = Some(Instant::now());
        }
        self.searched.clear();
        self.search_now();
        if let Some(row) = self.pattern_row(last) {
            self.restore_row(row, 0);
        }
    }

    /// 보고 있는 플레이리스트의 받은 응답이나 사본
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
    fn playlist_random_note(&self, rows: &[Row]) -> Option<String> {
        self.playlist_data()?;
        let n = picks(rows).count();
        let m = self.rolls.choices(rows).0.count();
        let left = if m < n { format!(" ({m}곡 남음)") } else { String::new() };
        Some(format!("{n}곡 중 하나{left}"))
    }

    /// 그리는 시트의 레벨 칸에 모드까지 씀: 플레이리스트 결과와 뽑은 기록
    /// 둘 다 바에 모드가 안 보이고 줄마다 모드가 다를 수 있음
    /// 플레이리스트 결과는 아래 칸처럼 검색어를 지워 사라지는 동안에도 그대로
    fn mode_levels(&self) -> bool {
        match self.sheet {
            Sheet::Results => self.playlist_data().is_some(),
            Sheet::Rolls => true,
            _ => false,
        }
    }

    /// 줄 오른쪽 레벨 칸 너비 (DIP), 모드까지 쓰는 시트는 넓힘
    fn level_w(&self) -> f32 {
        if self.mode_levels() { lay::PLAYLIST_LEVEL_W } else { lay::LEVEL_W }
    }

    /// 줄 레벨 칸의 글과 서열표, 무작위 줄·히스토리의 플레이리스트 줄은 None
    /// 플레이리스트 결과·뽑은 기록은 모드까지 "4B · SC 15", 곡 줄은 설정 모드
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
        match (self.mode_levels(), level) {
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
            self.show_dlc.then(|| format!("  ·  {}", self.catalog.dlc_name(song)))
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
        self.find_in(q, self.owned.as_ref())
    }

    /// 찾을 곡을 `owned`로 정한 `find`, 히스토리에선 그 탭의 곡만 넘김
    fn find_in(&self, q: &str, owned: Option<&HashSet<u32>>) -> Vec<Row> {
        let Some(f) = self.catalog.parse_filter(q) else {
            // 특성 조건을 치다 만 글은 엉뚱한 곡 이름 결과 대신 빈 목록
            if self.catalog.has_tag_syntax(q) {
                return Vec::new();
            }
            return self.catalog.search(q, LIMIT, owned).into_iter().map(Row::Song).collect();
        };
        let mode = f.level.and_then(|l| l.mode).unwrap_or(self.mode_choice);
        let row = |(i, diff)| match diff {
            Some(d) => Row::Chart(i, mode, d),
            None => Row::Song(i),
        };
        let hits = self.catalog.filter(&f, mode, owned);
        let random = (!hits.is_empty()).then_some(Row::Random);
        random.into_iter().chain(hits.into_iter().map(row)).collect()
    }

    /// 히스토리 줄: 고른 탭에서 지금 목록에 있는 곡과 플레이리스트, 즐겨찾기는 곡이 있으면 맨 위에 무작위 줄
    /// 히스토리 보기에선 그중 입력한 글에 맞는 줄만
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
                let songs: Vec<Row> = self.library.favorites.iter().filter_map(|&id| self.catalog.pick(id, owned).map(Row::Song)).collect();
                let random = (!songs.is_empty()).then_some(Row::Random);
                random.into_iter().chain(songs).collect()
            }
        };
        self.history_query = if self.view == View::History { self.query() } else { String::new() };
        if !self.history_query.trim().is_empty() {
            self.history = self.find_in_history(&self.history_query);
        }
        if self.view == View::History {
            self.searched.clone_from(&self.history_query);
        }
        if self.sel_of == Sheet::History {
            self.reset_rows();
        }
    }

    /// 히스토리 줄 중 `q`에 맞는 줄: 곡은 결과 검색과 같은 규칙, 플레이리스트는 이름으로 찾아 곡 뒤에
    /// 조건 검색이면 곡만
    fn find_in_history(&self, q: &str) -> Vec<Row> {
        let ids: HashSet<u32> = self.history.iter().filter_map(|r| r.song()).map(|i| self.catalog.songs[i].id).collect();
        let mut rows = self.find_in(q, Some(&ids));
        if self.catalog.parse_filter(q).is_none() && !self.catalog.has_tag_syntax(q) {
            let q = dmpicker_search::normalize(q);
            // 사본이 없는 플레이리스트는 줄에 보이는 id로
            let named = |i: usize| {
                self.history_playlist(i).is_some_and(|(id, _, copy)| name_matches(copy.map_or(id, |p| p.name.as_str()), &q))
            };
            rows.extend((0..self.history.len()).filter(|&i| named(i)).map(|i| self.history[i]));
        }
        rows
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

    /// 결과 줄을 고를 수 있음: 검색 결과·히스토리·뽑은 기록, 검색어를 지운 뒤 사라지는 중인 줄은 아님
    fn listing(&self) -> bool {
        !matches!(self.view, View::Keywords | View::Exclude) && (!self.searched.trim().is_empty() || matches!(self.view, View::History | View::Rolls))
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
        // 플레이리스트 결과에서 연 뽑은 기록의 줄도 플레이리스트 항목으로 고름
        let from = self.roll_edit.as_ref().map_or(self.view, |r| r.from);
        if from == View::Results {
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

    /// 무작위 줄: 남은 줄 중 하나를 고르게 뽑음 (`Rolls`)
    fn roll(&mut self) -> Option<Row> {
        let rows = if self.view == View::History { &self.history } else { &self.results };
        self.rolls.draw(rows, self.rng.next())
    }

    fn toggle_value(&self, toggle: Toggle) -> bool {
        match toggle {
            Toggle::KeepQuery => self.keep_query,
            Toggle::ShowDlc => self.show_dlc,
            Toggle::Glass => self.glass_unavailable().is_none() && self.glass,
        }
    }

    /// 토글 값을 바꿈, 노브와 색이 따로 움직임
    fn set_toggle(&mut self, toggle: Toggle, on: bool) {
        match toggle {
            Toggle::KeepQuery => self.keep_query = on,
            Toggle::ShowDlc => self.show_dlc = on,
            Toggle::Glass if self.glass_unavailable().is_none() => self.glass = on,
            Toggle::Glass => return,
        }
        self.toggles[toggle as usize].go(on);
        self.glass_fade.go(self.glass_now() as u8 as f32, QUICK_SECS, QUICK_EASE);
    }

    /// 토글에서 손을 뗌, 노브가 원으로 돌아옴
    fn release_toggle(&mut self) {
        self.release_exclude();
        if let Some(toggle) = self.toggle_down.take() {
            self.toggles[toggle as usize].press.go(1.0, QUICK_SECS, QUICK_EASE);
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    /// 세그먼트마다 고른 칸: 모드, 난이도, 히스토리 탭, 검색 제외 탭
    fn segment_choices(&self) -> [usize; 4] {
        [self.mode_choice.index(), self.diff_choice.index(), self.history_tab as usize, self.exclude_tab as usize]
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
        let uses_mode = |q: &str| self.catalog.parse_filter(q).is_some_and(|f| f.uses_mode_setting());
        // 히스토리·검색 제외 시트 뒤에 숨은 원래 검색은 돌아갈 때 다시 찾음
        if let Some(h) = self.list_edit.as_mut().filter(|h| uses_mode(&h.searched)) {
            h.searched.clear();
        }
        if uses_mode(&self.searched) {
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

    fn input_hint(&self) -> &'static str {
        match self.view {
            View::Exclude => match self.exclude_tab {
                ExcludeTab::Dlc => "DLC 찾기",
                ExcludeTab::Songs => "숨길 곡 찾기",
            },
            View::Rolls => "나온 곡에서 찾기",
            View::Keywords => "추가할 키워드를 입력해 주세요",
            View::History if self.history_tab == HistoryTab::Favorites => "즐겨찾기에서 찾기",
            View::History => "최근 기록에서 찾기",
            _ => "곡 · 초성 · 작곡가 · 디맥지지 플리",
        }
    }

    /// 줄이 없을 때 목록 자리 문구
    fn empty_text(&self) -> Option<&'static str> {
        match self.sheet {
            Sheet::Rolls => self.roll_empty_text(),
            Sheet::History if !self.history_query.trim().is_empty() => Some(match self.history_tab {
                HistoryTab::Favorites => "즐겨찾기에 맞는 곡이 없어요",
                HistoryTab::Recent => "최근 기록에 맞는 곡이 없어요",
            }),
            // 기록이나 즐겨찾기가 있는데 이 목록에 하나도 없을 때도 따로 알림
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
        }
    }

    /// 두 버튼용 호버 머무름·떠남 시간 처리, 앞 버튼이 먼저 움직이고 다른 버튼은 시차 뒤에 따라감
    /// 설정·히스토리 보기 (히스토리에서 연 키워드·뽑은 기록 시트 포함)에선 늘 나옴
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
                if c == 0x20 && (self.space_toggles_row() || (self.view == View::Exclude && self.exclude_space)) {
                    return Some((0, Action::None));
                }
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
            WM_KEYUP if wp.0 == VK_SPACE.0 as usize && self.exclude_space => {
                self.exclude_space = false;
                Some((0, Action::None))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let mut vk = VIRTUAL_KEY(wp.0 as u16);
                if vk == VK_PROCESSKEY {
                    let real = unsafe { ImmGetVirtualKey(self.hwnd) };
                    vk = VIRTUAL_KEY(real as u16);
                    if matches!(vk, VK_RETURN | VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_DELETE | VK_ESCAPE | VK_PRIOR | VK_NEXT | VK_TAB)
                        || (vk == VK_SPACE && (self.space_toggles_row() || (self.view == View::Exclude && self.exclude_space))) {
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
                // 히스토리·검색 제외 탭은 왼쪽·오른쪽 방향키로 바꿈, 입력 중엔 캐럿을 옮기니 안 바꿈
                if !self.typed() && matches!(vk, VK_LEFT | VK_RIGHT) && self.arrow_tab(vk == VK_RIGHT) {
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
                if let Some(action) = self.exclude_key(vk, lp.0 & (1 << 30) != 0) {
                    return Some((0, action));
                }
                let act = if self.view == View::Settings { self.settings_key(vk) } else { self.list_key(vk, ctrl, shift) };
                Some((0, act))
            }
            WM_MOUSEMOVE => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                self.on_mouse_move(x, y);
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
                if matches!(self.view, View::Results | View::History | View::Rolls | View::Exclude)
                    || (self.view == View::Settings && self.settings_scroll_max() > 0.0)
                {
                    if d.signum() != self.wheel_acc.signum() {
                        self.wheel_acc = 0; // 방향 바뀜
                    }
                    self.wheel_acc += d;
                    let rows = self.wheel_acc / WHEEL_DELTA as i32;
                    self.wheel_acc -= rows * WHEEL_DELTA as i32;
                    if rows != 0 {
                        let mut p = POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 }; // 화면 좌표
                        let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut p) };
                        if self.view == View::Settings {
                            self.kb_focus = None;
                            self.last_mouse = Some((p.x, p.y));
                            self.scroll_settings(-rows);
                            return Some((0, Action::None));
                        }
                        if self.view == View::Exclude {
                            self.exclude_focus = None;
                            self.last_mouse = Some((p.x, p.y));
                            self.scroll_exclude(-rows);
                            self.refresh_scroll_hover();
                            return Some((0, Action::None));
                        }
                        self.search_now();
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
                Some((0, self.on_click(x, y)))
            }
            WM_LBUTTONUP if self.dragging => {
                self.end_drag();
                Some((0, Action::None))
            }
            WM_LBUTTONUP if self.exclude_down.is_some() => {
                let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
                let i = self.exclude_down.unwrap();
                let over = self.hit(x, y) == if self.exclude_tab == ExcludeTab::Songs { Hit::ExcludeButton(i) } else { Hit::ExcludeRow(i) };
                self.release_exclude();
                Some((0, if over { self.toggle_exclude(i) } else { Action::None }))
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
                if let Some(i) = self.exclude_down.take() {
                    self.exclude_rows[i].motion.press.go(1.0, QUICK_SECS, QUICK_EASE);
                }
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

    /// 설정 보기의 키, 포커스가 없으면 첫 컨트롤 기준
    fn settings_key(&mut self, vk: VIRTUAL_KEY) -> Action {
        let f = self.kb_focus.unwrap_or(FOCUS_MODE);
        match vk {
            VK_ESCAPE => {
                self.set_view(View::Results);
                Action::None
            }
            VK_UP => {
                self.move_settings(-1);
                Action::None
            }
            VK_DOWN => {
                // 처음 누른 화살표는 첫 컨트롤로
                self.move_settings(1);
                Action::None
            }
            VK_LEFT | VK_RIGHT => {
                let d = if vk == VK_LEFT { -1 } else { 1 };
                self.focus_settings(f);
                match f {
                    FOCUS_MODE => {
                        self.cycle_mode(d);
                        Action::Choice
                    }
                    FOCUS_DIFF => {
                        self.cycle_diff(d);
                        Action::Choice
                    }
                    // 토글은 왼쪽이 끔, 오른쪽이 켬
                    FOCUS_KEEP if (d > 0) != self.keep_query => {
                        self.set_toggle(Toggle::KeepQuery, d > 0);
                        Action::Choice
                    }
                    FOCUS_DLC if (d > 0) != self.show_dlc => {
                        self.set_toggle(Toggle::ShowDlc, d > 0);
                        Action::Choice
                    }
                    FOCUS_GLASS if self.glass_unavailable().is_none() && (d > 0) != self.glass => {
                        self.set_toggle(Toggle::Glass, d > 0);
                        Action::Choice
                    }
                    _ => Action::None,
                }
            }
            VK_RETURN if f == FOCUS_EXCLUDE => {
                self.open_exclude();
                Action::None
            }
            VK_RETURN if f == FOCUS_RESCAN => Action::Scan,
            VK_RETURN if f == FOCUS_HOTKEY => {
                self.start_hotkey_wait();
                Action::None
            }
            VK_RETURN if f == FOCUS_KEEP => {
                self.set_toggle(Toggle::KeepQuery, !self.keep_query);
                Action::Choice
            }
            VK_RETURN if f == FOCUS_DLC => {
                self.set_toggle(Toggle::ShowDlc, !self.show_dlc);
                Action::Choice
            }
            VK_RETURN if f == FOCUS_GLASS && self.glass_unavailable().is_none() => {
                self.set_toggle(Toggle::Glass, !self.glass);
                Action::Choice
            }
            VK_BACK => {
                self.edit.backspace(false);
                self.requery(Edit::Typed);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// 설정 밖 보기의 키: 줄 이동·고르기, 입력 칸 캐럿 이동과 지우기
    fn list_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> Action {
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
    }

    /// 끄는 중이면 선택을 넓히고, 아니면 호버와 두 버튼을 드러내는 영역, 줄 호버를 맞춤
    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.dragging {
            self.complete_composition(); // 위치는 조합 글자가 없는 글 기준
            self.edit.move_to(self.text_index(x), true);
            self.input_changed();
            return;
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
            return;
        }
        if self.view == View::Exclude {
            self.exclude_focus = None;
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
    }

    /// 왼쪽 버튼을 누른 자리의 동작
    fn on_click(&mut self, x: f32, y: f32) -> Action {
        let hit = self.hit(x, y);
        // 새 단축키를 기다리는 중 누르면 기다림을 끝냄, 단축키 버튼을 다시 누른 거면 그걸로 끝
        if self.hotkey_wait.is_some() {
            self.end_hotkey_wait();
            if hit == Hit::Hotkey {
                return Action::None;
            }
        }
        match hit {
            Hit::Text => {
                // 조합 중인 글자는 확정하고, Shift를 누른 채면 선택을 넓힘
                // 고른 줄은 풂, 커서를 안 움직이고 누르면 이동 메시지로는 안 풀려서 Space가 줄을 토글함
                self.exclude_focus = None;
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
            Hit::Exclude => {
                self.kb_focus = None;
                self.open_exclude();
                Action::None
            }
            Hit::DoneExclude => {
                self.close_exclude();
                Action::None
            }
            Hit::ExcludeTab(tab) => {
                self.set_exclude_tab(tab);
                Action::None
            }
            Hit::ExcludeRow(i) | Hit::ExcludeButton(i) if self.exclude_tab == ExcludeTab::Dlc || hit == Hit::ExcludeButton(i) => {
                self.exclude_focus = None;
                self.exclude_down = Some(i);
                // 곡 줄은 버튼이라 토글 노브 눌림이 없음
                if matches!(self.exclude_rows[i].kind, ExcludeKind::Dlc(_)) {
                    self.exclude_rows[i].motion.press.go(PRESS_Y, PRESS_SECS, PRESS_EASE);
                }
                unsafe { SetCapture(self.hwnd); }
                Action::None
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
            Hit::ClearRolls => {
                self.clear_rolls();
                Action::None
            }
            Hit::DoneRolls => {
                self.close_rolls();
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

    /// 위쪽이 `y`인 줄의 재킷 자리: 줄 강조 안쪽 8에 32 x 32로 세로 가운데, 모서리는 줄 강조와 동심
    fn jacket_slot(&self, y: f32) -> D2D1_ROUNDED_RECT {
        let size = self.px(32.0);
        let (x, top) = (self.px(lay::MARGIN) + self.px(lay::PAD) + self.px(8.0), y + (self.px(lay::ROW_H) - size) / 2.0);
        rrect(x, top, x + size, top + size, self.px(lay::PANEL_R - lay::PAD - 8.0))
    }

    /// 위쪽이 `y`인 줄의 제목과 아랫줄, 재킷 자리 오른쪽 10부터 `right`까지 (넘치면 말줄임)
    /// 결과·히스토리·뽑은 기록·키워드·검색 제외의 곡 줄과 무작위 줄이 같이 씀
    /// 레벨 칸이 맞추게 제목·아랫줄의 글자 기준선 y를 돌려줌
    fn draw_row_text(&self, y: f32, right: f32, title: &str, sub: &str, tail: Option<&str>, (title_b, sub_b): (&ID2D1SolidColorBrush, &ID2D1SolidColorBrush)) -> Result<(f32, f32)> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let x = self.jacket_slot(y).rect.right + self.px(10.0);
        let (title_y, sub_y) = (y + self.px(6.67), y + self.px(26.67));
        let tl = g.layout(title, &f.title, right - x, self.px(19.0))?;
        unsafe { g.dc.DrawTextLayout(gfx::pt(x, title_y), &tl, title_b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT) };
        let sl = self.draw_sub(x, sub_y, right - x, sub, tail, sub_b)?;
        Ok((title_y + baseline(&tl), sub_y + baseline(&sl)))
    }

    /// 위쪽이 `y`인 무작위로 고르기 줄: 재킷 자리에 셔플 아이콘, 제목과 아랫줄 `note`는 `right` 왼쪽 8까지
    /// 결과·히스토리 목록의 무작위 줄과 뽑은 기록 맨 위 줄이 같이 씀
    fn draw_random_row(&self, y: f32, right: f32, note: &str) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        self.draw_slot_icon(y, Icon::Random)?;
        self.draw_row_text(y, right - self.px(8.0), "무작위로 고르기", note, None, (&g.brush(TEXT)?, &g.brush(SUB)?))?;
        Ok(())
    }

    /// 재킷 자리에 곡 대신 아이콘: 옅은 채움 가운데에 글자색 아이콘 (무작위 줄, 히스토리의 플레이리스트 줄)
    fn draw_slot_icon(&self, y: f32, icon: Icon) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        let slot = self.jacket_slot(y);
        unsafe { g.dc.FillRoundedRectangle(&slot, &g.brush(white(FILL_HOVER))?) };
        let r = slot.rect;
        g.fill_icon(icon, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, self.px(lay::ICON), &g.brush(TEXT)?)
    }

    /// 키워드·뽑은 기록 시트의 라벨·빈 칸 문구·안내 문구는 줄 높이 안 가운데, 잉크가 16 정렬선에 맞음
    /// `l0`·`r0`은 패널 안쪽 16 정렬선
    fn text_line(&self, text: &str, font: &IDWriteTextFormat, top: f32, h: f32, b: &ID2D1SolidColorBrush, (l0, r0): (f32, f32)) -> Result<()> {
        let g = self.gfx.as_ref().unwrap();
        let l = g.layout(text, font, r0 - l0, h)?;
        unsafe {
            l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            g.dc.DrawTextLayout(gfx::pt(l0 - BEARING, top), &l, b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(())
    }

    /// 설정 시트 (다 편 시트 자리), `l0`·`r0`은 패널 안쪽 16 정렬선
    fn draw_settings(&self, (l0, r0): (f32, f32)) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let dc = &g.dc;
        let s = self.scale;
        let (text_b, sub_b) = (g.brush(TEXT)?, g.brush(SUB)?);
        let l0t = l0 - BEARING; // 글자 x, 잉크가 16 정렬선에 맞음
        let sl = self.settings_layout();
        unsafe {
            let t = g.layout("검색 설정", &f.title, self.px(200.0), self.px(19.0))?;
            dc.DrawTextLayout(gfx::pt(l0t, sl.title), &t, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            // 머리 줄 오른쪽: 곡 데이터 상태, 새 버전이 있으면 그 안내를 조금 밝게
            // 오른쪽 정렬선에 붙이고 제목 오른쪽 12 앞에서 말줄임, 글자 기준선은 제목과 같음
            if let Some((line, bright)) = &self.status_line {
                let left = l0t + self.measure("검색 설정", |f| &f.title) + self.px(12.0);
                let st = g.layout(line, &f.sub, (r0 - left).max(0.0), self.px(18.0))?;
                st.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                let sb = g.brush(if *bright { SUB } else { FAINT })?;
                dc.DrawTextLayout(gfx::pt(left, sl.title + baseline(&t) - baseline(&st)), &st, &sb, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            // 화살표로 바꾸는 컨트롤은 (한 번 쓴 뒤로) 테두리 없이 라벨 밝기로만 표시
            for (focus, label, y) in [(FOCUS_MODE, "버튼 모드", sl.labels[0]), (FOCUS_DIFF, "난이도", sl.labels[1])] {
                let ll = g.layout(label, &f.sub, self.px(200.0), self.px(18.0))?;
                let lb = if self.kb_focus == Some(focus) { &text_b } else { &sub_b };
                dc.DrawTextLayout(gfx::pt(l0t, y), &ll, lb, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            let thumb = |k: usize| {
                let at = self.seg[k].value();
                (at, at + 1.0)
            };
            draw_segments(g, f, sl.mode, &Mode::ALL.map(Mode::label), thumb(SEG_MODE), lay::SEG_R, s)?;
            draw_segments(g, f, sl.diff, &Diff::ALL.map(Diff::label), thumb(SEG_DIFF), lay::SEG_R, s)?;
            let note_b = g.brush(FAINT)?;
            let lt = g.layout("내 곡 목록", &f.item_bold, self.px(200.0), self.px(20.0))?;
            dc.DrawTextLayout(gfx::pt(l0t, sl.list_title), &lt, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            let label = if self.owned.is_some() { "다시 인식" } else { "목록 인식" };
            draw_button(g, &f.item, sl.rescan, label, matches!(self.hover, Hit::Rescan) || self.kb_focus == Some(FOCUS_RESCAN), s)?;
            if self.owned.is_some() {
                draw_button(g, &f.item, sl.exclude, "검색 제외", self.hover == Hit::Exclude || self.kb_focus == Some(FOCUS_EXCLUDE), s)?;
            } else {
                draw_button_off(g, &f.item, sl.exclude, "검색 제외", s)?;
            }
            let nl2 = g.layout(&self.exclude_note(), &f.sub, sl.exclude.left - self.px(12.0) - l0, self.px(18.0))?;
            dc.DrawTextLayout(gfx::pt(l0t, sl.list_note), &nl2, &note_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            // 단축키: 새 조합을 기다리는 동안 버튼이 눌린 채, 못 받으면 아랫줄에 잠깐 이유
            let ht = g.layout("검색창 단축키", &f.item_bold, self.px(200.0), self.px(20.0))?;
            dc.DrawTextLayout(gfx::pt(l0t, sl.hotkey_title), &ht, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            let waiting = self.hotkey_wait.is_some();
            draw_button(g, &f.item, sl.hotkey, &self.hotkey_text(), waiting || self.hover == Hit::Hotkey || self.kb_focus == Some(FOCUS_HOTKEY), s)?;
            let (note, nb) = match self.hotkey_note.filter(|(_, at)| at.elapsed().as_secs_f32() < HOTKEY_NOTE_SECS) {
                Some((why, _)) => (why, &text_b),
                None if waiting => ("Esc를 누르면 취소돼요", &note_b),
                None => ("선곡 화면에서 이 키로 검색창을 열어요", &note_b),
            };
            let nl3 = g.layout(note, &f.sub, sl.hotkey.left - self.px(12.0) - l0, self.px(18.0))?;
            dc.DrawTextLayout(gfx::pt(l0t, sl.hotkey_note), &nl3, nb, D2D1_DRAW_TEXT_OPTIONS_NONE);
            // 토글 줄은 같은 모양: 제목, 토글 왼쪽 12까지 설명, 버튼 줄 가운데 토글
            for (title, note, title_y, note_y, r, toggle, focus) in [
                ("검색어 유지", "다시 열면 마지막 검색어가 남아 있어요", sl.keep_title, sl.keep_note, sl.keep, Toggle::KeepQuery, FOCUS_KEEP),
                ("DLC 표시", "작곡가 옆에 DLC 이름을 표시해요", sl.dlc_title, sl.dlc_note, sl.dlc, Toggle::ShowDlc, FOCUS_DLC),
                ("유리 효과", self.glass_unavailable().unwrap_or("검색창 뒤로 게임 화면이 은은하게 비쳐요"), sl.glass_title, sl.glass_note, sl.glass, Toggle::Glass, FOCUS_GLASS),
            ] {
                let tl = g.layout(title, &f.item_bold, self.px(200.0), self.px(20.0))?;
                dc.DrawTextLayout(gfx::pt(l0t, title_y), &tl, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                let nl = g.layout(note, &f.sub, r.left - self.px(12.0) - l0, self.px(18.0))?;
                dc.DrawTextLayout(gfx::pt(l0t, note_y), &nl, &note_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                let m = &self.toggles[toggle as usize];
                if toggle == Toggle::Glass && self.glass_unavailable().is_some() {
                    draw_toggle_off(g, r, s)?;
                } else {
                    draw_toggle(g, r, (m.knob.value(), m.tint.value(), m.press.value()), self.hover == Hit::Toggle(toggle) || self.kb_focus == Some(focus), s)?;
                }
            }
        }
        Ok(())
    }

    /// 키워드 시트 (다 편 시트 자리): 곡 줄과 별·완료 버튼, 라벨, 칩, 안내 문구
    /// `jacket`은 곡 재킷, `l0`·`r0`은 패널 안쪽 16 정렬선
    fn draw_keywords(&self, song: &Song, jacket: Option<&ID2D1Bitmap1>, (l0, r0): (f32, f32)) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let dc = &g.dc;
        let s = self.scale;
        let (text_b, sub_b) = (g.brush(TEXT)?, g.brush(SUB)?);
        let icon_box = self.px(lay::ICON);
        let ct = self.panel_top();
        let kl = self.keyword_layout();
        unsafe {
            // 곡 줄: 결과 줄과 같은 재킷·제목·아랫줄 (선택 강조 없음), 레벨 칸 자리에 별과 완료 버튼
            let y = ct + self.px(lay::PAD);
            if let Some(bmp) = jacket {
                draw_jacket(g, bmp, &self.jacket_slot(y), 1.0)?;
            }
            self.draw_row_text(y, kl.fav.left - self.px(8.0), &song.name, &song.composer, self.sub_tail(song, false).as_deref(), (&text_b, &sub_b))?;
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

            let faint_b = g.brush(FAINT)?;
            self.text_line("내 키워드", &f.sub, kl.label, self.px(16.0), &sub_b, (l0, r0))?;
            if song.user_keys.is_empty() {
                self.text_line("아직 없어요", &f.item_regular, kl.chips_top, self.px(28.0), &faint_b, (l0, r0))?;
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
                self.text_line(note, &f.sub, kl.note, self.px(16.0), &sub_b, (l0, r0))?;
            }
        }
        Ok(())
    }

    /// 검색 바 입력 칸의 안내 글이나 입력한 글, 선택, 캐럿
    /// 캐럿이 칸 안에 오게 민 거리를 돌려줌, 부르는 쪽이 `text_scroll`에 넣음
    fn draw_input(&self, bar: D2D_RECT_F, now: Instant) -> Result<f32> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let dc = &g.dc;
        let s = self.scale;
        let text_b = g.brush(TEXT)?;
        let bar_h = bar.bottom - bar.top;
        let cy = (bar.top + bar.bottom) / 2.0;
        unsafe {
            let (tx, _) = self.text_origin();
            let tw = (self.text_right() - tx).max(10.0);
            let q = self.query();
            if q.is_empty() {
                let ph = g.brush(FAINT)?;
                let l = g.layout(self.input_hint(), &f.placeholder, tw, bar_h)?;
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
            let text_scroll = match self.text_return {
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
            let ox = tx - text_scroll;
            let field = rect(tx - self.px(2.0), bar.top, tx + tw, bar.bottom);
            dc.PushAxisAlignedClip(&field, D2D1_ANTIALIAS_MODE_ALIASED);
            // 글이 가려진 쪽 가장자리는 잘린 끝 대신 흐리게, 캐럿은 빼고
            let (fl, fr) = edge_fades((field.left, field.right), (ox, ox + end), (ox + text_x(&l, &q, c), ox + caret_x), self.px(lay::TEXT_FADE));
            // 레이어 브러시 참조, 있으면 레이어를 넣은 것
            // PopLayer 뒤 이 함수가 끝날 때 해제
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
            Ok(text_scroll)
        }
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
        self.step_exclude_rows(dt);
        self.exclude_scroll_s.step(dt);
        if self.exclude_scroll_s.settled() {
            self.exclude_scroll_s.snap(self.exclude_scroll_s.target);
        }
        self.step_settings_scroll(dt);
        self.refresh_scroll_hover();
        if self.scroll_s.settled() {
            self.scroll_s.snap(self.scroll_s.target); // 정수 픽셀에서 멈춰야 글자가 선명함
        }
        self.retarget_segments();
        self.retarget_panel();
        self.open_s.step(dt);
        self.reveal.iter_mut().for_each(|r| r.step(dt));
        self.panel_h.step(dt);
        self.rest_on_pixels();
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
        let listed = matches!(self.sheet, Sheet::Results | Sheet::History | Sheet::Rolls);
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
        // 검색 제외 목록 자리에 걸치는 줄과 곡 줄 재킷
        let exclude_ids: Vec<(usize, Option<u32>)> = if self.sheet == Sheet::Exclude {
            self.exclude_shown().into_iter().map(|i| (i, self.exclude_rows[i].kind.song().map(|s| self.catalog.songs[s].id))).collect()
        } else {
            Vec::new()
        };
        let exclude_shown: Vec<_> = exclude_ids.into_iter().map(|(i, id)| (i, id.and_then(|id| self.jacket(id)))).collect();

        let glass = self.glass_mix();
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

            // 2) 테두리 선의 빛: 바와 패널에 알림과 같은 그라데이션 하나씩 (`Gfx::rim_gradient`)
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

            // 줄 목록 (결과·히스토리·뽑은 기록): 빈 목록 문구나 선택 강조·줄·아래 칸
            let draw_list = || -> Result<()> {
                let row0 = self.list_top(self.sheet);
                if n_rows == 0 {
                    if let Some(empty) = self.empty_text() {
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
                let lvl_w = self.px(self.level_w());
                let lvl_r = panel.right - inset - self.px(8.0);
                // 줄은 줄 영역 안에서 미끄러짐 (멈추면 딱 보이는 줄만이라 잘림 없음)
                let top = ct + row0;
                // 히스토리·뽑은 기록은 그 안에서 찾은 글 기준
                let bpm = match self.sheet {
                    Sheet::History => self.catalog.parse_filter(&self.history_query).is_some_and(|f| f.bpm.is_some()),
                    Sheet::Rolls => self.catalog.parse_filter(&self.searched).is_some_and(|f| f.bpm.is_some()),
                    _ => self.bpm_rows,
                };
                dc.PushAxisAlignedClip(&rect(panel.left, top, panel.right, top + pitch * n_page as f32), D2D1_ANTIALIAS_MODE_ALIASED);
                for (k, &j) in shown.iter().enumerate() {
                    let row = rows[j];
                    let y = ct + (row0 + pitch * j as f32 - off);
                    // 제목, 아랫줄과 그 꼬리
                    let (title, sub, tail) = match row {
                        Row::Random => {
                            // 플레이리스트 문구는 결과 줄 기준이라 히스토리의 무작위 줄엔 안 씀
                            let note = self.playlist_random_note(rows).filter(|_| self.sheet == Sheet::Results);
                            self.draw_random_row(y, lvl_r - lvl_w, &note.unwrap_or_else(|| self.rolls.note(rows)))?;
                            continue;
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
                            let tail = self.sub_tail(song, bpm);
                            (Cow::Borrowed(song.name.as_str()), Cow::Borrowed(song.composer.as_str()), tail)
                        }
                    };
                    let slot = self.jacket_slot(y);
                    // 고를 수 없는 줄은 제자리에 흐리게
                    let dim = matches!(row, Row::Unavailable(_));
                    match row {
                        // 재킷 자리에 아이콘: 히스토리의 플레이리스트는 목록
                        Row::Playlist(_) => self.draw_slot_icon(y, Icon::Playlist)?,
                        _ => match &jackets[k] {
                            Some(bmp) => draw_jacket(g, bmp, &slot, if dim { DISABLED.a } else { 1.0 })?,
                            // 곡 데이터에 없는 곡은 빈 재킷 자리
                            None if dim && ids[k].is_none() => dc.FillRoundedRectangle(&slot, &g.brush(white(FILL_TRACK))?),
                            None => {}
                        },
                    }
                    let (title_b, line_b) = if dim { (&off_b, &off_b) } else { (&text_b, &sub_b) };
                    let (title_base, sub_base) = self.draw_row_text(y, lvl_r - lvl_w - self.px(8.0), &title, &sub, tail.as_deref(), (title_b, line_b))?;

                    // 레벨 칸: 레벨 "HD 11"은 곡 이름과, 서열표 "13.4"는 아랫줄과 같은 글자 기준선
                    // 그 모드·난이도 패턴이 없는 곡은 비우고 (플레이리스트·뽑은 기록은 채보만), 서열표가 없는 패턴은 아래만 비움
                    // 고를 수 없는 줄은 서열표도 흐리게
                    let Some((label, floor)) = self.level_text(row) else { continue };
                    let lx = lvl_r - lvl_w;
                    let ll = g.layout(&label, &f.item_bold, lvl_w, self.px(20.0))?;
                    ll.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                    dc.DrawTextLayout(gfx::pt(lx, title_base - baseline(&ll)), &ll, title_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    if let Some(fl) = floor {
                        let fl = g.layout(&format!("{fl:.1}"), &f.sub, lvl_w, self.px(16.0))?;
                        fl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                        let fb = if dim { &off_b } else { &floor_b };
                        dc.DrawTextLayout(gfx::pt(lx, sub_base - baseline(&fl)), &fl, fb, D2D1_DRAW_TEXT_OPTIONS_NONE);
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
            // 시트 내용 (다 편 시트 자리), 히스토리·검색 제외는 제자리 탭 아래에 겹쳐 바꾸는 줄 목록 `list`
            let draw_sheet = |list: Option<&ID2D1Image>| -> Result<()> {
                if self.sheet == Sheet::Settings {
                    self.draw_settings((l0, r0))?;
                } else if self.sheet == Sheet::Exclude {
                    self.draw_exclude_head()?;
                    if let Some(list) = list {
                        dc.DrawImage(list, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                    }
                } else if let (Sheet::Keywords, Some(song)) = (self.sheet, self.keyword_song()) {
                    self.draw_keywords(song, kw_jacket.as_ref(), (l0, r0))?;
                } else if self.sheet == Sheet::Rolls {
                    let rl = self.roll_layout();
                    // 무작위 줄: 결과의 무작위 줄과 같은 모양 (선택 강조 없음), 레벨 칸 자리에 초기화와 완료
                    self.draw_random_row(ct + self.px(lay::PAD), rl.clear.left, &self.roll_note())?;
                    if self.can_clear_rolls() {
                        draw_button(g, &f.item, rl.clear, "초기화", self.hover == Hit::ClearRolls, s)?;
                    } else {
                        draw_button_off(g, &f.item, rl.clear, "초기화", s)?;
                    }
                    draw_button(g, &f.item, rl.done, "완료", self.hover == Hit::DoneRolls, s)?;
                    // 라벨은 키워드 시트 라벨과 같은 줄, 곡 수는 같은 줄 오른쪽 16 정렬선에 한 단계 흐리게
                    self.text_line("이 목록에서 나온 곡", &f.sub, rl.label, self.px(16.0), &sub_b, (l0, r0))?;
                    let cl = g.layout(&self.roll_count_text(), &f.sub, r0 - l0, self.px(16.0))?;
                    cl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                    cl.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                    dc.DrawTextLayout(gfx::pt(l0, rl.label), &cl, &g.brush(FAINT)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    draw_list()?;
                } else if self.sheet == Sheet::History {
                    let at = self.seg[SEG_HISTORY].value();
                    draw_segments(g, f, self.tabs_rect(), &["최근", "즐겨찾기"], (at, at + 1.0), lay::PANEL_R - lay::PAD, s)?;
                    if let Some(list) = list {
                        dc.DrawImage(list, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                    }
                } else if self.sheet == Sheet::Card {
                    draw_card()?;
                } else if listed {
                    draw_list()?;
                }
                Ok(())
            };

            // 3) 패널 내용 기록: 바꾸는 중이면 옛 그림과 섞어서 7)에서 그리고, 다음 프레임에 보기·카드·탭이 바뀌면 이게 옛 그림이 됨 (`Crossfade`)
            // 히스토리·검색 제외 줄 목록은 탭만 바뀔 때 혼자 섞으려고 먼저 따로 기록
            if has_panel {
                let list = match self.sheet {
                    Sheet::History => Some(self.list_swap.compose(g, record(dc, draw_list)?)?),
                    Sheet::Exclude => Some(self.list_swap.compose(g, record(dc, || self.draw_exclude_list(&exclude_shown))?)?),
                    _ => None,
                };
                // 줄인 설정 시트는 보이는 칸만 기록, 더 긴 시트로 바뀌며 판이 커질 때 가려졌던 줄이 드러나지 않게
                let cut = (self.sheet == Sheet::Settings && self.settings_scroll_max() > 0.0).then(|| {
                    let top = self.panel_top();
                    rect(self.px(lay::MARGIN), top, self.px(lay::MARGIN + lay::W), top + self.settings_height())
                });
                sheet_drawn = Some(self.sheet_swap.compose(g, record(dc, || {
                    if let Some(c) = &cut {
                        dc.PushAxisAlignedClip(c, D2D1_ANTIALIAS_MODE_ALIASED);
                    }
                    let drawn = draw_sheet(list.as_ref().map(|(l, _)| l));
                    if cut.is_some() {
                        dc.PopAxisAlignedClip();
                    }
                    drawn
                })?)?);
                list_drawn = list;
            }

            // 4) 합성: 그림자, 판, 테두리 빛 순 (효과 출력은 이미 제자리라 변환 없이 그림)
            if fx.flood_glass.get() != glass {
                fx.flood_glass.set(glass);
                set_plate(&fx.flood, glass)?;
            }
            dc.SetTarget(&target);
            let d = Drawing::begin(dc, &IDENTITY);
            g.begin_content();
            dc.PushLayer(&gfx::layer(open_t, None), None);
            for img in [fx.shadow.output(glass)?, fx.plate.GetOutput()?, fx.rim_light.GetOutput()?] {
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
            // 플레이리스트는 항목마다 채보가 정해져 있어 안 보임 (`shows_cond`)
            if self.shows_cond() {
                let cr = self.cond_rect();
                let searching = matches!(self.view, View::Results | View::History);
                let cond = match self.catalog.parse_filter(&self.searched).filter(|_| searching) {
                    Some(Filter { level: Some(q), .. }) => format!("{} · {}", q.mode.unwrap_or(self.mode_choice).label(), q.diff.label()),
                    // 레벨 조건 없이 특성 조건이 있으면 난이도는 줄마다 달라서 모드만
                    Some(c) if !c.tags.is_empty() => self.mode_choice.label().to_owned(),
                    None if searching && self.catalog.has_tag_syntax(&self.searched) => self.mode_choice.label().to_owned(),
                    _ => format!("{} · {}", self.mode_choice.label(), self.diff_choice.label()),
                };
                let cl_ = g.layout_centered(&cond, &f.small, cr.right - cr.left, cr.bottom - cr.top)?;
                dc.DrawTextLayout(gfx::pt(cr.left, cr.top), &cl_, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }

            // 버튼이 다 들어가 멈추면 바를 따라 돌아오던 입력 칸 글은 원래대로
            if self.reveal.iter().all(Spring::settled) {
                self.text_return = None;
            }
            self.text_scroll = self.draw_input(bar, now)?;

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
            if let Some((drawn, _)) = &sheet_drawn {
                // 마스크는 PopLayer 뒤 이 블록이 끝날 때 해제
                let mask: ID2D1Geometry = g.factory.CreateRoundedRectangleGeometry(&rounded(panel, pr))?.cast()?;
                dc.PushLayer(&D2D1_LAYER_PARAMETERS1 { contentBounds: panel, ..gfx::layer(panel_a, Some(&mask)) }, None);
                // 내용은 다 편 시트 자리에서 시트와 같이 비켜남
                dc.SetTransform(&windows_numerics::Matrix3x2 { M32: shrink.M32 + k * sg.dy, ..shrink });
                dc.DrawImage(drawn, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                dc.SetTransform(&shrink);
                dc.PopLayer();
            }

            if self.closing {
                dc.PopLayer();
            }
            dc.PopLayer();
            g.end_content();

            // 8) 유리 효과 마스크: 판과 같은 모양·불투명도, 모양 마스크는 닫을 때 줄어드는 변환까지 들어간 창 좌표
            if glass > 0.0 {
                // 키는 1)의 모양 마스크를 정하는 값만, 안 그린 버튼은 반지름 0
                let bead = |i: usize| if self.reveal[i].value > 0.02 { beads[i].2 } else { 0.0 };
                let panel_key = if has_panel { [panel.left, panel.top, panel.right, panel.bottom, panel_a] } else { [0.0; 5] };
                let mut key = vec![bar.left, bar.top, bar.right, bar.bottom, beads[0].0, bead(0), beads[1].0, bead(1), open_t, k];
                key.extend(panel_key);
                g.paint_mask(&key, || {
                    dc.PushLayer(&gfx::layer(open_t, None), None);
                    dc.DrawImage(&fx.sheets.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                    dc.PopLayer();
                    Ok(())
                })?;
            }
            d.end()?;
        }
        if let Some(Err(e)) = g.scene().map(|sc| sc.set_glass(glass)) {
            eprintln!("glass: {e:?}");
        }
        if let Some(b) = &self.beams {
            // 목록 인식 카드 (첫 인식·다시 인식), 결과·히스토리에서 선택된 무작위로 고르기 줄 (시트 내용과 같이 비켜나고 바뀌면 새 내용과 같이 나타남)
            let content_a = panel_a * alpha_step(self.sheet_swap.shown.value());
            let scan_card = matches!(self.card(), Some(Card::Scan | Card::Stale));
            let card = (self.sheet == Sheet::Card && scan_card && has_panel).then_some((panel, content_a));
            let list_top = self.panel_top() + sg.dy + self.list_top(self.sheet);
            let list_bottom = (list_top + pitch * n_page as f32).min(panel.bottom);
            let random_sheet = matches!(self.sheet, Sheet::Results | Sheet::History);
            let random = (random_sheet && has_panel && listed && self.rows_of(self.sheet).get(self.selected) == Some(&Row::Random))
                .then(|| {
                    let inset = self.px(lay::PAD);
                    let y = list_top + pitch * self.selected as f32 - off;
                    // 히스토리 탭을 바꾸는 동안엔 새 줄 목록과 같이 섞여 나타남
                    let list_a = alpha_step(self.list_swap.shown.value());
                    (rect(panel.left + inset, y, panel.right - inset, y + pitch), content_a * list_a)
                })
                .filter(|(at, _)| at.top >= list_top - 0.5 && at.bottom <= list_bottom + 0.5);
            if let Err(e) = self.place_beams(b, &shrink, bar, open_t, card, random) {
                eprintln!("beams: {e:?}");
            }
        }
        self.sheet_swap.last = sheet_drawn.map(|(_, parts)| parts).unwrap_or_default();
        self.list_swap.last = list_drawn.map(|(_, parts)| parts).unwrap_or_default();
        self.sheet_swap.retire();
        self.list_swap.retire();
        let seen = self.schema_card_visible();
        self.moving = self.animating();
        let presented = self.gfx.as_ref().unwrap().present();
        self.presented(seen, presented)
    }

    /// 화면에 내보낸 프레임만 형식 안내 표시로 기록
    fn presented(&mut self, seen: bool, result: Result<()>) -> Result<()> {
        if seen && result.is_ok() {
            self.schema_seen = true;
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

/// 재킷을 둥근 자리 `slot`에 줄여 채움, 불투명도 `alpha`
fn draw_jacket(g: &Gfx, bmp: &ID2D1Bitmap1, slot: &D2D1_ROUNDED_RECT, alpha: f32) -> Result<()> {
    unsafe {
        let r = slot.rect;
        let bb = g.dc.CreateBitmapBrush(bmp, None, None)?;
        let sz = bmp.GetPixelSize();
        let sc = (r.right - r.left) / sz.width as f32;
        bb.SetTransform(&windows_numerics::Matrix3x2 { M11: sc, M12: 0.0, M21: 0.0, M22: sc, M31: r.left, M32: r.top });
        bb.SetInterpolationMode1(D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC);
        bb.SetOpacity(alpha);
        g.dc.FillRoundedRectangle(slot, &bb);
    }
    Ok(())
}

/// 패널 버튼: 둥근 채움 (`draw_button_fill`)과 가운데 글
fn draw_button(g: &Gfx, font: &IDWriteTextFormat, r: D2D_RECT_F, label: &str, lit: bool, s: f32) -> Result<()> {
    draw_button_fill(g, r, lit, s)?;
    unsafe {
        let l = g.layout_centered(label, font, r.right - r.left, r.bottom - r.top)?;
        g.dc.DrawTextLayout(gfx::pt(r.left, r.top), &l, &g.brush(TEXT)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}

/// 유리 효과 전환의 지금 값, 끝나면 목표 값 그대로 (곡선 끝값은 1에 아주 조금 못 미침)
pub(crate) fn glass_level(fade: &Tween) -> f32 {
    if fade.done() { fade.target() } else { fade.value() }
}

/// 판 색 (Flood), premultiplied라 진하기를 미리 곱함
fn set_plate(flood: &ID2D1Effect, glass: f32) -> Result<()> {
    gfx::set_floats(flood, D2D1_FLOOD_PROP_COLOR.0 as u32, &[TINT.r, TINT.g, TINT.b, 1.0].map(|c| c * gfx::plate_alpha(glass)))
}

/// 누를 수 없는 패널 버튼: 세그먼트 트랙만큼 옅은 채움에 고를 수 없는 줄 글자색, 호버 없음
fn draw_button_off(g: &Gfx, font: &IDWriteTextFormat, r: D2D_RECT_F, label: &str, s: f32) -> Result<()> {
    unsafe {
        g.dc.FillRoundedRectangle(&rounded(r, (8.0 * s).round()), &g.brush(white(FILL_TRACK))?);
        let l = g.layout_centered(label, font, r.right - r.left, r.bottom - r.top)?;
        g.dc.DrawTextLayout(gfx::pt(r.left, r.top), &l, &g.brush(DISABLED)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
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
/// `(at, tint, press)`는 (자리 0 끔 .. 1 켬, 넘칠 수 있음, 켬 색 비율, 노브 세로 배율)
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

/// 쓸 수 없는 토글: 누를 수 없는 버튼과 같은 색, 노브는 꺼진 자리
fn draw_toggle_off(g: &Gfx, r: D2D_RECT_F, s: f32) -> Result<()> {
    unsafe {
        let h = r.bottom - r.top;
        let d = (lay::KNOB * s).round();
        let pad = (h - d) / 2.0;
        g.dc.FillRoundedRectangle(&rounded(r, h / 2.0), &g.brush(white(FILL_TRACK))?);
        g.dc.FillEllipse(&circle(r.left + pad + d / 2.0, (r.top + r.bottom) / 2.0, d / 2.0), &g.brush(DISABLED)?);
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
