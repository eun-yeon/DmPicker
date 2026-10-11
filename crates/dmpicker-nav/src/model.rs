//! 목록 모델: 항목, 제목 그룹, 같은 그룹이 이어지는 구간

use std::collections::HashMap;
use std::fmt;

/// 목록 항목의 제목 그룹, 게임의 순환 그룹 순서를 따름
/// `A..Z -> RANDOM -> Kanji -> Hangul -> Symbol -> Digit -> A ...`
///
/// derive한 `Ord`도 정확히 이 순서 (Letter('A') < ... < Letter('Z') < Random < ...)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// ASCII 대문자 `'A'..='Z'`
    Letter(char),
    /// RANDOM 가짜 항목 (프리스타일만)
    Random,
    /// 한자와 가나
    Kanji,
    /// 한글 음절·자모 (전부 한 그룹)
    Hangul,
    /// 나머지 전부 (문장 부호, `#`, `!` 등)
    Symbol,
    /// ASCII·전각 숫자
    Digit,
}

impl Group {
    /// 그룹 수 (글자 26개 + RANDOM + Kanji + Hangul + Symbol + Digit)
    pub const COUNT: usize = 31;

    /// 순환 그룹 순서에서의 위치, `0..Group::COUNT`
    pub fn rank(self) -> usize {
        match self {
            Group::Letter(c) => (c as u8 - b'A') as usize,
            Group::Random => 26,
            Group::Kanji => 27,
            Group::Hangul => 28,
            Group::Symbol => 29,
            Group::Digit => 30,
        }
    }

    /// [`Group::rank`]의 역함수
    pub fn from_rank(rank: usize) -> Option<Group> {
        Some(match rank {
            0..=25 => Group::Letter((b'A' + rank as u8) as char),
            26 => Group::Random,
            27 => Group::Kanji,
            28 => Group::Hangul,
            29 => Group::Symbol,
            30 => Group::Digit,
            _ => return None,
        })
    }

    /// `c`가 글자면 그 글자 그룹 (ASCII, 대소문자 무시)
    pub fn letter(c: char) -> Option<Group> {
        c.is_ascii_alphabetic().then(|| Group::Letter(c.to_ascii_uppercase()))
    }

    /// 표시 제목의 첫 글자로 정한 그룹
    /// 빈 제목이면 `None` (제목을 인식 못 한 LOCKED 곡 같은 미확인 항목)
    /// 이런 항목은 놓인 구간의 그룹을 따름
    pub fn of_title(title: &str) -> Option<Group> {
        title.chars().next().map(Group::of_char)
    }

    /// 맨 앞 글자 하나로 정한 그룹
    pub fn of_char(c: char) -> Group {
        if c.is_ascii_alphabetic() {
            return Group::Letter(c.to_ascii_uppercase());
        }
        if c.is_ascii_digit() {
            return Group::Digit;
        }
        let u = c as u32;
        match u {
            // 전각 라틴 글자·숫자
            0xFF21..=0xFF3A => Group::Letter(char::from(b'A' + (u - 0xFF21) as u8)),
            0xFF41..=0xFF5A => Group::Letter(char::from(b'A' + (u - 0xFF41) as u8)),
            0xFF10..=0xFF19 => Group::Digit,
            // 한글 음절, 자모, 호환 자모, 자모 확장 A/B
            0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xD7B0..=0xD7FF => {
                Group::Hangul
            }
            // 히라가나, 가타카나 (음성 확장, 반각 포함), 한자 (확장 A, 호환, 확장 B.. 포함), 반복 부호
            0x3040..=0x30FF
            | 0x31F0..=0x31FF
            | 0xFF66..=0xFF9F
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x3FFFF
            | 0x3005 => Group::Kanji,
            _ => fold_latin(c).map_or(Group::Symbol, Group::Letter),
        }
    }

