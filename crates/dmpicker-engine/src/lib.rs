//! `dmpicker-engine`: DJMAX RESPECT V 곡 검색기에서 화면 읽는 쪽
//!
//! 전부 RGB 프레임([`img::Rgb`]) 기준이고 해상도와 무관함
//! 게임 UI는 위아래 여백을 뺀 화면의 *높이*에 맞춰 커지므로 기준 수치는 모두 `H / 1440` 단위
//!
//! 처리 순서
//! 1. [`capture`]로 게임 클라이언트 영역 캡처 (PrintWindow, 전용 전체 화면이면 GDI 화면 캡처)
//!    16:9보다 세로로 길면 위아래 여백을 빼고 가운데 16:9 띠만 씀
//! 2. [`geom::find_list`]로 곡 목록 위치 자동 보정 (줄 색 막대 + 재킷 열)
//!    프리스타일 배치, 가운데 정렬된 온라인(OPEN MATCH) 배치 둘 다 됨
//!    스크롤 애니메이션이 끝났는지는 [`geom::at_rest`]로 확인
//! 3. [`read::read_window`]로 보이는 재킷 11개를 [`features::RefSet`]과 대조
//! 4. [`panel`]로 난이도 칸 읽기 (NORMAL / HARD / MAXIMUM / SC)
//! 5. [`state::classify`]로 지금 화면 판별, [`tabs`]로 ALL TRACK 탭 찾기, [`sort`]로 정렬 창 읽기
//! 6. [`input`]으로 scan code 키 입력, 게임이 포그라운드 창일 때만 보냄
//!    위험한 키(Enter, Esc, F5, F7, F10, Space, 숫자, Alt)는 [`input::Key`]로 표현 불가
//!    Enter와 Esc는 정렬 창이 화면에 뜬 뒤에 쓰는 [`input::PopupKey`]로만 있음
//!
//! 안티치트(XIGNCODE3) 때문에 게임 프로세스 핸들을 안 열고 메모리도 안 읽고 가짜 포커스 메시지도 안 보냄
//! 쓰는 건 FindWindow, PrintWindow / GDI 캡처, GetForegroundWindow, SetForegroundWindow, SendInput뿐

pub mod features;
pub mod geom;
pub mod img;
pub mod panel;
pub mod read;
pub mod sort;
pub mod state;
pub mod tabs;

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod input;

pub use features::{
    jacket_feature, Candidate, Feature, RefSet, SongId, FEAT_LEN, FEAT_SIZE, RANDOM_ID,
};
pub use geom::{at_rest, find_list, ListGeom};
pub use img::{Rect, Rgb, RgbView};
pub use panel::{find_diff_slots, slot_states, Difficulty, SlotState};
pub use read::{read_window, RowRead};
pub use state::{analyze, classify, ScreenInfo, ScreenKind};

/// 목록에 보이는 곡 줄 수
pub const NROWS: usize = 11;
/// 보이는 줄 중 선택(커서) 줄 위치
pub const SEL_ROW: usize = 5;
/// 기준 픽셀 수치의 클라이언트 높이
pub const REF_H: f32 = 1440.0;
