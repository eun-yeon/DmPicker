//! 데이터 세대 폴더, pointer, 원자적 파일 쓰기

use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::archive::validate_webp;
use crate::model::{validate_release_tag, validate_set_hash, Core, Dynamic, Manifest};
use crate::{ensure_running, sha256, validate_bytes, Error, Result};
use crate::{
    MAX_CORE_BYTES, MAX_DYNAMIC_BYTES, MAX_JACKET_BYTES, MAX_MANIFEST_BYTES, MAX_ZIP_BYTES,
};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const CORE_JSON: &str = "core.json";
pub const DYNAMIC_JSON: &str = "dynamic.json";
pub const FEATURES_FILE: &str = "features.bin";
pub const JACKETS_DIR: &str = "jackets";
const POINTER_FILE: &str = "pointer.json";
const POINTER_BACKUP: &str = "pointer.bak";
const COMPLETE: &str = "COMPLETE";
const GENERATION_PREFIX: &str = "gen-";
const STAGING: &str = "staging";
const LOCAL_JSON_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointer {
    pub active: String,
    pub previous: Option<String>,
}

impl Pointer {
    pub fn new(active: &str, previous: Option<&str>) -> Result<Self> {
        validate_generation_name(active)?;
        if let Some(name) = previous {
            validate_generation_name(name)?;
        }
        Ok(Self {
            active: active.into(),
            previous: previous.filter(|name| *name != active).map(str::to_owned),
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > LOCAL_JSON_LIMIT {
            return Err(Error::invalid("pointer exceeds size limit"));
        }
        let value: Value = serde_json::from_slice(bytes)?;
        let active = value
            .get("active")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("missing active generation"))?;
        let previous = match value.get("previous") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(name.as_str()),
            _ => return Err(Error::invalid("invalid previous generation")),
        };
        Self::new(active, previous)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&json!({
            "active": self.active,
            "previous": self.previous,
        }))?)
    }
}

/// 태그는 manifest를 읽을 때나 호출부에서 이미 검사됨
pub fn generation_name(tag: &str) -> String {
    format!("{GENERATION_PREFIX}{tag}")
}

pub(crate) fn validate_generation_name(name: &str) -> Result<()> {
    let tag = name
        .strip_prefix(GENERATION_PREFIX)
        .ok_or_else(|| Error::invalid("invalid generation name"))?;
    validate_release_tag(tag)
}

pub fn read_pointer(path: &Path) -> Result<Pointer> {
    Pointer::parse(&read_limited(path, LOCAL_JSON_LIMIT)?)
}

/// pointer.json, pointer.bak 순서로 읽히는 것만
pub fn read_pointers(root: &Path) -> Vec<Pointer> {
    [POINTER_FILE, POINTER_BACKUP]
        .into_iter()
        .filter_map(|file| read_pointer(&root.join(file)).ok())
        .collect()
}

fn jacket_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(JACKETS_DIR).join(format!("{id}.webp"))
}

/// 크기 상한과 webp 헤더 확인, `hash`가 있으면 내용도 대조
pub fn read_jacket(dir: &Path, id: u32, hash: Option<&str>) -> Result<Vec<u8>> {
    let bytes = read_limited(&jacket_path(dir, id), MAX_JACKET_BYTES)?;
    if hash.is_some_and(|hash| sha256(&bytes) != hash) {
        return Err(Error::invalid("jacket hash mismatch"));
    }
    validate_webp(&bytes)?;
    Ok(bytes)
}

/// 실제 읽은 크기로 상한 검사
pub fn read_limited(path: &Path, max: usize) -> Result<Vec<u8>> {
    let limit = u64::try_from(max)
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| Error::invalid("file size limit overflow"))?;
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::invalid("file exceeds size limit"));
    }
    Ok(bytes)
}

/// 같은 폴더의 임시 파일을 내린 뒤 원자적으로 교체
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_inner(path, bytes, None)
}

fn atomic_write_inner(path: &Path, bytes: &[u8], backup: Option<&Path>) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::invalid("missing parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::invalid("missing file name"))?;
    let mut temp_name = name.to_os_string();
    temp_name.push(".tmp");
    let temp = parent.join(temp_name);
    write_synced(&temp, bytes)?;
    if let Some(backup) = backup {
        atomic_write(backup, &read_limited(path, LOCAL_JSON_LIMIT)?)?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug)]
pub struct Generation {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub core: Core,
    pub dynamic: Dynamic,
    pub manifest_hash: String,
}