    /// 짧은 라벨 (`"A"`, `"RANDOM"`, `"KANJI"` 등)
    pub fn label(self) -> &'static str {
        const L: [&str; Group::COUNT] = [
            "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
            "T", "U", "V", "W", "X", "Y", "Z", "RANDOM", "KANJI", "HANGUL", "SYMBOL", "DIGIT",
        ];
        L[self.rank()]
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 기본 글자와 같이 정렬되는 악센트 라틴 글자, 기본 글자를 대문자로 돌려줌
/// "Löschen"은 L 제목, "Ébène"이라면 E 제목
fn fold_latin(c: char) -> Option<char> {
    let base = match c {
        'À'..='Å' | 'à'..='å' | 'Ā'..='ą' => 'A',
        'Ç' | 'ç' | 'Ć'..='č' => 'C',
        'Ð' | 'ð' | 'Ď'..='đ' => 'D',
        'È'..='Ë' | 'è'..='ë' | 'Ē'..='ě' => 'E',
        'Ì'..='Ï' | 'ì'..='ï' | 'Ĩ'..='ı' => 'I',
        'Ñ' | 'ñ' | 'Ń'..='ň' => 'N',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' | 'Ō'..='ő' => 'O',
        'Ù'..='Ü' | 'ù'..='ü' | 'Ũ'..='ų' => 'U',
        'Ý' | 'ý' | 'ÿ' => 'Y',
        'ß' | 'Ś'..='š' => 'S',
        _ => return None,
    };
    Some(base)
}

/// 게임의 제목 정렬이 비교하는 형태 (게임에서 읽은 목록 기준)
/// 대소문자·악센트 무시, 아포스트로피·하이픈은 건너뜀, 기타 기호, 숫자, 영문 순
fn title_key(t: &str) -> Vec<(u8, char)> {
    t.chars()
        .filter(|c| !matches!(c, '\'' | '-'))
        .map(|c| match Group::of_char(c) {
            Group::Letter(base) => (2, base),
            Group::Digit => (1, c),
            _ if c.is_alphabetic() => (2, c),
            _ => (0, c),
        })
        .collect()
}

/// 제목 순서에서의 자리, 그룹 (영문, 한자·가나, 한글, 기호, 숫자 순) 다음 제목 글자 (`title_key`)로 비교
pub type OrderKey = (Group, Vec<(u8, char)>);

/// 제목의 [`OrderKey`], 빈 제목은 None
pub fn order_key(t: &str) -> Option<OrderKey> {
    Some((Group::of_title(t)?, title_key(t)))
}

/// 학습한 게임 내 목록의 한 줄
///
/// `song == None && !locked`는 RANDOM 항목, id를 모르는 LOCKED 숨김 곡은 `song == None && locked`
/// 빈 `title`은 제목 모름, 둘러싼 구간의 그룹을 따르고 이상 항목으로 안 잡음
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub song: Option<u32>,
    pub title: String,
    pub locked: bool,
}

impl Entry {
    pub fn song(id: u32, title: impl Into<String>) -> Self {
        Entry { song: Some(id), title: title.into(), locked: false }
    }
    pub fn random() -> Self {
        Entry { song: None, title: "RANDOM".into(), locked: false }
    }
    /// id·제목을 모르는 LOCKED 자리 표시용 항목
    pub fn locked_unknown() -> Self {
        Entry { song: None, title: String::new(), locked: true }
    }
    pub fn is_random(&self) -> bool {
        self.song.is_none() && !self.locked
    }
}

/// 같은 그룹이 이어지는 최대 구간
/// 목록이 순환이라 `start + len`이 끝을 넘어 처음으로 돌아갈 수 있음
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupRun {
    pub group: Group,
    pub start: usize,
    pub len: usize,
}

impl GroupRun {
    pub fn last(&self, n: usize) -> usize {
        (self.start + self.len - 1) % n
    }
    /// `pos`가 구간 안이면 구간 안에서의 위치
    pub fn offset_of(&self, pos: usize, n: usize) -> Option<usize> {
        let off = (pos + n - self.start) % n;
        (off < self.len).then_some(off)
    }
}

