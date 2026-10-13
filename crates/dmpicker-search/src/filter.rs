//! 조건 검색: 검색어 전체가 레벨·BPM·패턴 특성 조건이면 맞는 곡이나 패턴을 모두 모음
//! ("sc15", "bpm:150~180", "4b sc14-15 bpm200~", "트릴:2~ 연타:1~")
//!
//! * 레벨 조건은 [`LevelQuery`]
//! * BPM 조건은 `bpm` 뒤에 BPM 하나나 범위, ':'는 써도 되고 안 써도 됨 ("bpm150", "bpm:150~180")
//! * 패턴 특성 조건은 `이름:값`, 값은 0..=3 하나나 범위 ("트릴:2", "트릴:1~2")
//! * 범위 구분은 '-' 또는 '~', 양 끝 포함, 거꾸로 쓴 범위도 받음
//! * 한쪽 끝을 비우면 그 이상·이하 ("bpm200~", "bpm~120", "트릴:1~")
//! * 변속곡은 최저·최고 BPM이 둘 다 범위 안이어야 맞음, 그래서 "bpm100"은 BPM이 100으로 고정된 곡만
//! * 소수 BPM은 반올림해서 비교 (123.8은 124)
//! * 레벨·BPM 조건은 하나씩, 어느 쪽을 먼저 써도 됨 ("sc14 bpm150", "bpm150 4b sc14")
//! * 패턴 특성 조건은 아무 데나 몇 개든 ("트릴:2~ sc14 연타:1~")
//! * 같이 쓰면 전부 맞는 같은 패턴, 레벨 조건이 없으면 설정 모드의 아무 난이도
//! * 대소문자·공백 무시, 한글 모드로 친 것도 받음 ("ㅠㅔㅡ150")
//! * 결과는 게임 목록의 제목순 (영문, 한자·가나, 한글, 기호, 숫자), 같은 곡은 NM부터

use std::collections::BTreeMap;

use crate::catalog::{Diff, Mode, Song};
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

/// "a", "a~b", "a~", "~b" ('-'도 됨), 비운 끝은 `min`·`max`, 거꾸로 쓴 범위는 바로잡음
pub(crate) fn range<T: Ord + Copy>(s: &str, one: impl Fn(&str) -> Option<T>, min: T, max: T) -> Option<(T, T)> {
    let (a, b) = match s.split_once(['-', '~']) {
        None => (one(s)?, one(s)?),
        Some(("", "")) => return None,
        Some((a, "")) => (one(a)?, max),
        Some(("", b)) => (min, one(b)?),
        // 구분자가 더 있으면 뒤쪽이 숫자가 아니라서 걸림
        Some((a, b)) => (one(a)?, one(b)?),
    };
    Some((a.min(b), a.max(b)))
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
        let (lo, hi) = range(s, bpm, 0, u16::MAX)?;
        Some(BpmRange { lo, hi })
    }

    /// 곡의 최저·최고 BPM이 둘 다 범위 안, BPM을 모르는 곡은 안 맞음
    pub fn matches(&self, song: &Song) -> bool {
        song.bpm_range.is_some_and(|(lo, hi)| f32::from(self.lo) <= lo.round() && hi.round() <= f32::from(self.hi))
    }
}

/// 공백을 빼고 영문은 소문자로
fn squeeze(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_lowercase()).collect()
}

/// 검색어에 쓸 수 있는 패턴 특성 이름, 공백을 빼고 소문자로 바꾼 이름과 그 이름으로 찾는 특성들
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagNames(BTreeMap<String, Vec<String>>);

impl TagNames {
    /// 전체 이름, '/'를 뺀 이름, '/'로 나눈 조각 어느 것으로도 찾음
    /// ("레이저 / 가변"은 "레이저/가변", "레이저가변", "레이저", "가변")
    /// 숫자나 ':'·'~'·'-'가 든 이름은 값과 헷갈려서 뺌
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> TagNames {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in names {
            let joined = name.replace('/', "");
            for form in [name, joined.as_str()].into_iter().chain(name.split('/')) {
                let key = squeeze(form);
                if key.is_empty() || key.contains(|c: char| c.is_ascii_digit() || matches!(c, ':' | '~' | '-')) {
                    continue;
                }
                let found = map.entry(key).or_default();
                if !found.iter().any(|n| n == name) {
                    found.push(name.to_owned());
                }
            }
        }
        TagNames(map)
    }

