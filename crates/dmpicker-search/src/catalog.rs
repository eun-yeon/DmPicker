//! 받은 곡 데이터(core·dynamic)로 만든 검색용 곡 목록


/// 버튼 모드
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    B4,
    B5,
    B6,
    B8,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::B4, Mode::B5, Mode::B6, Mode::B8];

    /// `Song::levels` 인덱스 (0..4)
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// djmax.gg·게임 내 라벨: "4B", "5B", "6B", "8B"
    pub fn label(self) -> &'static str {
        match self {
            Mode::B4 => "4B",
            Mode::B5 => "5B",
            Mode::B6 => "6B",
            Mode::B8 => "8B",
        }
    }

    /// "4B" / "4b" / "4" 파싱
    pub fn from_label(s: &str) -> Option<Mode> {
        match s.trim().trim_end_matches(['B', 'b']) {
            "4" => Some(Mode::B4),
            "5" => Some(Mode::B5),
            "6" => Some(Mode::B6),
            "8" => Some(Mode::B8),
            _ => None,
        }
    }
}

/// 난이도 칸
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Diff {
    NM,
    HD,
    MX,
    SC,
}

impl Diff {
    pub const ALL: [Diff; 4] = [Diff::NM, Diff::HD, Diff::MX, Diff::SC];

    /// `Song::levels[mode]` 인덱스 (0..4)
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// 라벨 "NM", "HD", "MX", "SC"
    pub fn label(self) -> &'static str {
        match self {
            Diff::NM => "NM",
            Diff::HD => "HD",
            Diff::MX => "MX",
            Diff::SC => "SC",
        }
    }

    /// "NM"/"HD"/"MX"/"SC" 파싱 (대소문자 무시)
    pub fn from_label(s: &str) -> Option<Diff> {
        match s.trim().to_ascii_uppercase().as_str() {
            "NM" => Some(Diff::NM),
            "HD" => Some(Diff::HD),
            "MX" => Some(Diff::MX),
            "SC" => Some(Diff::SC),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Song {
    /// djmax.gg 숫자 곡 id (core.json `id`, 재킷 파일 이름과 같음)
    pub id: u32,
    pub name: String,
    pub composer: String,
    pub dlc: String,
    pub dlc_code: String,
    pub bpm: String,
    /// `bpm`의 최저·최고 BPM, 고정이면 둘이 같고 숫자가 아니면 None
    pub bpm_range: Option<(f32, f32)>,
    pub genres: Vec<String>,
    /// djmax.gg 검색 별칭 (`searchDoc.segments`), 이미 compact 형태
    pub segments: Vec<String>,
    /// djmax.gg 작곡가 별칭 (`searchDoc.composerKeys`)
    pub composer_keys: Vec<String>,
    /// segment 중 djmax.gg 태그 (`searchDoc.tags`: 팩 이름, 장르)
    /// 곡이 속한 그룹이지 곡 자체의 별칭은 아님, 같이 쓰는 곡이 적어도 마찬가지
    pub tags: Vec<String>,
    /// djmax.gg의 곡 자체 이름 (`searchDoc.names`: 읽는 법, 별명)
    /// 같이 쓰는 곡이 많아도 항상 곡 자체의 별칭 (DIE IN과 아티스트 DIEIN 둘 다 "다인")
    pub names: Vec<String>,
    /// 내 키워드, djmax.gg 데이터엔 없고 앱이 따로 읽어 채움
    pub user_keys: Vec<String>,
    /// 패턴이 있으면 `[mode][diff]` 레벨
    /// `Some(0)`은 패턴은 있는데 레벨이 미정
    pub levels: [[Option<u8>; 4]; 4],
    /// 서열표 난이도 `[mode][diff]` (djmax.gg `floor`, 소수 한 자리), 모든 SC와 일부 NM·HD·MX에 있음
    pub floors: [[Option<f32>; 4]; 4],
}

impl Song {
    #[inline]
    pub fn level(&self, mode: Mode, diff: Diff) -> Option<u8> {
        self.levels[mode.index()][diff.index()]
    }

    #[inline]
    pub fn floor(&self, mode: Mode, diff: Diff) -> Option<f32> {
        self.floors[mode.index()][diff.index()]
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub songs: Vec<Song>,
}

/// "142"나 변속곡의 "116~155"에서 최저·최고 BPM
fn bpm_range(bpm: &str) -> Option<(f32, f32)> {
    let mut range: Option<(f32, f32)> = None;
    for part in bpm.split('~') {
        let b = part.trim().parse::<f32>().ok().filter(|b| b.is_finite() && *b > 0.0)?;
        range = Some(range.map_or((b, b), |(lo, hi)| (lo.min(b), hi.max(b))));
    }
    range
}

impl Catalog {
    /// 검증된 곡 정보와 패턴을 검색용 곡으로 변환
    pub fn from_feed(
        core: &dmpicker_update::Core,
        dynamic: &dmpicker_update::Dynamic,
    ) -> Result<Self, String> {
        use dmpicker_update::{Difficulty, Mode as FeedMode, PatternKey};
        if core.songs.len() != dynamic.patterns.len()
            || core
                .songs
                .iter()
                .any(|s| !dynamic.patterns.contains_key(&s.id))
        {
            return Err("feed song ids mismatch".into());
        }
        let strings = |values: &[String]| {
            values
                .iter()
                .filter(|s| !s.is_empty() && s.as_str() != "$undefined")
                .cloned()
                .collect()
        };
        let songs = core
            .songs
            .iter()
            .map(|s| {
                if s.name.trim().is_empty() || s.name == "$undefined" {
                    return Err(format!("id {}: missing song name", s.id));
                }
                let mut levels = [[None; 4]; 4];
                let mut floors = [[None; 4]; 4];
                for ((levels, floors), mode) in
                    levels.iter_mut().zip(&mut floors).zip(FeedMode::ALL)
                {
                    for ((level, floor), difficulty) in
                        levels.iter_mut().zip(floors).zip(Difficulty::ALL)
                    {
                        if let Some(pattern) = dynamic.pattern(PatternKey {
                            id: s.id,
                            mode,
                            difficulty,
                        }) {
                            // 레벨 미정 패턴도 고를 수 있게 칸은 남김
                            *level = Some(pattern.level.unwrap_or(0));
                            *floor = pattern.floor.map(|f| f as f32).filter(|f| *f > 0.0);
                        }
                    }
                }
                Ok(Song {
                    id: s.id,
                    name: s.name.clone(),
                    composer: s.composer.clone(),
                    dlc: s.dlc.clone(),
                    dlc_code: s.dlc_code.clone(),
                    bpm: s.bpm.clone(),
                    bpm_range: bpm_range(&s.bpm),
                    genres: strings(&s.genres),
                    segments: strings(&s.segments),
                    composer_keys: strings(&s.composer_keys),
                    tags: strings(&s.tags),
                    names: strings(&s.names),
                    user_keys: Vec::new(),
                    levels,
                    floors,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self { songs })
    }

    /// 이 id인 곡의 `songs` 안 위치
    pub fn index_of(&self, id: u32) -> Option<usize> {
        // djmax.gg 데이터는 id가 0..n으로 빈틈없어서 그 칸부터 확인
        match self.songs.get(id as usize) {
            Some(s) if s.id == id => Some(id as usize),
            _ => self.songs.iter().position(|s| s.id == id),
        }
    }

    pub fn by_id(&self, id: u32) -> Option<&Song> {
        self.index_of(id).map(|i| &self.songs[i])
    }

    pub fn len(&self) -> usize {
        self.songs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }
}
