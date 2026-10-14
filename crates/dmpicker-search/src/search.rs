//! 검색기: 미리 펼쳐 둔 검색 문자열 + 등급별 점수
//!
//! 곡마다 검색할 문자열 (이름, 별칭, 내 키워드, 약어, 작곡가 키, DLC·장르 키) 하나하나가 *키*
//! 키를 `\0`으로 구분해 이어 붙여 긴 문자열 세 개를 만듦
//!
//! * `compact`: 정규화한 텍스트 (모든 키)
//! * `keystroke`: 2벌식 타자 자모 (한글이 든 키만)
//! * `chosung`: 초성 (한글이 든 이름·별칭·키워드 키만)
//!
//! 검색은 해당 문자열을 처음부터 끝까지 훑음 (`char` 수백 KB)
//! 찾은 자리는 위치별 표로 키를 찾아 점수를 매기고, 곡마다 최고 점수만 남김
//! 검색 중엔 곡별 할당이 없음

use crate::catalog::Catalog;
use crate::hangul;
use crate::normalize::{normalize, normalize_into, NormChar};
use std::collections::{HashMap, HashSet};

/// 검색 결과 하나
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// `catalog.songs` 인덱스
    pub index: usize,
    /// 클수록 좋음, 등급은 crate 문서에 있음
    pub score: i32,
}

/// 키의 종류 (점수 등급을 정함)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Name,
    /// core.json `segments` 중 제목 별칭·발음
    Alias,
    /// 내 키워드, compact 완전 일치만 이름보다 위
    User,
    /// 이름 각 단어의 첫 글자 ("!!New Game Start!!"면 "ngs")
    Acronym,
    Composer,
    /// DLC 이름·코드, 장르, 여러 곡이 같이 쓰는 segment (팩 이름)
    Group,
}

const SEP: char = '\0';
/// 매치가 시작될 수 있는 위치 (음절 시작, `compact`에선 항상 켜짐)
const F_START: u8 = 1;
/// Name / Composer / Group 키에서 단어가 시작되는 위치
const F_WORD: u8 = 2;
/// Name / Composer / Group 키에서 구분자로 나뉜 단어가 시작되는 위치
/// (바로 앞에 공백, '-', '~', '(', ':' 같은 게 있었음)
const F_SEP: u8 = 4;
/// `for_each_match`만 붙이는 값: 매치가 키 끝이나 `F_SEP` 위치 바로 앞에서 끝남
/// 즉 단어 하나를 통째로 맞춤
const F_ENDS_WORD: u8 = 8;
/// `for_each_match`만 붙이는 값: 매치 안 (첫 위치 다음)에서 `F_SEP` 단어가 시작됨
/// camelCase 경계는 안 셈 ("ALiCE" 안의 "lice")
const F_CROSSES: u8 = 16;

