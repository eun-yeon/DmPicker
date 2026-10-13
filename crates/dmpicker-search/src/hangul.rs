//! 한글 도우미: 음절을 2벌식 *타자* 자모로 분해, 초성 추출, 조합형 자모 처리,
//! IME 모드를 잘못 두고 친 검색어용 2벌식·QWERTY 키 대응
//!
//! 여기서 만드는 자모는 전부 한글 *호환* 자모 (U+3131..U+3163)
//! IME가 자음·모음 하나만 낼 때 쓰는 문자

const S_BASE: u32 = 0xAC00;
const S_COUNT: u32 = 11_172;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT; // 588

/// 초성 (인덱스 0..19)의 호환 자모
const L_COMPAT: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ',
    'ㅌ', 'ㅍ', 'ㅎ',
];

/// 종성 (인덱스 1..28)의 호환 자모
const T_COMPAT: [char; 27] = [
    'ㄱ', 'ㄲ', 'ㄳ', 'ㄴ', 'ㄵ', 'ㄶ', 'ㄷ', 'ㄹ', 'ㄺ', 'ㄻ', 'ㄼ', 'ㄽ', 'ㄾ', 'ㄿ', 'ㅀ', 'ㅁ',
    'ㅂ', 'ㅄ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
];

/// 첫 호환 모음 (ㅏ), 모음은 U+314F..=U+3163에 중성 인덱스와 같은 순서로 이어져 있음
const V_COMPAT_BASE: u32 = 0x314F;

/// 완성형 한글 음절 (가..힣)
#[inline]
pub fn is_syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// 한글 호환 자모 블록 (ㄱ..ㆎ)
#[inline]
pub fn is_compat_jamo(c: char) -> bool {
    ('\u{3131}'..='\u{318E}').contains(&c)
}

/// 현대 호환 자음 (ㄱ..ㅎ, 쌍자음·겹자음 포함)
#[inline]
pub fn is_compat_consonant(c: char) -> bool {
    ('\u{3131}'..='\u{314E}').contains(&c)
}

/// 현대 호환 모음 (ㅏ..ㅣ)
#[inline]
pub fn is_compat_vowel(c: char) -> bool {
    ('\u{314F}'..='\u{3163}').contains(&c)
}

/// 조합형 자모 블록 (U+1100..U+11FF, U+A960.., U+D7B0..)
#[inline]
pub fn is_conjoining_jamo(c: char) -> bool {
    ('\u{1100}'..='\u{11FF}').contains(&c)
        || ('\u{A960}'..='\u{A97F}').contains(&c)
        || ('\u{D7B0}'..='\u{D7FF}').contains(&c)
}

/// 한글이면 다 (음절, 두 종류 자모 모두)
#[inline]
pub fn is_hangul(c: char) -> bool {
    is_syllable(c) || is_compat_jamo(c) || is_conjoining_jamo(c)
}

/// 완성형 음절을 (초성, 중성, 종성이 있으면 종성)으로 분해, 전부 호환 자모
/// 겹모음·겹받침은 여기서 안 나눔
#[inline]
pub fn decompose_syllable(c: char) -> Option<(char, char, Option<char>)> {
    let s = (c as u32).checked_sub(S_BASE)?;
    if s >= S_COUNT {
        return None;
    }
    let l = (s / N_COUNT) as usize;
    let v = (s % N_COUNT) / T_COUNT;
    let t = (s % T_COUNT) as usize;
    let vc = char::from_u32(V_COMPAT_BASE + v)?;
    let tc = if t == 0 { None } else { Some(T_COMPAT[t - 1]) };
    Some((L_COMPAT[l], vc, tc))
}

/// 완성형 음절의 초성
#[inline]
pub fn chosung_of(c: char) -> Option<char> {
    let s = (c as u32).checked_sub(S_BASE)?;
    if s >= S_COUNT {
        return None;
    }
    Some(L_COMPAT[(s / N_COUNT) as usize])
}

/// 호환 자모를 2벌식 타자 순서로 나눔, 겹모음 (ㅘ는 ㅗㅏ 등)과 겹받침 (ㄳ은 ㄱㅅ 등)은 키 두 개
/// 쌍자음 (ㄲ ㄸ ㅃ ㅆ ㅉ)은 Shift 누른 키 하나라 그대로 둠
#[inline]
pub fn split_jamo(c: char) -> (char, Option<char>) {
    match c {
        'ㄳ' => ('ㄱ', Some('ㅅ')),
        'ㄵ' => ('ㄴ', Some('ㅈ')),
        'ㄶ' => ('ㄴ', Some('ㅎ')),
        'ㄺ' => ('ㄹ', Some('ㄱ')),
        'ㄻ' => ('ㄹ', Some('ㅁ')),
        'ㄼ' => ('ㄹ', Some('ㅂ')),
        'ㄽ' => ('ㄹ', Some('ㅅ')),
        'ㄾ' => ('ㄹ', Some('ㅌ')),
        'ㄿ' => ('ㄹ', Some('ㅍ')),
        'ㅀ' => ('ㄹ', Some('ㅎ')),
        'ㅄ' => ('ㅂ', Some('ㅅ')),
        'ㅘ' => ('ㅗ', Some('ㅏ')),
        'ㅙ' => ('ㅗ', Some('ㅐ')),
        'ㅚ' => ('ㅗ', Some('ㅣ')),
        'ㅝ' => ('ㅜ', Some('ㅓ')),
        'ㅞ' => ('ㅜ', Some('ㅔ')),
        'ㅟ' => ('ㅜ', Some('ㅣ')),
        'ㅢ' => ('ㅡ', Some('ㅣ')),
        _ => (c, None),
    }
}

