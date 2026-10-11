//! 레벨 조건: `[모드] 난이도 레벨` 또는 `[모드] 난이도 레벨-레벨`
//! ("sc15", "4b sc13-15", "sc 14 ~ 15"), 곡 목록은 [`Filter`](crate::Filter)로 찾음
//!
//! * 모드 4b·5b·6b·8b (없으면 설정의 모드), 난이도 nm·hd·mx·sc (꼭 있어야 함), 레벨 1..15
//! * 범위 구분은 '-' 또는 '~', 양 끝 포함, 거꾸로 쓴 범위도 받음
//! * 난이도가 없으면 ("15", "4b15") 숫자가 든 곡 이름을 찾는 텍스트 검색
//! * 대소문자·공백 무시
//! * 한글 모드로 친 것도 받음, 자모를 2벌식 자판의 영문 키로 읽음 ("ㄴㅊ15"는 sc15)

use crate::catalog::{Diff, Mode, Song};
use crate::filter::keys;

const MODE_KEYS: [(&str, Mode); 4] = [("4b", Mode::B4), ("5b", Mode::B5), ("6b", Mode::B6), ("8b", Mode::B8)];
const DIFF_KEYS: [(&str, Diff); 4] = [("nm", Diff::NM), ("hd", Diff::HD), ("mx", Diff::MX), ("sc", Diff::SC)];

/// 레벨 조건
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelQuery {
    /// 없으면 설정의 모드
    pub mode: Option<Mode>,
    pub diff: Diff,
    /// 레벨 범위 `lo..=hi`, 레벨 하나면 둘이 같음
    pub lo: u8,
    pub hi: u8,
}

impl LevelQuery {
    /// 검색어 전체가 `[모드] 난이도 레벨` 또는 레벨 범위 꼴이면 Some
    pub fn parse(raw: &str) -> Option<LevelQuery> {
        Self::parse_keys(&keys(raw)?)
    }

    /// [`keys`]로 바꾼 검색어에서 읽음
    pub(crate) fn parse_keys(keys: &str) -> Option<LevelQuery> {
        let mut rest = keys;
        let mut take = |prefix: &str| {
            let hit = rest.starts_with(prefix);
            if hit {
                rest = &rest[prefix.len()..];
            }
            hit
        };
        let mode = MODE_KEYS.iter().find(|(k, _)| take(k)).map(|&(_, m)| m);
        let diff = DIFF_KEYS.iter().find(|(k, _)| take(k)).map(|&(_, d)| d)?;
        // 레벨은 숫자 한두 자리, `parse`가 받는 '+'는 `keys`에서 이미 걸러짐
        let parse_level = |s: &str| s.parse::<u8>().ok().filter(|l| s.len() <= 2 && (1..=15).contains(l));
        // 범위는 구분자 하나로 이은 두 레벨, 구분자가 더 있으면 뒤쪽이 숫자가 아니라서 걸림
        let (a, b) = rest.split_once(['-', '~']).unwrap_or((rest, rest));
        let (a, b) = (parse_level(a)?, parse_level(b)?);
        Some(LevelQuery { mode, diff, lo: a.min(b), hi: a.max(b) })
    }

    /// 모드 `mode` (실제로 적용할 모드)에서 이 난이도·레벨 범위인 패턴이 있음
    pub fn matches(&self, song: &Song, mode: Mode) -> bool {
        song.level(mode, self.diff).is_some_and(|l| (self.lo..=self.hi).contains(&l))
    }
}