// ---- 점수 등급 --------------------------------------------------------
/// 내 키워드 compact 완전 일치, 이름이 똑같은 곡보다도 위
const USER_EXACT: i32 = NAME_EXACT + 1;
const NAME_EXACT: i32 = 1000;
const ALIAS_EXACT: i32 = 950;
/// 이름이 검색어를 온전한 단어로 시작함 ("zero"로 "Zero-Break")
const NAME_PREFIX_WORD: i32 = 920;
const NAME_PREFIX: i32 = 900;
const ALIAS_PREFIX: i32 = 850;
const NAME_WORD: i32 = 800;
/// 단어 시작에서 맞고 단어 경계에서 끝나기까지 하면 더하는 값
const WHOLE_WORD_BONUS: i32 = 10;
const ACRONYM_EXACT: i32 = 790;
const ACRONYM_PREFIX: i32 = 765;
/// 검색어가 아티스트 이름·별칭 그 자체면 ("xeon", "테라마이신", "esti")
/// 제목 단어 중간에 걸린 것 ("B-esti-e")보다 높음
const COMPOSER_EXACT: i32 = 760;
const NAME_SUB: i32 = 750;
const COMPOSER_PREFIX: i32 = 710;
/// 작곡가 표기 안의 단어 시작 ("escape"로 "Forte Escape")
const COMPOSER_WORD: i32 = 700;
const ALIAS_SUB: i32 = 700;
/// 한글이 타자 자모 단계에서만 맞을 때 빼는 값 ("비상"을 치는 중의 "빗", "비사")
const KEYSTROKE_PENALTY: i32 = 30;
/// 타자 자모 단계 매치는 아직 치는 중인 음절을 짐작한 것 ("다이나믹"으로 가는 중의 "다인")
/// 친 그대로가 어떤 곡의 이름·별칭·아티스트와 똑같으면 (DIE IN, 아티스트 DIEIN) 다 친 걸로 봄
/// 그때 짐작한 매치는 그보다 아래로 내림
const KEYSTROKE_MAX: i32 = COMPOSER_EXACT - 1;
const CHO_NAME_EXACT: i32 = 700;
const CHO_NAME_PREFIX: i32 = 680;
const CHO_ALIAS_EXACT: i32 = 670;
const CHO_NAME_WORD: i32 = 650;
const CHO_ALIAS_PREFIX: i32 = 650;
const CHO_NAME_SUB: i32 = 630;
const CHO_ALIAS_SUB: i32 = 600;
const GROUP_EXACT: i32 = 540;
const GROUP_PREFIX: i32 = 500;
/// DLC 이름 안의 단어 시작 ("extension"으로 "V EXTENSION 3")
const GROUP_WORD: i32 = 480;
/// 작곡가·팩 문자열의 단어 중간 매치는 약한 근거
/// ("Sam-pli-ng Masters"의 "pli"가 PLI 팩을 이기면 안 됨)
const COMPOSER_SUB: i32 = 460;
const GROUP_SUB: i32 = 420;
/// IME 한/영 모드를 잘못 두고 친 검색어에서 빼는 값 ("뇆샤ㅡㄷ", "qltkd")
const LAYOUT_PENALTY: i32 = 300;
/// 검색어와 오타 한두 개 차이로 시작하는 이름·별칭 ("fermoin", "설렘"으로 설레임)
/// 이 값에서 고친 수마다 `TYPO_EDIT`를 빼고, 별칭이면 10을 더 뺌
const TYPO: i32 = 390;
const TYPO_EDIT: i32 = 30;
const FUZZY_MAX: i32 = 300;
const FUZZY_MIN: i32 = 100;
/// 같은 등급 안에서 매치 위치 때문에 빼는 최대 값
const MAX_OFF_PENALTY: usize = 20;
/// 이만큼 이상의 곡에 나오는 segment는 그룹 별칭으로 봄
/// (팩·콜라보 이름, "리스펙트", "익텐3", "블아" 등)
const GROUP_DF: u32 = 4;

#[derive(Clone, Copy, Debug)]
struct KeyRef {
    start: u32,
    len: u32,
    song: u32,
    kind: Kind,
}

/// 펼쳐 둔 검색 문자열 하나
#[derive(Default)]
struct Hay {
    text: Vec<char>,
    flags: Vec<u8>,
    pos_key: Vec<u32>,
    keys: Vec<KeyRef>,
    /// 곡별 `keys` 범위
    song_keys: Vec<(u32, u32)>,
}

impl Hay {
    fn begin_song(&mut self) -> u32 {
        self.keys.len() as u32
    }

    fn end_song(&mut self, first_key: u32) {
        self.song_keys.push((first_key, self.keys.len() as u32));
    }

    fn push_key(&mut self, song: u32, kind: Kind, units: &[(char, u8)]) {
        if units.is_empty() {
            return;
        }
        let k = self.keys.len() as u32;
        let start = self.text.len() as u32;
        for &(c, f) in units {
            self.text.push(c);
            self.flags.push(f);
            self.pos_key.push(k);
        }
        self.text.push(SEP);
        self.flags.push(0);
        self.pos_key.push(k);
        self.keys.push(KeyRef {
            start,
            len: units.len() as u32,
            song,
            kind,
        });
    }

    fn shrink(&mut self) {
        self.text.shrink_to_fit();
        self.flags.shrink_to_fit();
        self.pos_key.shrink_to_fit();
        self.keys.shrink_to_fit();
    }

