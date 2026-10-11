//! 조건 검색: 검색어 전체가 레벨 조건, BPM 조건 또는 둘 다면 맞는 곡을 모두 모음
//! ("sc15", "bpm:150~180", "4b sc14-15 bpm200~")
//!
//! * 레벨 조건은 [`LevelQuery`]
//! * BPM 조건은 `bpm` 뒤에 BPM 하나나 범위, ':'는 써도 되고 안 써도 됨 ("bpm150", "bpm:150~180")
//! * 범위 구분은 '-' 또는 '~', 양 끝 포함, 거꾸로 쓴 범위도 받음
//! * 한쪽 끝을 비우면 그 이상·이하 ("bpm200~", "bpm~120")
//! * 변속곡은 최저·최고 BPM이 둘 다 범위 안이어야 맞음, 그래서 "bpm100"은 BPM이 100으로 고정된 곡만
//! * 소수 BPM은 반올림해서 비교 (123.8은 124)
//! * 레벨 조건과 같이 쓰면 둘 다 맞는 곡, 어느 쪽을 먼저 써도 됨 ("sc14 bpm150", "bpm150 4b sc14")
//! * 대소문자·공백 무시, 한글 모드로 친 것도 받음 ("ㅠㅔㅡ150")
//! * 결과는 게임 목록의 제목순 (영문, 한자·가나, 한글, 기호, 숫자)

use crate::catalog::{Mode, Song};
use crate::hangul;
use crate::level::LevelQuery;

/// 공백을 빼고 영문 소문자·숫자·'-'·'~'·':'로, 한글은 그 자모를 치는 2벌식 자판의 영문 키로
/// 다른 글자가 있으면 None
pub(crate) fn keys(raw: &str) -> Option<String> {
    let mut keys = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '~' | ':') {
            keys.push(c.to_ascii_lowercase());
        } else if hangul::is_hangul(c) {
            let mut ok = true;
            hangul::push_keystrokes(c, &mut |j| match hangul::jamo_to_qwerty(j) {
                Some(k) => keys.push(k),
                None => ok = false,
            });
            if !ok {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(keys)
}

/// BPM 범위 `lo..=hi`, BPM 하나면 둘이 같음
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BpmRange {
    /// 비워 둔 끝은 0
    pub lo: u16,
    /// 비워 둔 끝은 `u16::MAX`
    pub hi: u16,
}

impl BpmRange {
    /// "150", "150~180", "200~", "~120" (':'는 뺀 뒤)
    fn parse(s: &str) -> Option<BpmRange> {
        // `u16` 파싱이 받는 '+'는 `keys`에서 이미 걸러짐
        let bpm = |t: &str| t.parse::<u16>().ok().filter(|&b| b > 0);
        let (lo, hi) = match s.split_once(['-', '~']) {
            None => (bpm(s)?, bpm(s)?),
            Some(("", "")) => return None,
            Some((a, "")) => (bpm(a)?, u16::MAX),
            Some(("", b)) => (0, bpm(b)?),
            // 구분자가 더 있으면 뒤쪽이 숫자가 아니라서 걸림
            Some((a, b)) => (bpm(a)?, bpm(b)?),
        };
        Some(BpmRange { lo: lo.min(hi), hi: lo.max(hi) })
    }

    /// 곡의 최저·최고 BPM이 둘 다 범위 안, BPM을 모르는 곡은 안 맞음
    pub fn matches(&self, song: &Song) -> bool {
        song.bpm_range.is_some_and(|(lo, hi)| f32::from(self.lo) <= lo.round() && hi.round() <= f32::from(self.hi))
    }
}

/// 조건 검색어, 둘 중 하나는 꼭 있음
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub level: Option<LevelQuery>,
    pub bpm: Option<BpmRange>,
}

impl Filter {
    /// 검색어 전체가 레벨 조건, BPM 조건 또는 둘을 이은 꼴이면 Some, 아니면 텍스트 검색
    pub fn parse(raw: &str) -> Option<Filter> {
        let keys = keys(raw)?;
        let Some(at) = keys.find("bpm") else {
            return LevelQuery::parse_keys(&keys).map(|level| Filter { level: Some(level), bpm: None });
        };
        let (before, after) = (&keys[..at], &keys[at + 3..]);
        // BPM 뒤에 레벨 조건이 오면 첫 영문자부터, 모드로 시작하면 ("bpm150 4b sc14") 그 숫자부터 레벨
        let split = match after.find(|c: char| c.is_ascii_alphabetic()) {
            Some(p) if p > 0 && after[p..].starts_with('b') && matches!(after.as_bytes()[p - 1], b'4' | b'5' | b'6' | b'8') => p - 1,
            Some(p) => p,
            None => after.len(),
        };
        let (range, rest) = after.split_at(split);
        // 레벨 조건은 BPM 조건의 앞이나 뒤 한쪽에만
        let level = match (before, rest) {
            ("", "") => None,
            (l, "") | ("", l) => Some(LevelQuery::parse_keys(l)?),
            _ => return None,
        };
        let bpm = BpmRange::parse(range.strip_prefix(':').unwrap_or(range))?;
        Some(Filter { level, bpm: Some(bpm) })
    }

    /// 조건에 맞는 곡의 `songs` 인덱스, `allow`가 받는 곡만
    /// 레벨 조건은 모드 `mode` (실제로 적용할 모드)로 봄
    /// 게임 목록의 제목순 (`dmpicker_nav::order_key`, 같으면 원래 이름)
    pub fn find(&self, songs: &[Song], mode: Mode, allow: impl Fn(usize) -> bool) -> Vec<usize> {
        let mut hits: Vec<(Option<dmpicker_nav::OrderKey>, usize)> = songs
            .iter()
            .enumerate()
            .filter(|&(i, s)| {
                self.level.is_none_or(|l| l.matches(s, mode)) && self.bpm.is_none_or(|b| b.matches(s)) && allow(i)
            })
            .map(|(i, s)| (dmpicker_nav::order_key(&s.name), i))
            .collect();
        hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| songs[a.1].name.cmp(&songs[b.1].name)));
        hits.into_iter().map(|(_, i)| i).collect()
    }
}
