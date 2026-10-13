//! 마지막으로 알린 목록과 현재 목록의 차이

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use crate::model::{array, document, field, hash, object, song_id, string};
use crate::{
    Core, Difficulty, Dynamic, Error, Mode, PatternKey, Result, MAX_DYNAMIC_BYTES, MAX_SONGS,
};

/// 세대 정리와 별도로 남기는 알림 기준
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub songs: BTreeMap<u32, String>,
    pub patterns: BTreeSet<PatternKey>,
}

impl Snapshot {
    pub fn from_data(core: &Core, dynamic: &Dynamic) -> Self {
        Self {
            songs: core
                .songs
                .iter()
                .map(|s| (s.id, s.jacket.clone()))
                .collect(),
            patterns: dynamic.pattern_keys(),
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = document(bytes, MAX_DYNAMIC_BYTES)?;
        let obj = object(&value)?;
        let mut result = Self::default();
        for entry in array(field(obj, "songs")?, MAX_SONGS)? {
            let pair = array(entry, 2)?;
            let id = song_id(
                pair.first()
                    .ok_or_else(|| Error::invalid("missing song id"))?,
            )?;
            let jacket = hash(
                pair.get(1)
                    .ok_or_else(|| Error::invalid("missing jacket hash"))?,
            )?;
            if result.songs.insert(id, jacket).is_some() {
                return Err(Error::invalid("duplicate snapshot song"));
            }
        }
        for entry in array(field(obj, "patterns")?, MAX_SONGS * 16)? {
            let row = array(entry, 3)?;
            let id = song_id(
                row.first()
                    .ok_or_else(|| Error::invalid("missing pattern id"))?,
            )?;
            let mode = Mode::parse(string(
                row.get(1).ok_or_else(|| Error::invalid("missing mode"))?,
            )?)?;
            let difficulty = Difficulty::parse(string(
                row.get(2)
                    .ok_or_else(|| Error::invalid("missing difficulty"))?,
            )?)?;
            if !result.songs.contains_key(&id)
                || !result.patterns.insert(PatternKey {
                    id,
                    mode,
                    difficulty,
                })
            {
                return Err(Error::invalid("unknown or duplicate snapshot pattern"));
            }
        }
        Ok(result)
    }

    pub fn to_json(&self) -> Vec<u8> {
        let songs: Vec<_> = self
            .songs
            .iter()
            .map(|(id, hash)| json!([id, hash]))
            .collect();
        let patterns: Vec<_> = self
            .patterns
            .iter()
            .map(|p| json!([p.id, p.mode.label(), p.difficulty.label()]))
            .collect();
        json!({"schema":1,"songs":songs,"patterns":patterns})
            .to_string()
            .into_bytes()
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::parse(&crate::store::read_limited(path, MAX_DYNAMIC_BYTES)?)
    }

    /// 알림을 보여 준 뒤, 또는 기준이 없을 때 첫 기준으로 저장
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::store::atomic_write(path, &self.to_json())
    }

    pub fn changes_to(&self, current: &Self) -> Notice {
        Notice {
            new_songs: current
                .songs
                .keys()
                .filter(|id| !self.songs.contains_key(id))
                .count(),
            new_patterns: current
                .patterns
                .difference(&self.patterns)
                .filter(|key| self.songs.contains_key(&key.id))
                .count(),
            changed_jackets: current
                .songs
                .iter()
                .filter(|(id, hash)| self.songs.get(id).is_some_and(|old| old != *hash))
                .count(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    pub new_songs: usize,
    pub new_patterns: usize,
    pub changed_jackets: usize,
}

impl Notice {
    pub fn any(&self) -> bool {
        self.new_songs != 0 || self.new_patterns != 0 || self.changed_jackets != 0
    }
}

/// 직전 적용 세대와 비교하는 설정 상태 줄용 개수
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub notice: Notice,
    pub levels: usize,
    pub tiers: usize,
    pub pattern_info: usize,
}

fn pattern_tags(dynamic: &Dynamic, key: PatternKey) -> Option<&BTreeMap<String, u8>> {
    dynamic
        .pattern_info
        .get(&key.id)?
        .iter()
        .find(|info| info.mode == key.mode && info.difficulty == key.difficulty)
        .map(|info| &info.tags)
        .filter(|tags| !tags.is_empty())
}

impl Summary {
    /// `before`·`after`는 각 데이터의 `Snapshot::from_data`, 호출부가 이미 들고 있음
    pub fn between(before: &Snapshot, old: &Dynamic, after: &Snapshot, new: &Dynamic) -> Self {
        let mut summary = Self {
            notice: before.changes_to(after),
            ..Self::default()
        };
        for &key in before.patterns.intersection(&after.patterns) {
            if let (Some(a), Some(b)) = (old.pattern(key), new.pattern(key)) {
                summary.levels += usize::from(a.level != b.level);
                summary.tiers += usize::from(a.floor != b.floor || a.rating != b.rating);
                summary.pattern_info += usize::from(pattern_tags(old, key) != pattern_tags(new, key));
            }
        }
        summary
    }
}