    /// `needle`이 나올 때마다 `f(key, offset_in_key, flags)` 호출
    /// `flags`는 매치 시작 위치의 플래그에, 단어 경계에서 끝나면 `F_ENDS_WORD`,
    /// 구분자를 넘어가면 `F_CROSSES`를 더한 값
    /// `needle`엔 `\0`이 없어서 매치가 키를 넘지 않음
    #[inline]
    fn for_each_match(&self, needle: &[char], mut f: impl FnMut(&KeyRef, usize, u8)) {
        let n = needle.len();
        let text = &self.text[..];
        if n == 0 || n > text.len() {
            return;
        }
        let first = needle[0];
        let rest = &needle[1..];
        let end = text.len() - n + 1; // 매치 시작 위치의 상한 (미포함)
        let mut i = 0;
        while i < end {
            let Some(o) = text[i..end].iter().position(|&c| c == first) else {
                return;
            };
            let p = i + o;
            if text[p + 1..p + n] == *rest {
                let key = &self.keys[self.pos_key[p] as usize];
                // 키 뒤엔 항상 SEP이 있어서 p + n은 범위 안
                let mut fl = self.flags[p];
                if text[p + n] == SEP || self.flags[p + n] & F_SEP != 0 {
                    fl |= F_ENDS_WORD;
                }
                if self.flags[p + 1..p + n].iter().any(|&w| w & F_SEP != 0) {
                    fl |= F_CROSSES;
                }
                f(key, p - key.start as usize, fl);
            }
            i = p + 1;
        }
    }

    /// `key` 안에서 `needle`이 순서대로 가장 촘촘하게 나오는 곳
    /// (첫 글자 위치, 길이)를 돌려줌
    fn subsequence(&self, key: &KeyRef, needle: &[char]) -> Option<(usize, usize)> {
        let s = key.start as usize;
        let hay = &self.text[s..s + key.len as usize];
        let n = needle.len();
        let mut best: Option<(usize, usize)> = None;
        for (i, &c) in hay.iter().enumerate() {
            if c != needle[0] {
                continue;
            }
            let mut j = 1;
            let mut p = i + 1;
            while j < n && p < hay.len() {
                if hay[p] == needle[j] {
                    j += 1;
                }
                p += 1;
            }
            if j < n {
                break; // 더 뒤에서 시작해도 못 채움
            }
            let span = p - i;
            if best.is_none_or(|(_, b)| span < b) {
                best = Some((i, span));
                if span == n {
                    break;
                }
            }
        }
        best
    }
}

/// 전처리한 검색어
struct Query {
    compact: Vec<char>,
    keys: Vec<char>,
    has_hangul: bool,
    all_consonants: bool,
    /// 한글 검색어를 QWERTY로 바꾼 것 (실수로 한글 모드에서 친 경우)
    latin_from_jamo: Option<Vec<char>>,
    /// 영문 검색어를 2벌식 자모로 바꾼 것 (실수로 영문 모드에서 친 경우)
    jamo_from_latin: Option<Vec<char>>,
}

impl Query {
    fn parse(raw: &str) -> Query {
        let mut nc = Vec::with_capacity(raw.len());
        normalize_into(raw, &mut nc);
        let compact: Vec<char> = nc.iter().map(|n| n.c).collect();
        let has_hangul = compact.iter().any(|&c| hangul::is_hangul(c));
        let mut keys = Vec::with_capacity(compact.len() * 3);
        for &c in &compact {
            hangul::push_keystrokes(c, &mut |k| keys.push(k));
        }
        let all_consonants =
            !keys.is_empty() && keys.iter().all(|&k| hangul::is_compat_consonant(k));

        // 자음만 있어도 됨, 한글 모드로 친 "fear"는 ㄹㄷㅁㄱ, 숫자는 그대로 ("2ㅜㅑㅅㄷ")
        let latin_from_jamo = if has_hangul && keys.len() >= 3 {
            keys.iter()
                .map(|&k| hangul::jamo_to_qwerty(k).or(k.is_ascii_digit().then_some(k)))
                .collect::<Option<Vec<char>>>()
        } else {
            None
        };

        let letters: Vec<char> = raw.chars().filter(|c| !c.is_whitespace()).collect();
        let jamo_from_latin =
            if letters.len() >= 3 && letters.iter().all(|c| c.is_ascii_alphabetic()) {
                let j: Vec<char> = letters
                    .iter()
                    .filter_map(|&c| hangul::qwerty_to_jamo(c))
                    .collect();
                let plausible = hangul::is_compat_consonant(j[0])
                    && j.iter().any(|&k| hangul::is_compat_vowel(k));
                plausible.then_some(j)
            } else {
                None
            };

        Query {
            compact,
            keys,
            has_hangul,
            all_consonants,
            latin_from_jamo,
            jamo_from_latin,
        }
    }
}