/// 저장된 JSON과 재킷을 다시 검증, 특징 캐시는 호출부에서 확인
pub fn read_generation(path: &Path) -> Result<Generation> {
    let complete = read_limited(&path.join(COMPLETE), 64)?;
    let generation = read_data(path)?;
    if complete != generation.manifest_hash.as_bytes() {
        return Err(Error::invalid("generation is not complete"));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::invalid("invalid generation path"))?;
    if name != generation_name(&generation.manifest.release_tag) {
        return Err(Error::invalid("generation tag mismatch"));
    }
    Ok(generation)
}

/// 이미 받은 정상 세대는 재사용, 불량 세대는 지움
/// 지금 쓰는 세대는 화면이 그 폴더의 재킷을 읽는 중이라 지우지 않고 오류
pub fn existing_generation(
    root: &Path,
    tag: &str,
    active: Option<&str>,
) -> Result<Option<Generation>> {
    validate_release_tag(tag)?;
    let name = generation_name(tag);
    let path = root.join(&name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if let Ok(generation) = read_generation(&path) {
        return Ok(Some(generation));
    }
    if active == Some(name.as_str()) {
        return Err(Error::invalid("cannot replace a protected generation"));
    }
    remove_entry(&path, &metadata)?;
    Ok(None)
}

fn read_data(path: &Path) -> Result<Generation> {
    let manifest_bytes = read_limited(&path.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
    let manifest = Manifest::parse(&manifest_bytes)?;
    let read_json =
        |name: &str, asset: &crate::model::JsonAsset, limit: usize| -> Result<Vec<u8>> {
            let bytes = read_limited(&path.join(name), limit)?;
            validate_bytes(&bytes, &asset.sha256, asset.bytes)?;
            Ok(bytes)
        };
    let core = Core::parse(&read_json(CORE_JSON, &manifest.core, MAX_CORE_BYTES)?)?;
    let dynamic = Dynamic::parse(
        &read_json(DYNAMIC_JSON, &manifest.dynamic, MAX_DYNAMIC_BYTES)?,
        &core,
    )?;
    validate_set_hash(&manifest, &core)?;
    validate_jacket_files(path, &core)?;
    Ok(Generation {
        path: path.into(),
        manifest,
        core,
        dynamic,
        manifest_hash: sha256(&manifest_bytes),
    })
}

fn validate_jacket_files(dir: &Path, core: &Core) -> Result<()> {
    let expected: HashSet<String> = core
        .songs
        .iter()
        .map(|song| format!("{}.webp", song.id))
        .collect();
    let mut count = 0usize;
    for entry in fs::read_dir(dir.join(JACKETS_DIR))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| canonical_jacket_name(name)) else {
            continue;
        };
        if !expected.contains(name) {
            return Err(Error::invalid("unexpected jacket file"));
        }
        count += 1;
    }
    if count != core.songs.len() {
        return Err(Error::invalid("missing jacket file"));
    }
    let mut total = 0usize;
    for song in &core.songs {
        let bytes = read_jacket(dir, song.id, Some(&song.jacket))?;
        total = total
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_ZIP_BYTES)
            .ok_or_else(|| Error::invalid("jackets exceed size limit"))?;
    }
    Ok(())
}

fn canonical_jacket_name(name: &str) -> bool {
    name.strip_suffix(".webp").is_some_and(|id| {
        !id.is_empty()
            && (id == "0" || !id.starts_with('0'))
            && id.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub path: PathBuf,
}

impl Candidate {
    pub fn load(&self) -> Result<Generation> {
        read_generation(&self.path)
    }
}

/// pointer.json, pointer.bak, COMPLETE 시각 최신순으로 처음 열리는 세대
/// 열 수 있는 세대가 없으면 `None`
/// 중단됐던 세대는 다른 후보를 모두 실패한 뒤 시도
/// `load`에서 loading 표식을 남기고 `Candidate::load`와 준비까지 마쳐야 함
pub fn choose_generation<T>(
    root: &Path,
    blocked: Option<&str>,
    mut load: impl FnMut(&Candidate) -> Result<T>,
) -> Option<T> {
    let mut names = Vec::new();
    if root.is_dir() {
        for pointer in read_pointers(root) {
            names.push(pointer.active);
            names.extend(pointer.previous);
        }
        let mut complete = Vec::new();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if validate_generation_name(&name).is_err() || !entry.path().is_dir() {
                    continue;
                }
                let marker = entry.path().join(COMPLETE);
                if let Ok(metadata) = fs::metadata(marker) {
                    if metadata.is_file() {
                        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                        complete.push((modified, name));
                    }
                }
            }
        }
        complete.sort_by(|a, b| b.cmp(a));
        names.extend(complete.into_iter().map(|(_, name)| name));
    }
    names.sort_by_key(|name| blocked == Some(name.as_str()));
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.clone()) {
            continue;
        }
        let candidate = Candidate {
            path: root.join(&name),
            name,
        };
        if let Ok(value) = load(&candidate) {
            return Some(value);
        }
    }
    None
}