    /// `songs`의 패턴 특성에 나오는 이름 전부
    pub fn from_songs(songs: &[Song]) -> TagNames {
        TagNames::new(songs.iter().flat_map(|s| s.pattern_tags.iter().flatten().flatten().flatten()).map(|(n, _)| n.as_str()))
    }

    /// 특성 이름 뒤에 ':'가 있는 검색어, 값이 비었거나 틀려도 일반 검색으로 안 넘기려고 값은 안 봄
    pub fn has_tag_syntax(&self, raw: &str) -> bool {
        let text = squeeze(raw);
        text.match_indices(':').any(|(at, _)| self.suffix(&text[..at]).is_some())
    }

    /// `s` 끝에 붙은 이름 중 가장 긴 것의 (키 길이, 특성들), 겹계단이 계단으로 안 읽히게
    /// 이름 앞이 맨 앞이나 숫자·'~'·'-'일 때만, 풀사이드의 사이드는 안 잡음
    fn suffix(&self, s: &str) -> Option<(usize, &[String])> {
        self.0.iter().filter(|(k, _)| {
            s.strip_suffix(k.as_str()).is_some_and(|before| {
                before.chars().next_back().is_none_or(|c| c.is_ascii_digit() || matches!(c, '~' | '-'))
            })
        }).max_by_key(|(k, _)| k.len()).map(|(k, n)| (k.len(), n.as_slice()))
    }
}

/// 패턴 특성 조건 `lo..=hi`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagRange {
    /// 검색어 이름에 맞는 특성, 한 패턴에 여럿 있으면 큰 값으로 봄
    pub names: Vec<String>,
    /// 비워 둔 끝은 0
    pub lo: u8,
    /// 비워 둔 끝은 `u8::MAX`
    pub hi: u8,
}

impl TagRange {
    /// ':' 뒤 "2", "1~2", "1~", "~1"과 그 길이
    /// 값은 0..=3 한 자리만 받아서 바로 뒤에 붙은 "4b" 같은 모드와 안 헷갈림
    fn parse(s: &str) -> Option<(u8, u8, usize)> {
        let b = s.as_bytes();
        let digit = |i: usize| b.get(i).filter(|c| (b'0'..=b'3').contains(c)).map(|c| c - b'0');
        let lo = digit(0);
        let mut len = usize::from(lo.is_some());
        if !matches!(b.get(len), Some(b'-' | b'~')) {
            return lo.map(|v| (v, v, len));
        }
        let hi = digit(len + 1);
        len += 1 + usize::from(hi.is_some());
        if lo.is_none() && hi.is_none() {
            return None;
        }
        let (lo, hi) = (lo.unwrap_or(0), hi.unwrap_or(u8::MAX));
        Some((lo.min(hi), lo.max(hi), len))
    }

    /// 평가된 패턴에 그 특성이 없으면 0, 평가가 없는 패턴은 안 맞음
    pub fn matches(&self, tags: Option<&[(String, u8)]>) -> bool {
        tags.is_some_and(|tags| {
            let v = tags.iter().filter(|(n, _)| self.names.contains(n)).map(|&(_, v)| v).max().unwrap_or(0);
            (self.lo..=self.hi).contains(&v)
        })
    }
}

/// 조건 검색어, 셋 중 하나는 꼭 있음
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub level: Option<LevelQuery>,
    pub bpm: Option<BpmRange>,
    pub tags: Vec<TagRange>,
}