#[inline]
fn off_penalty(off: usize) -> i32 {
    off.min(MAX_OFF_PENALTY) as i32
}

/// `len` 글자 검색어에 허용하는 오타 수 (한글은 타자 자모 기준)
/// 4글자·5자모 미만은 0, 8글자·10자모 미만은 1, 그 이상은 2
fn typo_budget(len: usize, hangul: bool) -> usize {
    let (one, two) = if hangul { (5, 10) } else { (4, 8) };
    if len >= two {
        2
    } else if len >= one {
        1
    } else {
        0
    }
}

/// `q`를 `key`의 어떤 앞부분으로 바꾸는 최소 편집 수, `max` 이하일 때만
/// (삽입, 삭제, 바꾸기, 이웃 둘 맞바꾸기)
/// 첫 글자는 친 그대로로 봄 ("leve"는 "fever"가 아님), `rows`는 작업용 버퍼
fn prefix_typos(q: &[char], key: &[char], max: usize, rows: &mut [Vec<usize>; 3]) -> Option<usize> {
    let n = q.len();
    let m = key.len().min(n + max);
    if m + max < n || key.first() != q.first() {
        return None;
    }
    let far = max + 1; // `max`를 넘으면 다 못 닿는 걸로 침
    let [older, prev, cur] = rows;
    for r in [&mut *older, &mut *prev, &mut *cur] {
        r.clear();
        r.resize(m + 1, far);
    }
    for (j, d) in prev.iter_mut().enumerate().take(max + 1) {
        *d = j;
    }
    for i in 1..=n {
        cur.fill(far);
        if i <= max {
            cur[0] = i;
        }
        let mut best = cur[0];
        for j in i.saturating_sub(max).max(1)..=(i + max).min(m) {
            let mut d = (prev[j - 1] + usize::from(q[i - 1] != key[j - 1]))
                .min(prev[j] + 1)
                .min(cur[j - 1] + 1);
            if i > 1 && j > 1 && q[i - 1] == key[j - 2] && q[i - 2] == key[j - 1] {
                d = d.min(older[j - 2] + 1);
            }
            cur[j] = d.min(far);
            best = best.min(cur[j]);
        }
        if best > max {
            return None;
        }
        std::mem::swap(older, prev);
        std::mem::swap(prev, cur);
    }
    prev[n.saturating_sub(max)..=m]
        .iter()
        .copied()
        .min()
        .filter(|&d| d <= max)
}

/// 단어 중간에서 시작해 구분자를 넘어 다음 단어로 이어진 매치
/// ("t-he lo-st"의 "helo", "ne-d rem-ix"의 "drem"), 잡음이라 매치로 안 침
#[inline]
fn mid_word_into_next(fl: u8) -> bool {
    fl & (F_WORD | F_CROSSES) == F_CROSSES
}

/// compact·keystroke 매치의 점수
#[inline]
fn direct_score(kind: Kind, off: usize, qlen: usize, klen: usize, fl: u8) -> i32 {
    let exact = off == 0 && qlen == klen;
    let prefix = off == 0;
    let word = fl & F_WORD != 0;
    let whole = fl & F_ENDS_WORD != 0;
    let pen = off_penalty(off);
    match kind {
        Kind::Name if exact => NAME_EXACT,
        Kind::Name if prefix && whole => NAME_PREFIX_WORD,
        Kind::Name if prefix => NAME_PREFIX,
        Kind::Name if word && whole => NAME_WORD + WHOLE_WORD_BONUS - pen,
        Kind::Name if word => NAME_WORD - pen,
        Kind::Name => NAME_SUB - pen,
        Kind::Alias | Kind::User if exact => ALIAS_EXACT,
        Kind::Alias | Kind::User if prefix => ALIAS_PREFIX,
        Kind::Alias | Kind::User => ALIAS_SUB - pen,
        Kind::Acronym if exact => ACRONYM_EXACT,
        Kind::Acronym if prefix && qlen >= 2 => ACRONYM_PREFIX,
        Kind::Acronym => 0,
        Kind::Composer if exact => COMPOSER_EXACT,
        Kind::Composer if prefix => COMPOSER_PREFIX,
        Kind::Composer if word => COMPOSER_WORD - pen,
        Kind::Composer => COMPOSER_SUB - pen,
        Kind::Group if exact => GROUP_EXACT,
        Kind::Group if prefix => GROUP_PREFIX,
        Kind::Group if word => GROUP_WORD - pen,
        Kind::Group => GROUP_SUB - pen,
    }
}