/// 현대 조합형 자모 (U+1100 초성, U+1161 중성, U+11A8 종성)를 같은 호환 자모로 바꿈
pub fn conjoining_to_compat(c: char) -> Option<char> {
    let u = c as u32;
    match u {
        0x1100..=0x1112 => Some(L_COMPAT[(u - 0x1100) as usize]),
        0x1161..=0x1175 => char::from_u32(V_COMPAT_BASE + (u - 0x1161)),
        0x11A8..=0x11C2 => Some(T_COMPAT[(u - 0x11A8) as usize]),
        _ => None,
    }
}

/// 조합형 L V (T) 순서를 완성형 음절로 합침
/// `l`은 U+1100..=U+1112, `v`는 U+1161..=U+1175, `t`는 U+11A8..=U+11C2여야 함
pub fn compose_conjoining(l: char, v: char, t: Option<char>) -> Option<char> {
    let l = (l as u32).checked_sub(0x1100).filter(|&x| x < 19)?;
    let v = (v as u32).checked_sub(0x1161).filter(|&x| x < V_COUNT)?;
    let t = match t {
        None => 0,
        Some(t) => (t as u32)
            .checked_sub(0x11A7)
            .filter(|&x| (1..T_COUNT).contains(&x))?,
    };
    char::from_u32(S_BASE + l * N_COUNT + v * T_COUNT + t)
}

/// 글자 하나의 2벌식 타자 자모를 내보냄
/// 음절은 분해 (겹모음·겹받침도 나눔), 호환 자모는 나눔
/// 조합형 자모는 먼저 호환 자모로 바꾸고, 나머지 글자는 그대로
#[inline]
pub fn push_keystrokes(c: char, out: &mut impl FnMut(char)) {
    if let Some((l, v, t)) = decompose_syllable(c) {
        out(l);
        push_split(v, out);
        if let Some(t) = t {
            push_split(t, out);
        }
    } else if is_compat_jamo(c) {
        push_split(c, out);
    } else if let Some(j) = conjoining_to_compat(c) {
        push_split(j, out);
    } else {
        out(c);
    }
}

#[inline]
fn push_split(j: char, out: &mut impl FnMut(char)) {
    let (a, b) = split_jamo(j);
    out(a);
    if let Some(b) = b {
        out(b);
    }
}

#[inline]
pub(crate) fn push_chosung(c: char, out: &mut impl FnMut(char)) {
    if let Some(l) = chosung_of(c) {
        out(l);
    } else {
        // 자모는 타자 순서로 나누고 나머지는 그대로
        push_keystrokes(c, out);
    }
}

/// 표준 2벌식 배열에서 이 타자 자모를 내는 QWERTY 키 (소문자)
/// Shift로 치는 쌍자음도 같은 소문자 키로 대응
pub fn jamo_to_qwerty(j: char) -> Option<char> {
    Some(match j {
        'ㅂ' | 'ㅃ' => 'q',
        'ㅈ' | 'ㅉ' => 'w',
        'ㄷ' | 'ㄸ' => 'e',
        'ㄱ' | 'ㄲ' => 'r',
        'ㅅ' | 'ㅆ' => 't',
        'ㅛ' => 'y',
        'ㅕ' => 'u',
        'ㅑ' => 'i',
        'ㅐ' | 'ㅒ' => 'o',
        'ㅔ' | 'ㅖ' => 'p',
        'ㅁ' => 'a',
        'ㄴ' => 's',
        'ㅇ' => 'd',
        'ㄹ' => 'f',
        'ㅎ' => 'g',
        'ㅗ' => 'h',
        'ㅓ' => 'j',
        'ㅏ' => 'k',
        'ㅣ' => 'l',
        'ㅋ' => 'z',
        'ㅌ' => 'x',
        'ㅊ' => 'c',
        'ㅍ' => 'v',
        'ㅠ' => 'b',
        'ㅜ' => 'n',
        'ㅡ' => 'm',
        _ => return None,
    })
}

/// 2벌식 배열에서 QWERTY 키가 내는 타자 자모
/// 대소문자 구분, Q W E R T O P는 Shift 자모가 나옴
pub fn qwerty_to_jamo(k: char) -> Option<char> {
    Some(match k {
        'Q' => 'ㅃ',
        'W' => 'ㅉ',
        'E' => 'ㄸ',
        'R' => 'ㄲ',
        'T' => 'ㅆ',
        'O' => 'ㅒ',
        'P' => 'ㅖ',
        _ => match k.to_ascii_lowercase() {
            'q' => 'ㅂ',
            'w' => 'ㅈ',
            'e' => 'ㄷ',
            'r' => 'ㄱ',
            't' => 'ㅅ',
            'y' => 'ㅛ',
            'u' => 'ㅕ',
            'i' => 'ㅑ',
            'o' => 'ㅐ',
            'p' => 'ㅔ',
            'a' => 'ㅁ',
            's' => 'ㄴ',
            'd' => 'ㅇ',
            'f' => 'ㄹ',
            'g' => 'ㅎ',
            'h' => 'ㅗ',
            'j' => 'ㅓ',
            'k' => 'ㅏ',
            'l' => 'ㅣ',
            'z' => 'ㅋ',
            'x' => 'ㅌ',
            'c' => 'ㅊ',
            'v' => 'ㅍ',
            'b' => 'ㅠ',
            'n' => 'ㅜ',
            'm' => 'ㅡ',
            _ => return None,
        },
    })
}