/// 제목으로 정한 그룹이 놓인 구간과 다른 항목
/// 관찰한 순서를 따름 ([`ListModel::from_entries`])
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anomaly {
    pub index: usize,
    pub title: String,
    pub title_group: Group,
    pub assigned: Group,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    Empty,
    TooLarge(usize),
    MultipleRandom(Vec<usize>),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Empty => write!(f, "empty list"),
            ModelError::TooLarge(n) => write!(f, "list too large ({n} entries, max 65535)"),
            ModelError::MultipleRandom(v) => write!(f, "more than one RANDOM entry at {v:?}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// 학습한 순환 곡 목록과 그룹 구조
#[derive(Clone, Debug)]
pub struct ListModel {
    entries: Vec<Entry>,
    assigned: Vec<Group>,
    run_of: Vec<u8>,
    /// 순환 그룹 순서대로 놓인 구간, 있는 그룹 중 가장 앞 그룹부터
    runs: Vec<GroupRun>,
    letter_run: [Option<u8>; 26],
    anomalies: Vec<Anomaly>,
    random_pos: Option<usize>,
    reset_pos: usize,
}

const INF: u32 = u32::MAX / 4;

impl ListModel {
    /// 게임에서 본 목록으로 모델을 만듦 (순환 목록이라 인덱스 0은 어디든 됨)
    ///
    /// **그룹 규칙 (관찰한 순서 우선)**
    /// 각 항목은 제목 첫 글자로 임시 그룹을 받음 ([`Group::of_char`])
    /// RANDOM은 `song == None && !locked`로 알아보고, 빈 제목은 아무 그룹이나 될 수 있음
    /// 최종 그룹은 아래를 만족하는 배정
    ///  1. 모든 그룹이 연속 구간 하나이고, 구간이 정해진 순환 순서
    ///     `A..Z, RANDOM, Kanji, Hangul, Symbol, Digit`대로 나옴 (없는 그룹은 건너뜀)
    ///  2. 그중 임시 그룹을 바꾸는 항목이 가장 적음
    ///     (구간별 다수결, 모든 자르는 점에 대해 DP로 정확히 풂)
    ///
    /// 동점 (경계에 딱 걸친 튀는 항목)은 *앞* 구간으로 감
    /// 임시 그룹이 뒤집힌 항목은 전부 [`ListModel::anomalies`]에 들어감, 깨끗한 목록이면 비어 있음
    pub fn from_entries(entries: Vec<Entry>) -> Result<ListModel, ModelError> {
        let n = entries.len();
        if n == 0 {
            return Err(ModelError::Empty);
        }
        if n > u16::MAX as usize {
            return Err(ModelError::TooLarge(n));
        }
        let randoms: Vec<usize> = (0..n).filter(|&i| entries[i].is_random()).collect();
        if randoms.len() > 1 {
            return Err(ModelError::MultipleRandom(randoms));
        }
        let random_pos = randoms.first().copied();

        let cls: Vec<Option<Group>> = entries
            .iter()
            .map(|e| if e.is_random() { Some(Group::Random) } else { Group::of_title(&e.title) })
            .collect();

        let assigned = assign_groups(&entries, &cls);

        // 목록 순서대로 구간을 나눔, 그룹 경계 (순위가 다시 작아지는 곳)에서 시작
        let start = (0..n)
            .find(|&i| assigned[i].rank() < assigned[(i + n - 1) % n].rank())
            .unwrap_or(0);
        let mut runs: Vec<GroupRun> = Vec::new();
        for k in 0..n {
            let i = (start + k) % n;
            match runs.last_mut() {
                Some(r) if r.group == assigned[i] => r.len += 1,
                _ => runs.push(GroupRun { group: assigned[i], start: i, len: 1 }),
            }
        }
        debug_assert!(runs.windows(2).all(|w| w[0].group < w[1].group));

        let mut run_of = vec![0u8; n];
        let mut letter_run = [None; 26];
        for (ri, r) in runs.iter().enumerate() {
            for k in 0..r.len {
                run_of[(r.start + k) % n] = ri as u8;
            }
            if let Group::Letter(c) = r.group {
                letter_run[(c as u8 - b'A') as usize] = Some(ri as u8);
            }
        }

        let anomalies = (0..n)
            .filter_map(|i| match cls[i] {
                Some(g) if g != assigned[i] => Some(Anomaly {
                    index: i,
                    title: entries[i].title.clone(),
                    title_group: g,
                    assigned: assigned[i],
                }),
                _ => None,
            })
            .collect();

        // 목록 시작은 RANDOM 바로 다음 항목
        // RANDOM이 없으면 순환 순서에서 RANDOM 자리 다음 첫 그룹의 첫 항목 (있으면 Kanji)
        let reset_pos = match random_pos {
            Some(r) => (r + 1) % n,
            None => runs
                .iter()
                .find(|r| r.group.rank() > Group::Random.rank())
                .unwrap_or(&runs[0])
                .start,
        };

        Ok(ListModel {
            entries,
            assigned,
            run_of,
            runs,
            letter_run,
            anomalies,
            random_pos,
            reset_pos,
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn entry(&self, pos: usize) -> &Entry {
        &self.entries[pos]
    }
    /// `pos` 항목의 최종 (구간) 그룹
    pub fn group_of(&self, pos: usize) -> Group {
        self.assigned[pos]
    }
    /// 순환 그룹 순서대로 놓인 구간
    pub fn runs(&self) -> &[GroupRun] {
        &self.runs
    }
    /// `pos`가 든 구간의 [`ListModel::runs`] 인덱스
    pub fn run_index(&self, pos: usize) -> usize {
        self.run_of[pos] as usize
    }
    pub fn run_of(&self, pos: usize) -> &GroupRun {
        &self.runs[self.run_of[pos] as usize]
    }
    /// 그룹의 구간, 없으면 None
    pub fn run_for(&self, group: Group) -> Option<&GroupRun> {
        match group {
            Group::Letter(c) if c.is_ascii_uppercase() => {
                self.letter_run[(c as u8 - b'A') as usize].map(|r| &self.runs[r as usize])
            }
            g => self.runs.iter().find(|r| r.group == g),
        }
    }
    pub(crate) fn letter_run_index(&self, c: char) -> Option<usize> {
        let c = c.to_ascii_uppercase();
        if !c.is_ascii_uppercase() {
            return None;
        }
        self.letter_run[(c as u8 - b'A') as usize].map(|r| r as usize)
    }
    /// 제목으로 정한 그룹이 관찰한 순서에 밀려 바뀐 항목
    pub fn anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }
    pub fn random_pos(&self) -> Option<usize> {
        self.random_pos
    }
    /// LShift+RShift 뒤 (탭 전환 뒤도) 커서 위치
    /// 프리스타일은 RANDOM 다음 항목, 온라인은 첫 Kanji 항목
    pub fn reset_pos(&self) -> usize {
        self.reset_pos
    }
    /// 두 번 이상 나오는 곡 id와 그 위치, id순 (목록 학습 때 인식 오류)
    pub fn duplicate_songs(&self) -> Vec<(u32, Vec<usize>)> {
        let mut by_id: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(id) = e.song {
                by_id.entry(id).or_default().push(i);
            }
        }
        let mut d: Vec<(u32, Vec<usize>)> = by_id.into_iter().filter(|(_, v)| v.len() > 1).collect();
        d.sort();
        d
    }
    /// 커서 위치에 키 한 번을 적용 ([`crate::step`])
    pub fn step(&self, pos: usize, key: crate::Key) -> usize {
        crate::step(self, pos, key)
    }
}

/// 바꾸는 항목이 가장 적은 단조 순환 분할 ([`ListModel::from_entries`])
fn assign_groups(entries: &[Entry], cls: &[Option<Group>]) -> Vec<Group> {
    const G: usize = Group::COUNT;
    let n = entries.len();
    let cost = |i: usize, g: usize| -> u32 {
        let is_rand = entries[i].is_random();
        if (g == Group::Random.rank()) != is_rand {
            return INF;
        }
        match cls[i] {
            None => 0,
            Some(c) if c.rank() == g => 0,
            Some(_) => 1,
        }
    };

    // 자르는 점 후보: 분류가 다른 확정 항목 사이 경계
    // 경계에 빈 제목이 끼어 있을 수 있어서 앞 확정 항목 바로 다음 위치도 넣음
    // 최적 순환 배정은 항상 이 중 한 곳에서 자를 수 있음
    let known: Vec<usize> = (0..n).filter(|&i| cls[i].is_some()).collect();
    let mut cuts: Vec<usize> = Vec::new();
    for (k, &i) in known.iter().enumerate() {
        let prev = known[(k + known.len() - 1) % known.len()];
        if cls[prev] != cls[i] {
            cuts.push(i);
            cuts.push((prev + 1) % n);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    if cuts.is_empty() {
        cuts.push(known.first().copied().unwrap_or(0));
    }

    let mut best: Option<(u32, Vec<Group>)> = None;
    // argpre[k*G + g] = argmin_{g' <= g} dp[k][g'] (동점이면 가장 작은 g')
    let mut argpre = vec![0u8; n * G];
    let mut cur = [0u32; G];
    for &c in &cuts {
        // `c`에서 시작해 일렬로 편 목록에 DP, 그룹 순위가 줄어들면 안 됨
        let mut premin = [0u32; G];
        for k in 0..n {
            let i = (c + k) % n;
            for g in 0..G {
                let base = if k == 0 { 0 } else { premin[g] };
                cur[g] = base.saturating_add(cost(i, g)).min(INF);
            }
            let (mut bv, mut ba) = (cur[0], 0usize);
            for g in 0..G {
                if cur[g] < bv {
                    bv = cur[g];
                    ba = g;
                }
                premin[g] = bv;
                argpre[k * G + g] = ba as u8;
            }
        }
        let total = premin[G - 1];
        if best.as_ref().is_some_and(|b| b.0 <= total) {
            continue;
        }
        let mut out = vec![Group::Random; n];
        let mut g = argpre[(n - 1) * G + G - 1] as usize;
        for k in (0..n).rev() {
            out[(c + k) % n] = Group::from_rank(g).expect("rank");
            if k > 0 {
                g = argpre[(k - 1) * G + g] as usize;
            }
        }
        best = Some((total, out));
    }
    best.expect("at least one cut").1
}