pub struct Staging {
    root: PathBuf,
    path: PathBuf,
}

impl Staging {
    pub fn create(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let path = root.join(STAGING);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => remove_entry(&path, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::create_dir(&path)?;
        fs::create_dir(path.join(JACKETS_DIR))?;
        Ok(Self {
            root: root.into(),
            path,
        })
    }

    pub fn write_data(&self, manifest: &[u8], core: &[u8], dynamic: &[u8]) -> Result<()> {
        fs::write(self.path.join(MANIFEST_FILE), manifest)?;
        fs::write(self.path.join(CORE_JSON), core)?;
        fs::write(self.path.join(DYNAMIC_JSON), dynamic)?;
        Ok(())
    }

    pub fn write_jacket(&self, id: u32, bytes: &[u8]) -> Result<()> {
        fs::write(jacket_path(&self.path, id), bytes)?;
        Ok(())
    }

    /// 바뀌지 않은 재킷을 `source` 세대에서 복사, 내용 검사는 `finish`에서 한 번에 함
    pub fn copy_jackets(&self, source: &Path, core: &Core, cancel: &AtomicBool) -> Result<()> {
        for song in &core.songs {
            ensure_running(cancel)?;
            let bytes = read_limited(&jacket_path(source, song.id), MAX_JACKET_BYTES)?;
            self.write_jacket(song.id, &bytes)?;
        }
        Ok(())
    }

    /// 전체 검증과 호출부 준비를 마친 세대만 완료 표시 후 이동
    pub fn finish<T>(
        self,
        prepare: impl FnOnce(&Generation) -> Result<T>,
    ) -> Result<(Generation, T)> {
        let mut generation = read_data(&self.path)?;
        let target = self
            .root
            .join(generation_name(&generation.manifest.release_tag));
        if fs::symlink_metadata(&target).is_ok() {
            return Err(Error::invalid("generation already exists"));
        }
        let prepared = prepare(&generation)?;
        fs::write(
            self.path.join(COMPLETE),
            generation.manifest_hash.as_bytes(),
        )?;
        fs::rename(&self.path, &target)?;
        generation.path = target;
        Ok((generation, prepared))
    }
}

pub struct CleanupPermit {
    root: PathBuf,
    pointer: Pointer,
}

/// 검증된 세대를 가리키는 pointer를 쓴 뒤에만 정리 권한 반환
pub fn write_pointer(root: &Path, pointer: &Pointer) -> Result<CleanupPermit> {
    let pointer = Pointer::new(&pointer.active, pointer.previous.as_deref())?;
    for name in std::iter::once(&pointer.active).chain(pointer.previous.iter()) {
        let path = root.join(name);
        let manifest = read_limited(&path.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        if read_limited(&path.join(COMPLETE), 64)? != sha256(&manifest).as_bytes() {
            return Err(Error::invalid("pointer generation is not complete"));
        }
    }
    let current = root.join(POINTER_FILE);
    let backup = root.join(POINTER_BACKUP);
    // 깨진 pointer.json으로 정상 pointer.bak을 덮지 않음
    let keep_backup = read_pointer(&current).is_ok();
    atomic_write_inner(
        &current,
        &pointer.to_bytes()?,
        keep_backup.then_some(backup.as_path()),
    )?;
    Ok(CleanupPermit {
        root: root.into(),
        pointer,
    })
}

/// data 바로 아래 `gen-`·`staging` 항목 중 현재 pointer의 두 세대만 남김
/// 링크는 대상에 들어가지 않고 링크만 삭제
pub fn cleanup(permit: &CleanupPermit) -> Result<usize> {
    let mut removed = 0usize;
    for entry in fs::read_dir(&permit.root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(GENERATION_PREFIX) && !name.starts_with(STAGING) {
            continue;
        }
        if name == permit.pointer.active || permit.pointer.previous.as_ref() == Some(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        remove_entry(&path, &metadata)?;
        removed += 1;
    }
    Ok(removed)
}

fn remove_entry(path: &Path, metadata: &Metadata) -> Result<()> {
    // junction은 is_dir() 대신 디렉터리 속성으로 구분
    #[cfg(windows)]
    let directory = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x10 != 0
    };
    #[cfg(not(windows))]
    let directory = metadata.is_dir();
    if directory {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
