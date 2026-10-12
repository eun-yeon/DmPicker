//! 피드 문서의 형식과 곡·패턴 검증

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::{Error, Result, CORE_FILE, DATA_RELEASE_BASE, DYNAMIC_FILE, JACKETS_FILE};
use crate::{
    MAX_ARRAY_ITEMS, MAX_CORE_BYTES, MAX_DYNAMIC_BYTES, MAX_GZIP_BYTES, MAX_MANIFEST_BYTES,
};
use crate::{MAX_SONGS, MAX_STRING_BYTES, MAX_ZIP_BYTES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonAsset {
    pub file: String,
    pub sha256: String,
    pub bytes: usize,
    pub gz_sha256: String,
    pub gz_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JacketAsset {
    pub sha256: String,
    pub bytes: usize,
    pub set_hash: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub release_tag: String,
    pub published_at: String,
    pub source_commit: String,
    pub core: JsonAsset,
    pub dynamic: JsonAsset,
    pub jackets: JacketAsset,
}

/// 실제로 바뀐 내용만 받음, 압축 방식이나 발행 시각은 비교하지 않음
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Needs {
    pub core: bool,
    pub dynamic: bool,
    pub jackets: bool,
}

impl Needs {
    pub fn between(old: Option<&Manifest>, new: &Manifest) -> Self {
        Self {
            core: old.is_none_or(|m| m.core.sha256 != new.core.sha256),
            dynamic: old.is_none_or(|m| m.dynamic.sha256 != new.dynamic.sha256),
            jackets: old.is_none_or(|m| m.jackets.set_hash != new.jackets.set_hash),
        }
    }

    pub fn any(self) -> bool {
        self.core || self.dynamic || self.jackets
    }
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = document(bytes, MAX_MANIFEST_BYTES)?;
        let root = object(&value)?;
        let release_tag = string(field(root, "releaseTag")?)?.to_owned();
        validate_release_tag(&release_tag)?;
        let published_at = string(field(root, "publishedAt")?)?.to_owned();
        crate::time::validate_rfc3339(&published_at)?;
        let source = object(field(root, "source")?)?;
        let source_commit = string(field(source, "commit")?)?;
        if source_commit.len() != 40 || !source_commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::invalid("invalid source commit"));
        }
        let jackets = object(field(root, "jackets")?)?;
        if string(field(jackets, "file")?)? != JACKETS_FILE {
            return Err(Error::invalid("invalid jackets filename"));
        }
        let count = size(field(jackets, "count")?, MAX_SONGS)?;
        if count == 0 {
            return Err(Error::invalid("empty song catalog"));
        }
        Ok(Self {
            release_tag,
            published_at,
            source_commit: source_commit.to_ascii_lowercase(),
            core: json_asset(field(root, "core")?, CORE_FILE, MAX_CORE_BYTES)?,
            dynamic: json_asset(field(root, "dynamic")?, DYNAMIC_FILE, MAX_DYNAMIC_BYTES)?,
            jackets: JacketAsset {
                sha256: hash(field(jackets, "sha256")?)?,
                bytes: size(field(jackets, "bytes")?, MAX_ZIP_BYTES)?,
                set_hash: hash(field(jackets, "setHash")?)?,
                count,
            },
        })
    }

    /// 먼저 확인한 태그와 본문 태그 대조
    pub fn for_tag(bytes: &[u8], tag: &str) -> Result<Self> {
        validate_release_tag(tag)?;
        let manifest = Self::parse(bytes)?;
        if manifest.release_tag != tag {
            return Err(Error::invalid("manifest tag mismatch"));
        }
        Ok(manifest)
    }

    pub fn generation_name(&self) -> String {
        format!("gen-{}", self.release_tag)
    }

    /// 파일 이름은 정해진 세 가지에만 한정
    pub fn asset_url(&self, file: &str) -> Result<String> {
        validate_release_tag(&self.release_tag)?;
        if !matches!(file, CORE_FILE | DYNAMIC_FILE | JACKETS_FILE) {
            return Err(Error::invalid("unknown asset filename"));
        }
        Ok(format!("{DATA_RELEASE_BASE}{}/{file}", self.release_tag))
    }
}

fn json_asset(value: &Value, file: &str, max: usize) -> Result<JsonAsset> {
    let obj = object(value)?;
    if string(field(obj, "file")?)? != file {
        return Err(Error::invalid("invalid JSON asset filename"));
    }
    Ok(JsonAsset {
        file: file.to_owned(),
        sha256: hash(field(obj, "sha256")?)?,
        bytes: size(field(obj, "bytes")?, max)?,
        gz_sha256: hash(field(obj, "gzSha256")?)?,
        gz_bytes: size(field(obj, "gzBytes")?, MAX_GZIP_BYTES)?,
    })
}