/// 초성 매치의 점수 (이름·별칭 키만 색인됨)
#[inline]
fn chosung_score(kind: Kind, off: usize, qlen: usize, klen: usize, word: bool) -> i32 {
    let exact = off == 0 && qlen == klen;
    let pen = off_penalty(off);
    match kind {
        Kind::Name if exact => CHO_NAME_EXACT,
        Kind::Name if off == 0 => CHO_NAME_PREFIX,
        Kind::Name if word => CHO_NAME_WORD - pen,
        Kind::Name => CHO_NAME_SUB - pen,
        Kind::Alias | Kind::User if exact => CHO_ALIAS_EXACT,
        Kind::Alias | Kind::User if off == 0 => CHO_ALIAS_PREFIX,
        Kind::Alias | Kind::User => CHO_ALIAS_SUB - pen,
        _ => 0,
    }
}

/// IME 모드를 잘못 둔 매치의 점수
/// 이름·별칭의 완전 일치·앞부분, 이름의 단어 시작 매치만 셈
#[inline]
fn layout_score(kind: Kind, off: usize, qlen: usize, klen: usize, fl: u8) -> i32 {
    match kind {
        Kind::Name | Kind::Alias | Kind::User if off == 0 || (kind == Kind::Name && fl & F_WORD != 0) => {
            direct_score(kind, off, qlen, klen, fl) - LAYOUT_PENALTY
        }
        _ => 0,
    }
}

#[inline]
fn bump(best: &mut [i32], song: u32, score: i32) {
    let b = &mut best[song as usize];
    if score > *b {
        *b = score;
    }
}

/// 만드는 중인 곡별 키 목록 (중복 제거, 먼저 들어온 종류 우선, 내 키워드는 따로)
struct KeyList {
    keys: Vec<(Vec<NormChar>, Kind)>,
}

impl KeyList {
    fn add_norm(&mut self, v: Vec<NormChar>, kind: Kind) {
        if v.is_empty()
            || self
                .keys
                .iter()
                .any(|(k, t)| {
                    (*t == Kind::User) == (kind == Kind::User) && k.iter().map(|n| n.c).eq(v.iter().map(|n| n.c))
                })
        {
            return;
        }
        self.keys.push((v, kind));
    }

    fn add(&mut self, raw: &str, kind: Kind) {
        let mut v = Vec::new();
        normalize_into(raw, &mut v);
        self.add_norm(v, kind);
    }
}

/// 작곡가 표기를 아티스트별로 나눔
/// "XeoN + 7SEQ"는 ["xeon", "7seq"], "TAK × Zekk"는 ["tak", "zekk"]
fn composer_parts(composer: &str) -> Vec<String> {
    let mut t = format!(" {} ", composer.to_lowercase());
    for w in [
        " x ", " vs. ", " vs ", " feat. ", " feat ", " ft. ", " with ",
    ] {
        t = t.replace(w, "|");
    }
    t.split(['|', '/', '+', '&', ',', '×', '(', ')', '[', ']'])
        .map(normalize)
        .filter(|p| p.chars().count() >= 2)
        .collect()
}

/// 검색 색인, [`Searcher::new`]로 한 번 만들고
/// 키를 칠 때마다 [`Searcher::search`]로 검색
pub struct Searcher {
    compact: Hay,
    keystroke: Hay,
    chosung: Hay,
    /// 동점일 때 쓰는 곡 순위, (compact) 이름이 짧은 것 먼저, 다음은 이름순
    rank: Vec<u32>,
}

