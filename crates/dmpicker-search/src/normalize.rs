//! 색인과 검색어가 같이 쓰는 compact 텍스트 정규화
//!
//! * 소문자, 전각 ASCII는 ASCII로, 흔한 라틴 악센트 제거 (ö는 o, Ø는 o, Ə는 e, ß는 ss 등)
//! * 공백·문장 부호·기호는 버리고 글자, 숫자, 한글, 한자, 가나는 남김
//! * 조합형 자모 (NFD 한글)는 음절로 합침
//!   남은 낱자모는 호환 자모로 바꿔 타자 순서로 나눔 (ㅄ은 ㅂㅅ), 낱자모 텍스트끼리 바로 비교되게

use crate::hangul;

/// 정규화한 글자 하나와 원문의 단어 경계 정보
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NormChar {
    pub c: char,
    /// 단어 시작: 구분자 뒤, camelCase 경계, 라틴·한글·한자 사이에서 문자 종류가 바뀌는 곳
    pub word_start: bool,
    /// 구분자로 나뉜 단어의 시작 (약어에 씀)
    pub sep_start: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Script {
    Latin,
    Digit,
    Hangul,
    Cjk,
    Other,
}

fn script(c: char) -> Script {
    if c.is_ascii_digit() {
        Script::Digit
    } else if c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c) {
        Script::Latin
    } else if hangul::is_hangul(c) {
        Script::Hangul
    } else if ('\u{3040}'..='\u{30FF}').contains(&c)
        || ('\u{3400}'..='\u{4DBF}').contains(&c)
        || ('\u{4E00}'..='\u{9FFF}').contains(&c)
        || ('\u{F900}'..='\u{FAFF}').contains(&c)
    {
        Script::Cjk
    } else {
        Script::Other
    }
}

/// 악센트 붙은 라틴 글자를 기본 글자로 (소문자)
fn fold_latin(c: char) -> Option<&'static str> {
    Some(match c {
        'À'..='Å' | 'à'..='å' | 'Ā'..='ą' => "a",
        'Æ' | 'æ' => "ae",
        'Ç' | 'ç' | 'Ć'..='č' => "c",
        'Ð' | 'ð' | 'Ď'..='đ' => "d",
        'È'..='Ë' | 'è'..='ë' | 'Ē'..='ě' | 'Ə' | 'ə' => "e",
        'Ĝ'..='ģ' => "g",
        'Ĥ'..='ħ' => "h",
        'Ì'..='Ï' | 'ì'..='ï' | 'Ĩ'..='ı' => "i",
        'Ĵ' | 'ĵ' => "j",
        'Ķ' | 'ķ' => "k",
        'Ĺ'..='ł' => "l",
        'Ñ' | 'ñ' | 'Ń'..='ŋ' => "n",
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' | 'Ō'..='ő' => "o",
        'Œ' | 'œ' => "oe",
        'Ŕ'..='ř' => "r",
        'Ś'..='š' => "s",
        'ß' => "ss",
        'Ţ'..='ŧ' => "t",
        'Þ' | 'þ' => "th",
        'Ù'..='Ü' | 'ù'..='ü' | 'Ũ'..='ų' => "u",
        'Ŵ' | 'ŵ' => "w",
        'Ý' | 'ý' | 'ÿ' | 'Ŷ' | 'ŷ' | 'Ÿ' => "y",
        'Ź'..='ž' => "z",
        _ => return None,
    })
}

/// 원문 글자 하나를 정규화 글자 0개 이상으로 바꿈
#[inline]
fn fold(c: char, out: &mut impl FnMut(char)) {
    // 전각 ASCII는 ASCII로
    let c = match c as u32 {
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    };
    if c.is_ascii() {
        if c.is_ascii_alphanumeric() {
            out(c.to_ascii_lowercase());
        }
        return;
    }
    if hangul::is_syllable(c) {
        out(c);
        return;
    }
    if hangul::is_compat_jamo(c) || hangul::is_conjoining_jamo(c) {
        // 채움 문자는 글자가 아님
        if matches!(c, '\u{3164}' | '\u{115F}' | '\u{1160}') {
            return;
        }
        hangul::push_keystrokes(c, out);
        return;
    }
    if let Some(s) = fold_latin(c) {
        s.chars().for_each(&mut *out);
        return;
    }
    if c.is_alphanumeric() {
        c.to_lowercase().for_each(&mut *out);
    }
}

/// `s`를 정규화해서 compact 글자 (단어 플래그 포함)를 `out`에 덧붙임
pub(crate) fn normalize_into(s: &str, out: &mut Vec<NormChar>) {
    let mut chars = s.chars().peekable();
    let mut boundary = true; // 맨 앞이거나 구분자 바로 뒤
    let mut prev_lower = false;
    let mut prev_script = Script::Other;
    while let Some(mut c) = chars.next() {
        // 조합형 L V (T) 순서 (NFD 한글)를 음절 하나로 합침
        if ('\u{1100}'..='\u{1112}').contains(&c) {
            if let Some(&v) = chars.peek() {
                if ('\u{1161}'..='\u{1175}').contains(&v) {
                    chars.next();
                    let t = chars
                        .peek()
                        .copied()
                        .filter(|t| ('\u{11A8}'..='\u{11C2}').contains(t));
                    if t.is_some() {
                        chars.next();
                    }
                    if let Some(s) = hangul::compose_conjoining(c, v, t) {
                        c = s;
                    }
                }
            }
        }
        let before = out.len();
        let upper = c.is_uppercase();
        let mut first = true;
        fold(c, &mut |o| {
            let sc = script(o);
            let script_change = sc != prev_script
                && matches!(sc, Script::Latin | Script::Hangul | Script::Cjk)
                && matches!(prev_script, Script::Latin | Script::Hangul | Script::Cjk);
            out.push(NormChar {
                c: o,
                word_start: first && (boundary || (prev_lower && upper) || script_change),
                sep_start: first && boundary,
            });
            first = false;
        });
        if out.len() == before {
            // 버린 글자는 구분자 (아포스트로피는 단어를 이음: "Don't")
            if !matches!(c, '\'' | '\u{2019}' | '`') {
                boundary = true;
            }
            prev_lower = false;
        } else {
            boundary = false;
            prev_lower = c.is_lowercase();
            prev_script = script(out[out.len() - 1].c);
        }
    }
}

/// `s`의 compact 정규형 (소문자, 공백·문장 부호 없음, 낱자모는 타자 순서로 나눔)
/// 검색 키는 이 형태로 비교함
///
/// `normalize("비상 ~Stay With Me~") == "비상staywithme"`
pub fn normalize(s: &str) -> String {
    let mut v = Vec::with_capacity(s.len());
    normalize_into(s, &mut v);
    v.iter().map(|n| n.c).collect()
}