/// URL과 폴더 이름에 쓸 발행 태그
pub fn validate_release_tag(tag: &str) -> Result<()> {
    let invalid = || Error::invalid("invalid data release tag");
    let tail = tag.strip_prefix("data-").ok_or_else(invalid)?;
    if tag.len() > MAX_STRING_BYTES {
        return Err(invalid());
    }
    let mut parts = tail.split('-');
    let stamp = parts.next().ok_or_else(invalid)?;
    let run = parts.next().ok_or_else(invalid)?;
    let attempt = parts.next().ok_or_else(invalid)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if parts.next().is_some()
        || stamp.len() != 16
        || !stamp.bytes().enumerate().all(|(i, b)| match i {
            8 => b == b'T',
            15 => b == b'Z',
            _ => b.is_ascii_digit(),
        })
        || !digits(run)
        || !digits(attempt)
    {
        return Err(invalid());
    }
    Ok(())
}

/// 첫 302의 고정 저장소·경로에서만 태그 추출
pub fn data_location(location: &str) -> Result<String> {
    let tag = location
        .strip_prefix(DATA_RELEASE_BASE)
        .and_then(|s| s.strip_suffix("/manifest.json"))
        .ok_or_else(|| Error::invalid("invalid data redirect location"))?;
    validate_release_tag(tag)?;
    Ok(tag.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreSong {
    pub id: u32,
    pub name: String,
    pub composer: String,
    pub dlc_code: String,
    pub dlc: String,
    pub bpm: String,
    pub genres: Vec<String>,
    pub names: Vec<String>,
    pub segments: Vec<String>,
    pub composer_keys: Vec<String>,
    pub tags: Vec<String>,
    pub jacket: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Core {
    pub songs: Vec<CoreSong>,
}

impl Core {
    /// 곡은 id 오름차순, 중복 없음
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = document(bytes, MAX_CORE_BYTES)?;
        let root = object(&value)?;
        let arr = array(field(root, "songs")?, MAX_SONGS)?;
        if arr.is_empty() {
            return Err(Error::invalid("empty song catalog"));
        }
        let mut songs = Vec::with_capacity(arr.len());
        let mut previous = None;
        for value in arr {
            let obj = object(value)?;
            let id = song_id(field(obj, "id")?)?;
            if previous.is_some_and(|p| p >= id) {
                return Err(Error::invalid("song ids must be unique and sorted"));
            }
            previous = Some(id);
            songs.push(CoreSong {
                id,
                name: string(field(obj, "name")?)?.to_owned(),
                composer: string(field(obj, "composer")?)?.to_owned(),
                dlc_code: string(field(obj, "dlcCode")?)?.to_owned(),
                dlc: string(field(obj, "dlc")?)?.to_owned(),
                bpm: string(field(obj, "bpm")?)?.to_owned(),
                genres: strings(field(obj, "genres")?)?,
                names: strings(field(obj, "names")?)?,
                segments: strings(field(obj, "segments")?)?,
                composer_keys: strings(field(obj, "composerKeys")?)?,
                tags: strings(field(obj, "tags")?)?,
                jacket: hash(field(obj, "jacket")?)?,
            });
        }
        Ok(Self { songs })
    }

    /// id 오름차순 `[[id,"<webp sha256>"],...]`를 공백 없이 쓴 JSON의 SHA-256
    pub fn set_hash(&self) -> String {
        let mut pairs: Vec<_> = self
            .songs
            .iter()
            .map(|s| (s.id, s.jacket.as_str()))
            .collect();
        pairs.sort_unstable_by_key(|p| p.0);
        let values: Vec<Value> = pairs
            .into_iter()
            .map(|(id, hash)| serde_json::json!([id, hash]))
            .collect();
        crate::sha256(Value::Array(values).to_string().as_bytes())
    }

    /// 특징 버전과 자켓 내용이 같으면 재사용
    pub fn feature_signature(&self, version: u32) -> String {
        let mut bytes = version.to_le_bytes().to_vec();
        bytes.extend_from_slice(self.set_hash().as_bytes());
        crate::sha256(&bytes)
    }
}

pub fn validate_set_hash(manifest: &Manifest, core: &Core) -> Result<()> {
    if manifest.jackets.count != core.songs.len() || manifest.jackets.set_hash != core.set_hash() {
        return Err(Error::invalid("jacket set mismatch"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    B4,
    B5,
    B6,
    B8,
}

impl Mode {
    pub const ALL: [Self; 4] = [Self::B4, Self::B5, Self::B6, Self::B8];

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "4B" => Ok(Self::B4),
            "5B" => Ok(Self::B5),
            "6B" => Ok(Self::B6),
            "8B" => Ok(Self::B8),
            _ => Err(Error::invalid("invalid button mode")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::B4 => "4B",
            Self::B5 => "5B",
            Self::B6 => "6B",
            Self::B8 => "8B",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Difficulty {
    NM,
    HD,
    MX,
    SC,
}

impl Difficulty {
    pub const ALL: [Self; 4] = [Self::NM, Self::HD, Self::MX, Self::SC];

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "NM" => Ok(Self::NM),
            "HD" => Ok(Self::HD),
            "MX" => Ok(Self::MX),
            "SC" => Ok(Self::SC),
            _ => Err(Error::invalid("invalid difficulty")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NM => "NM",
            Self::HD => "HD",
            Self::MX => "MX",
            Self::SC => "SC",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatternKey {
    pub id: u32,
    pub mode: Mode,
    pub difficulty: Difficulty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub level: Option<u8>,
    pub floor: Option<f64>,
    pub rating: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternInfo {
    pub mode: Mode,
    pub difficulty: Difficulty,
    pub tags: BTreeMap<String, u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternAxes {
    pub radar: Vec<String>,
    pub extra: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dynamic {
    pub patterns: BTreeMap<u32, BTreeMap<Mode, BTreeMap<Difficulty, Pattern>>>,
    pub pattern_info: BTreeMap<u32, Vec<PatternInfo>>,
    pub pattern_axes: BTreeMap<Mode, PatternAxes>,
    pub sources: BTreeMap<String, String>,
}

impl Dynamic {
    /// 패턴 객체에 level이 없으면 레벨 미정, `patternInfo`는 없어도 됨
    pub fn parse(bytes: &[u8], core: &Core) -> Result<Self> {
        let value = document(bytes, MAX_DYNAMIC_BYTES)?;
        let root = object(&value)?;
        let ids: BTreeSet<_> = core.songs.iter().map(|s| s.id).collect();
        let entries = object(field(root, "patterns")?)?;
        if entries.len() != ids.len() {
            return Err(Error::invalid("pattern song ids mismatch"));
        }
        let mut patterns = BTreeMap::new();
        for (key, value) in entries {
            let id = id_key(key)?;
            if !ids.contains(&id) {
                return Err(Error::invalid("unknown pattern song id"));
            }
            let mut modes = BTreeMap::new();
            for (mode, value) in object(value)? {
                let mode = Mode::parse(mode)?;
                let mut charts = BTreeMap::new();
                for (difficulty, value) in object(value)? {
                    let difficulty = Difficulty::parse(difficulty)?;
                    let obj = object(value)?;
                    let level = obj.get("level").map(|v| small_integer(v, 20)).transpose()?;
                    let floor = obj.get("floor").map(finite).transpose()?;
                    if floor.is_some_and(|n| !(0.0..=30.0).contains(&n)) {
                        return Err(Error::invalid("floor outside 0..30"));
                    }
                    let rating = obj.get("rating").map(finite).transpose()?;
                    charts.insert(
                        difficulty,
                        Pattern {
                            level,
                            floor,
                            rating,
                        },
                    );
                }
                modes.insert(mode, charts);
            }
            patterns.insert(id, modes);
        }
        let mut pattern_info = BTreeMap::new();
        if let Some(value) = root.get("patternInfo") {
            for (key, value) in object(value)? {
                let id = id_key(key)?;
                if !ids.contains(&id) {
                    return Err(Error::invalid("unknown pattern info song id"));
                }
                let mut infos = Vec::new();
                let mut seen = BTreeSet::new();
                for value in array(value, MAX_ARRAY_ITEMS)? {
                    let obj = object(value)?;
                    let mode = Mode::parse(string(field(obj, "buttonMode")?)?)?;
                    let difficulty = Difficulty::parse(string(field(obj, "pattern")?)?)?;
                    if !seen.insert((mode, difficulty)) {
                        return Err(Error::invalid("duplicate pattern info"));
                    }
                    let mut tags = BTreeMap::new();
                    for (name, score) in object(field(obj, "tags")?)? {
                        check_string(name)?;
                        tags.insert(name.clone(), small_integer(score, 3)?);
                    }
                    infos.push(PatternInfo {
                        mode,
                        difficulty,
                        tags,
                    });
                }
                pattern_info.insert(id, infos);
            }
        }
        let mut pattern_axes = BTreeMap::new();
        if let Some(value) = root.get("patternAxes") {
            for (mode, value) in object(value)? {
                let axes = object(value)?;
                pattern_axes.insert(
                    Mode::parse(mode)?,
                    PatternAxes {
                        radar: strings(field(axes, "radar")?)?,
                        extra: strings(field(axes, "extra")?)?,
                    },
                );
            }
        }
        let source = object(field(root, "sources")?)?;
        let mut sources = BTreeMap::new();
        for name in ["tiers", "patternInfo"] {
            if let Some(value) = source.get(name) {
                sources.insert(name.to_owned(), string(value)?.to_owned());
            }
        }
        Ok(Self {
            patterns,
            pattern_info,
            pattern_axes,
            sources,
        })
    }

    pub fn pattern(&self, key: PatternKey) -> Option<&Pattern> {
        self.patterns
            .get(&key.id)?
            .get(&key.mode)?
            .get(&key.difficulty)
    }

    pub fn pattern_keys(&self) -> BTreeSet<PatternKey> {
        self.patterns
            .iter()
            .flat_map(|(&id, modes)| {
                modes.iter().flat_map(move |(&mode, charts)| {
                    charts.keys().map(move |&difficulty| PatternKey {
                        id,
                        mode,
                        difficulty,
                    })
                })
            })
            .collect()
    }
}

/// 크기 상한과 schema 1 확인, 모르는 필드는 읽지 않고 넘어감
pub(crate) fn document(bytes: &[u8], cap: usize) -> Result<Value> {
    if bytes.len() > cap {
        return Err(Error::invalid("JSON exceeds size limit"));
    }
    let value: Value = serde_json::from_slice(bytes)?;
    let schema = field(object(&value)?, "schema")?
        .as_u64()
        .ok_or_else(|| Error::invalid("invalid schema"))?;
    if schema != 1 {
        return Err(Error::UnsupportedSchema(schema));
    }
    Ok(value)
}

pub(crate) fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Value> {
    obj.get(key)
        .ok_or_else(|| Error::invalid(format!("missing field: {key}")))
}

pub(crate) fn object(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| Error::invalid("expected object"))
}

pub(crate) fn array(value: &Value, cap: usize) -> Result<&Vec<Value>> {
    value
        .as_array()
        .filter(|v| v.len() <= cap)
        .ok_or_else(|| Error::invalid("invalid array or too many items"))
}

pub(crate) fn string(value: &Value) -> Result<&str> {
    let s = value
        .as_str()
        .ok_or_else(|| Error::invalid("expected string"))?;
    check_string(s)?;
    Ok(s)
}

/// 1 KiB 넘는 문자열과 제어 문자·방향 제어 문자 거부, ZWJ 같은 다른 서식 문자는 허용
pub fn check_string(s: &str) -> Result<()> {
    if s.len() > MAX_STRING_BYTES || s.chars().any(|c| {
        c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }) {
        return Err(Error::invalid("invalid string length or control character"));
    }
    Ok(())
}

fn strings(value: &Value) -> Result<Vec<String>> {
    array(value, MAX_ARRAY_ITEMS)?
        .iter()
        .map(|v| string(v).map(str::to_owned))
        .collect()
}

pub(crate) fn hash(value: &Value) -> Result<String> {
    let s = string(value)?;
    if !crate::valid_hash(s) {
        return Err(Error::invalid("invalid SHA-256"));
    }
    Ok(s.to_ascii_lowercase())
}

fn size(value: &Value, max: usize) -> Result<usize> {
    value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n <= max)
        .ok_or_else(|| Error::invalid("invalid size or count"))
}

pub(crate) fn song_id(value: &Value) -> Result<u32> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| Error::invalid("invalid song id"))
}

pub(crate) fn id_key(value: &str) -> Result<u32> {
    let id = value
        .parse::<u32>()
        .map_err(|_| Error::invalid("invalid song id key"))?;
    if value != id.to_string() {
        return Err(Error::invalid("noncanonical song id key"));
    }
    Ok(id)
}

fn small_integer(value: &Value, max: u8) -> Result<u8> {
    value
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .filter(|&n| n <= max)
        .ok_or_else(|| Error::invalid("integer outside range"))
}

fn finite(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| Error::invalid("expected finite number"))
}