impl Filter {
    /// 검색어 전체가 조건으로만 이뤄졌으면 Some
    /// 패턴 특성 이름은 `names`에 있는 것만 받음
    pub fn parse(raw: &str, names: &TagNames) -> Option<Filter> {
        // 특성 이름은 친 글자 그대로 비교, 자판 키로 바꾸면 "게단"이 계단으로 읽힘
        let text = squeeze(raw);
        let mut rest = Vec::new();
        let mut tags = Vec::new();
        let mut from = 0;
        for (at, _) in text.match_indices(':') {
            // 이름이 안 맞는 ':'는 "bpm:150"처럼 남은 글에 그대로 둠
            let Some((len, found)) = names.suffix(&text[from..at]) else { continue };
            let (lo, hi, used) = TagRange::parse(&text[at + 1..])?;
            rest.push(&text[from..at - len]);
            tags.push(TagRange { names: found.to_vec(), lo, hi });
            from = at + 1 + used;
        }
        rest.push(&text[from..]);
        let mut filter = Filter { level: None, bpm: None, tags };
        // 남은 글을 이어 붙이면 "sc1트릴:24"가 sc14로 읽혀서 조각마다 따로 봄
        for part in rest.into_iter().filter(|p| !p.is_empty()) {
            let (level, bpm) = Self::level_and_bpm(&keys(part)?)?;
            // 레벨·BPM 조건은 하나씩만
            if (level.is_some() && filter.level.is_some()) || (bpm.is_some() && filter.bpm.is_some()) {
                return None;
            }
            filter.level = filter.level.or(level);
            filter.bpm = filter.bpm.or(bpm);
        }
        (filter.level.is_some() || filter.bpm.is_some() || !filter.tags.is_empty()).then_some(filter)
    }

    /// 레벨 조건, BPM 조건 또는 둘을 이은 꼴
    fn level_and_bpm(keys: &str) -> Option<(Option<LevelQuery>, Option<BpmRange>)> {
        let Some(at) = keys.find("bpm") else {
            return Some((Some(LevelQuery::parse_keys(keys)?), None));
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
        Some((level, Some(bpm)))
    }

    /// 설정 모드로 찾는 패턴 조건, 레벨 조건에 모드가 없거나 레벨 조건 없이 특성 조건이 있음
    pub fn uses_mode_setting(&self) -> bool {
        match self.level {
            Some(l) => l.mode.is_none(),
            None => !self.tags.is_empty(),
        }
    }

    /// 조건에 맞는 (`songs` 인덱스, 패턴 난이도), `allow`가 받는 곡만
    /// 레벨이나 특성 조건이 있으면 맞는 패턴마다, 없으면 곡마다 난이도 None
    /// 패턴은 모드 `mode` (실제로 적용할 모드)로 봄
    /// 게임 목록의 제목순 (`dmpicker_nav::order_key`, 같으면 원래 이름), 같은 곡은 NM부터
    pub fn find(&self, songs: &[Song], mode: Mode, allow: impl Fn(usize) -> bool) -> Vec<(usize, Option<Diff>)> {
        let fits = |s: &Song, d: Diff| s.level(mode, d).is_some() && self.tags.iter().all(|t| t.matches(s.pattern_tags(mode, d)));
        let mut hits: Vec<(Option<dmpicker_nav::OrderKey>, usize, Option<Diff>)> = Vec::new();
        for (i, s) in songs.iter().enumerate() {
            if !self.bpm.is_none_or(|b| b.matches(s)) || !allow(i) {
                continue;
            }
            let diffs: Vec<Option<Diff>> = if self.level.is_none() && self.tags.is_empty() {
                vec![None]
            } else {
                Diff::ALL.into_iter().filter(|&d| self.level.is_none_or(|l| l.diff == d && l.matches(s, mode)) && fits(s, d)).map(Some).collect()
            };
            if !diffs.is_empty() {
                let key = dmpicker_nav::order_key(&s.name);
                hits.extend(diffs.into_iter().map(|d| (key.clone(), i, d)));
            }
        }
        // 안정 정렬이라 같은 곡의 패턴은 넣은 순서 (NM부터) 그대로
        hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| songs[a.1].name.cmp(&songs[b.1].name)));
        hits.into_iter().map(|(_, i, d)| (i, d)).collect()
    }
}