impl Searcher {
    pub fn new(cat: &Catalog) -> Searcher {
        let n = cat.songs.len();

        // 정규화한 segment마다 나오는 곡 수 (팩 이름 찾기용)
        let mut df: HashMap<String, u32> = HashMap::new();
        for s in &cat.songs {
            let uniq: HashSet<String> = s.segments.iter().map(|x| normalize(x)).collect();
            for u in uniq {
                *df.entry(u).or_insert(0) += 1;
            }
        }

        let mut compact = Hay::default();
        let mut keystroke = Hay::default();
        let mut chosung = Hay::default();
        let mut name_keys: Vec<String> = Vec::with_capacity(n);
        let mut units: Vec<(char, u8)> = Vec::new();

        for (si, s) in cat.songs.iter().enumerate() {
            let si32 = si as u32;
            let mut list = KeyList { keys: Vec::new() };

            // 이름 + 약어
            let mut name = Vec::new();
            normalize_into(&s.name, &mut name);
            name_keys.push(name.iter().map(|c| c.c).collect());
            let acronym: Vec<NormChar> = name
                .iter()
                .filter(|c| c.sep_start)
                .map(|c| NormChar {
                    c: c.c,
                    word_start: false,
                    sep_start: false,
                })
                .collect();
            list.add_norm(name, Kind::Name);

            // segment 분류
            let mut composer_set: HashSet<String> =
                s.composer_keys.iter().map(|k| normalize(k)).collect();
            composer_set.insert(normalize(&s.composer));
            let parts = composer_parts(&s.composer);
            composer_set.extend(parts.iter().cloned());
            let genre_set: HashSet<String> = s.genres.iter().chain(&s.tags).map(|g| normalize(g)).collect();
            let name_set: HashSet<String> = s.names.iter().map(|n| normalize(n)).collect();
            let mut composer_segs = Vec::new();
            let mut group_segs = Vec::new();
            for seg in &s.segments {
                let ns = normalize(seg);
                if ns.is_empty() {
                    continue;
                }
                if name_set.contains(&ns) {
                    list.add(seg, Kind::Alias);
                } else if composer_set.contains(&ns) {
                    composer_segs.push(ns);
                } else if genre_set.contains(&ns) || df.get(&ns).copied().unwrap_or(0) >= GROUP_DF {
                    group_segs.push(ns);
                } else {
                    list.add(seg, Kind::Alias);
                }
            }
            if acronym.len() >= 2 {
                list.add_norm(acronym, Kind::Acronym);
            }
            list.add(&s.composer, Kind::Composer);
            for k in parts.iter().chain(&s.composer_keys).chain(&composer_segs) {
                list.add(k, Kind::Composer);
            }
            list.add(&s.dlc, Kind::Group);
            list.add(&s.dlc_code, Kind::Group);
            for k in s.genres.iter().chain(&group_segs) {
                list.add(k, Kind::Group);
            }
            // 이름·별칭과 같은 키워드도 따로 넣음, 이름 키의 단어 점수가 남게
            for k in &s.user_keys {
                list.add(k, Kind::User);
            }

            // 검색 문자열에 펼쳐 넣음
            let (c0, k0, h0) = (
                compact.begin_song(),
                keystroke.begin_song(),
                chosung.begin_song(),
            );
            for (key, kind) in &list.keys {
                // 단어 시작은 표시용 문자열로 만든 키에서만 의미 있음 (별칭은 이미 compact)
                let worded = matches!(kind, Kind::Name | Kind::Composer | Kind::Group);
                let word = |nc: &NormChar| {
                    let w = if worded && nc.word_start { F_WORD } else { 0 };
                    let sep = if worded && nc.sep_start { F_SEP } else { 0 };
                    w | sep
                };

                units.clear();
                units.extend(key.iter().map(|nc| (nc.c, F_START | word(nc))));
                compact.push_key(si32, *kind, &units);

                if !key.iter().any(|nc| hangul::is_hangul(nc.c)) {
                    continue;
                }
                units.clear();
                for nc in key {
                    let mut flag = F_START | word(nc);
                    hangul::push_keystrokes(nc.c, &mut |k| {
                        units.push((k, flag));
                        flag = 0;
                    });
                }
                keystroke.push_key(si32, *kind, &units);

                if matches!(kind, Kind::Name | Kind::Alias | Kind::User) {
                    units.clear();
                    for nc in key {
                        let mut flag = F_START | word(nc);
                        hangul::push_chosung(nc.c, &mut |k| {
                            units.push((k, flag));
                            flag = F_START;
                        });
                    }
                    chosung.push_key(si32, *kind, &units);
                }
            }
            compact.end_song(c0);
            keystroke.end_song(k0);
            chosung.end_song(h0);
        }
        compact.shrink();
        keystroke.shrink();
        chosung.shrink();

        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            let (na, nb) = (&name_keys[a], &name_keys[b]);
            na.chars()
                .count()
                .cmp(&nb.chars().count())
                .then_with(|| na.cmp(nb))
                .then(a.cmp(&b))
        });
        let mut rank = vec![0u32; n];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r as u32;
        }

        Searcher {
            compact,
            keystroke,
            chosung,
            rank,
        }
    }

    /// 색인한 곡 수
    fn len(&self) -> usize {
        self.rank.len()
    }

    /// `query`로 검색, 점수 높은 순으로 최대 `limit`개
    /// 비었거나 공백·문장 부호뿐인 검색어는 결과 없음
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        self.search_where(query, limit, |_| true)
    }

    /// `allow`가 받아 주는 곡 (catalog 인덱스) 안에서만 [`Searcher::search`]
    /// 플레이어가 가진 곡만 찾을 때 등
    pub fn search_where(&self, query: &str, limit: usize, allow: impl Fn(usize) -> bool) -> Vec<Hit> {
        let n = self.len();
        if limit == 0 || n == 0 {
            return Vec::new();
        }
        let q = Query::parse(query);
        if q.compact.is_empty() {
            return Vec::new();
        }
        let mut best = vec![0i32; n];

        // 1. 모든 키에서 compact (음절까지 정확한) 매치
        let ql = q.compact.len();
        let mut whole = false; // 검색어가 후보 곡의 이름·별칭·키워드·아티스트와 똑같음
        self.compact.for_each_match(&q.compact, |key, off, fl| {
            let exact = off == 0 && ql == key.len as usize;
            whole |= exact && matches!(key.kind, Kind::Name | Kind::Alias | Kind::User | Kind::Composer) && allow(key.song as usize);
            if mid_word_into_next(fl) {
                return;
            }
            let word = fl & F_WORD != 0;
            let s = if exact && key.kind == Kind::User {
                USER_EXACT
            } else if q.all_consonants && off > 0 {
                // "ㄷㄱㄷㄱㄷㄷ" 같은 자모뿐인 별칭 안이면 강한 부분 매치가 아니라 초성 매치
                chosung_score(key.kind, off, ql, key.len as usize, word)
            } else {
                direct_score(key.kind, off, ql, key.len as usize, fl)
            };
            bump(&mut best, key.song, s);
        });

        // 2. 타자 자모 단계 매치: 조합 중인 한글 ("빗", "비사")
        if q.has_hangul && !q.all_consonants {
            let kl = q.keys.len();
            let cap = if whole { KEYSTROKE_MAX } else { i32::MAX };
            self.keystroke.for_each_match(&q.keys, |key, off, fl| {
                if fl & F_START != 0 && !mid_word_into_next(fl) {
                    let s = direct_score(key.kind, off, kl, key.len as usize, fl);
                    if s > 0 {
                        bump(&mut best, key.song, (s - KEYSTROKE_PENALTY).min(cap));
                    }
                }
            });
        }

        // 3. 초성: 자음만 있는 검색어 ("ㅂㅅ", "ㄱㅂㄲ", IME의 "ㅄ")
        if q.all_consonants {
            let kl = q.keys.len();
            self.chosung.for_each_match(&q.keys, |key, off, fl| {
                let s = chosung_score(key.kind, off, kl, key.len as usize, fl & F_WORD != 0);
                bump(&mut best, key.song, s);
            });
        }

        // 4. IME 모드를 잘못 두고 친 경우
        if let Some(latin) = &q.latin_from_jamo {
            let l = latin.len();
            self.compact.for_each_match(latin, |key, off, fl| {
                let s = layout_score(key.kind, off, l, key.len as usize, fl);
                bump(&mut best, key.song, s);
            });
        }
        if let Some(jamo) = &q.jamo_from_latin {
            let l = jamo.len();
            self.keystroke.for_each_match(jamo, |key, off, fl| {
                if fl & F_START != 0 {
                    let s = layout_score(key.kind, off, l, key.len as usize, fl);
                    bump(&mut best, key.song, s);
                }
            });
        }

        for (song, b) in best.iter_mut().enumerate() {
            if !allow(song) {
                *b = i32::MIN; // 결과에서도, 아래 대체 검색에서도 뺌
            }
        }

        // 5. 검색어와 오타 한두 개 차이로 시작하는 이름·별칭
        // 한글은 타자 자모로 비교해서 치는 중인 음절에 오타가 있어도 됨
        let (hay, needle) = if q.has_hangul {
            (&self.keystroke, &q.keys)
        } else {
            (&self.compact, &q.compact)
        };
        let max = if q.all_consonants {
            0
        } else {
            typo_budget(needle.len(), q.has_hangul)
        };
        if max > 0 {
            let mut rows = [Vec::new(), Vec::new(), Vec::new()];
            for (song, b) in best.iter_mut().enumerate() {
                if !(0..TYPO).contains(b) {
                    continue; // 이미 더 좋은 매치가 있거나 허용 안 된 곡
                }
                let (a, z) = hay.song_keys[song];
                for key in &hay.keys[a as usize..z as usize] {
                    if !matches!(key.kind, Kind::Name | Kind::Alias | Kind::User) {
                        continue;
                    }
                    let text = &hay.text[key.start as usize..(key.start + key.len) as usize];
                    if let Some(d) = prefix_typos(needle, text, max, &mut rows) {
                        let alias = if matches!(key.kind, Kind::Alias | Kind::User) { 10 } else { 0 };
                        *b = (*b).max(TYPO - TYPO_EDIT * d as i32 - alias);
                    }
                }
            }
        }

        // 6. 순서만 맞는 부분열로 퍼지 검색 (자리가 남을 때만)
        let found = best.iter().filter(|&&s| s > 0).count();
        if found < limit {
            // 여기선 한글을 음절 단위로 맞춤
            // 자모는 ~40개뿐이라 타자 자모 부분열은 너무 많이 걸림
            let (hay, needle, min_len) = if q.all_consonants {
                (&self.chosung, &q.keys, 3)
            } else if q.has_hangul {
                (&self.compact, &q.compact, 2)
            } else {
                (&self.compact, &q.compact, 3)
            };
            let nl = needle.len();
            if nl >= min_len {
                for (song, b) in best.iter_mut().enumerate() {
                    if *b != 0 {
                        continue; // 이미 맞았거나 허용 안 된 곡
                    }
                    let (a, z) = hay.song_keys[song];
                    for key in &hay.keys[a as usize..z as usize] {
                        if !matches!(key.kind, Kind::Name | Kind::Alias | Kind::User) || (key.len as usize) < nl
                        {
                            continue;
                        }
                        if let Some((off, span)) = hay.subsequence(key, needle) {
                            if span > nl * 2 + 1 {
                                continue;
                            }
                            let gap = (span - nl) as i32;
                            let mut s = FUZZY_MAX - 5 * gap - off_penalty(off);
                            if key.kind == Kind::Name {
                                s += 10;
                            }
                            *b = (*b).max(s.max(FUZZY_MIN));
                        }
                    }
                }
            }
        }

        // 점수 높은 순으로 모음, 동점이면 이름 짧은 것, 다음은 이름순
        let mut hits: Vec<Hit> = best
            .iter()
            .enumerate()
            .filter(|(_, &s)| s > 0)
            .map(|(index, &score)| Hit { index, score })
            .collect();
        let rank = &self.rank;
        let cmp = |a: &Hit, b: &Hit| {
            b.score
                .cmp(&a.score)
                .then_with(|| rank[a.index].cmp(&rank[b.index]))
        };
        if hits.len() > limit {
            hits.select_nth_unstable_by(limit - 1, cmp);
            hits.truncate(limit);
        }
        hits.sort_unstable_by(cmp);
        hits
    }
}
