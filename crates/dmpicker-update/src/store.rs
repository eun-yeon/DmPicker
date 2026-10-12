//! 데이터 세대 폴더, pointer, 원자적 파일 쓰기

use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::archive::{decode_json_gzip, validate_webp};
use crate::model::{validate_release_tag, validate_set_hash, Core, Dynamic, Manifest};
use crate::{sha256, validate_bytes, Error, Result};
use crate::{
    MAX_CORE_BYTES, MAX_DYNAMIC_BYTES, MAX_GZIP_BYTES, MAX_JACKET_BYTES, MAX_MANIFEST_BYTES,
    MAX_ZIP_BYTES,
};

const LOCAL_JSON_LIMIT: usize = 64 * 1024;
const COMPLETE: &str = "COMPLETE";

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

pub fn generation_name(tag: &str) -> Result<String> {
    validate_release_tag(tag)?;
    Ok(format!("gen-{tag}"))
}

pub fn validate_generation_name(name: &str) -> Result<()> {
    let tag = name
        .strip_prefix("gen-")
        .ok_or_else(|| Error::invalid("invalid generation name"))?;
    validate_release_tag(tag)
}

pub fn read_pointer(path: &Path) -> Result<Pointer> {
    Pointer::parse(&read_limited(path, LOCAL_JSON_LIMIT)?)
}

/// 로컬 파일도 링크를 거부하고 실제 읽은 크기로 상한 검사
pub fn read_limited(path: &Path, max: usize) -> Result<Vec<u8>> {
    plain_file(path)?;
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
    plain_dir(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::invalid("missing file name"))?;
    let mut temp_name = name.to_os_string();
    temp_name.push(".tmp");
    let temp = parent.join(temp_name);
    normal_or_missing(path)?;
    if let Some(backup) = backup {
        normal_or_missing(backup)?;
    }
    write_synced(&temp, bytes)?;
    replace_file(path, &temp, backup)?;
    sync_directory(parent)?;
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    normal_or_missing(path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(path: &Path, temp: &Path, backup: Option<&Path>) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let wide = |p: &Path| -> Result<Vec<u16>> {
        let mut text: Vec<u16> = p.as_os_str().encode_wide().collect();
        if text.contains(&0) {
            return Err(Error::invalid("path contains null"));
        }
        text.push(0);
        Ok(text)
    };
    let exists = path.try_exists()?;
    let path_w = wide(path)?;
    let temp_w = wide(temp)?;
    let backup_w = backup.map(wide).transpose()?;
    // REPLACEFILE_WRITE_THROUGH는 지원 안 되는 플래그라 0
    // MoveFileExW의 0x8은 MOVEFILE_WRITE_THROUGH, 0x1은 MOVEFILE_REPLACE_EXISTING
    let result = unsafe {
        if exists && backup.is_some() {
            ReplaceFileW(
                path_w.as_ptr(),
                temp_w.as_ptr(),
                backup_w.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } else {
            MoveFileExW(temp_w.as_ptr(), path_w.as_ptr(), 0x8 | u32::from(exists))
        }
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(path: &Path, temp: &Path, backup: Option<&Path>) -> Result<()> {
    if let Some(backup) = backup.filter(|_| path.exists()) {
        let bytes = read_limited(path, LOCAL_JSON_LIMIT)?;
        atomic_write(backup, &bytes)?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

#[cfg(not(windows))]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_directory(_: &Path) -> Result<()> {
    Ok(())
}

fn is_link(metadata: &Metadata) -> bool {
    // FILE_ATTRIBUTE_REPARSE_POINT, 심볼릭 링크와 junction 모두 포함
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn plain_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(Error::invalid("expected directory without links"));
    }
    Ok(())
}

fn plain_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(Error::invalid("expected file without links"));
    }
    Ok(())
}

fn normal_or_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => Ok(()),
        Ok(_) => Err(Error::invalid("expected file without links")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug)]
pub struct Generation {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub core: Core,
    pub dynamic: Dynamic,
    pub manifest_hash: String,
}

/// 저장된 JSON과 자켓을 다시 검증, 특징 캐시는 호출부에서 확인
pub fn read_generation(path: &Path) -> Result<Generation> {
    plain_dir(path)?;
    let complete = read_limited(&path.join(COMPLETE), 64)?;
    let generation = read_data(path, false)?;
    if complete != generation.manifest_hash.as_bytes() {
        return Err(Error::invalid("generation is not complete"));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::invalid("invalid generation path"))?;
    if name != generation_name(&generation.manifest.release_tag)? {
        return Err(Error::invalid("generation tag mismatch"));
    }
    Ok(generation)
}

/// 이미 받은 정상 세대는 재사용, 불량 세대는 pointer가 가리키지 않을 때만 삭제
pub fn existing_generation(root: &Path, tag: &str) -> Result<Option<Generation>> {
    let name = generation_name(tag)?;
    let path = root.join(&name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    plain_dir(root)?;
    if let Ok(generation) = read_generation(&path) {
        return Ok(Some(generation));
    }
    let mut pointer = None;
    for file in ["pointer.json", "pointer.bak"] {
        match read_pointer(&root.join(file)) {
            Ok(value) => {
                pointer = Some(value);
                break;
            }
            Err(Error::Invalid(_)) if file == "pointer.json" => {}
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if pointer.is_some_and(|p| p.active == name || p.previous.as_deref() == Some(name.as_str())) {
        return Err(Error::invalid("cannot replace a protected generation"));
    }
    remove_entry(&path, &metadata)?;
    Ok(None)
}

/// 동봉본은 압축된 JSON과 자켓에 같은 검증 적용
pub fn read_bundled(path: &Path) -> Result<Generation> {
    read_data(path, true)
}

fn read_data(path: &Path, compressed: bool) -> Result<Generation> {
    plain_dir(path)?;
    let manifest_bytes = read_limited(&path.join("manifest.json"), MAX_MANIFEST_BYTES)?;
    let manifest = Manifest::parse(&manifest_bytes)?;
    let read_json =
        |name: &str, asset: &crate::model::JsonAsset, limit: usize| -> Result<Vec<u8>> {
            if compressed {
                // 압축을 풀면서 원문 크기·해시까지 확인함
                let bytes = read_limited(&path.join(&asset.file), MAX_GZIP_BYTES)?;
                return decode_json_gzip(&bytes, asset, limit);
            }
            let bytes = read_limited(&path.join(name), limit)?;
            validate_bytes(&bytes, &asset.sha256, asset.bytes, limit)?;
            Ok(bytes)
        };
    let core = Core::parse(&read_json("core.json", &manifest.core, MAX_CORE_BYTES)?)?;
    let dynamic = Dynamic::parse(
        &read_json("dynamic.json", &manifest.dynamic, MAX_DYNAMIC_BYTES)?,
        &core,
    )?;
    validate_set_hash(&manifest, &core)?;
    validate_jacket_files(&path.join("jackets"), &core)?;
    normal_or_missing(&path.join("features.bin"))?;
    Ok(Generation {
        path: path.into(),
        manifest,
        core,
        dynamic,
        manifest_hash: sha256(&manifest_bytes),
    })
}

fn validate_jacket_files(path: &Path, core: &Core) -> Result<()> {
    plain_dir(path)?;
    let expected: HashSet<String> = core
        .songs
        .iter()
        .map(|song| format!("{}.webp", song.id))
        .collect();
    let mut count = 0usize;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| canonical_jacket_name(name)) else {
            continue;
        };
        if !expected.contains(name) {
            return Err(Error::invalid("unexpected jacket file"));
        }
        plain_file(&entry.path())?;
        count += 1;
    }
    if count != core.songs.len() {
        return Err(Error::invalid("missing jacket file"));
    }
    let mut total = 0usize;
    for song in &core.songs {
        let bytes = read_limited(&path.join(format!("{}.webp", song.id)), MAX_JACKET_BYTES)?;
        total = total
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_ZIP_BYTES)
            .ok_or_else(|| Error::invalid("jackets exceed size limit"))?;
        if sha256(&bytes) != song.jacket {
            return Err(Error::invalid("jacket hash mismatch"));
        }
        validate_webp(&bytes)?;
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
    pub name: Option<String>,
    pub path: PathBuf,
}

impl Candidate {
    pub fn load(&self) -> Result<Generation> {
        if self.name.is_some() {
            read_generation(&self.path)
        } else {
            read_bundled(&self.path)
        }
    }
}

pub struct Selection<T> {
    pub candidate: Candidate,
    pub value: T,
}

/// pointer.json, pointer.bak, COMPLETE 시각 최신순, 동봉본 순으로 처음 열리는 세대
/// `load`에서 loading 표식을 남기고 `Candidate::load`와 준비까지 마쳐야 함
pub fn choose_generation<T>(
    root: &Path,
    bundled: &Path,
    blocked: Option<&str>,
    mut load: impl FnMut(&Candidate) -> Result<T>,
) -> Result<Selection<T>> {
    let mut names = Vec::new();
    if plain_dir(root).is_ok() {
        for file in ["pointer.json", "pointer.bak"] {
            if let Ok(pointer) = read_pointer(&root.join(file)) {
                names.push(pointer.active);
                names.extend(pointer.previous);
            }
        }
        let mut complete = Vec::new();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if validate_generation_name(&name).is_err() || plain_dir(&entry.path()).is_err() {
                    continue;
                }
                let marker = entry.path().join(COMPLETE);
                if plain_file(&marker).is_ok() {
                    if let Ok(metadata) = fs::metadata(marker) {
                        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                        complete.push((modified, name));
                    }
                }
            }
        }
        complete.sort_by(|a, b| b.cmp(a));
        names.extend(complete.into_iter().map(|(_, name)| name));
    }
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for name in names {
        if blocked == Some(name.as_str()) || !seen.insert(name.clone()) {
            continue;
        }
        candidates.push(Candidate {
            path: root.join(&name),
            name: Some(name),
        });
    }
    candidates.push(Candidate {
        name: None,
        path: bundled.into(),
    });
    let mut last_error = Error::invalid("no valid data generation");
    for candidate in candidates {
        match load(&candidate) {
            Ok(value) => return Ok(Selection { candidate, value }),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

pub struct Staging {
    root: PathBuf,
    path: PathBuf,
}

impl Staging {
    pub fn create(root: &Path, request_id: u64) -> Result<Self> {
        fs::create_dir_all(root)?;
        plain_dir(root)?;
        let path = root.join(format!("staging-{request_id}"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {
                fs::remove_dir_all(&path)?;
            }
            Ok(_) => return Err(Error::invalid("staging path is not a directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::create_dir(&path)?;
        fs::create_dir(path.join("jackets"))?;
        Ok(Self {
            root: root.into(),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_data(&self, manifest: &[u8], core: &[u8], dynamic: &[u8]) -> Result<()> {
        plain_dir(&self.path)?;
        self.write_part("manifest.json", manifest)?;
        self.write_part("core.json", core)?;
        self.write_part("dynamic.json", dynamic)?;
        Ok(())
    }

    pub fn write_jacket(&self, id: u32, bytes: &[u8]) -> Result<()> {
        plain_dir(&self.path.join("jackets"))?;
        self.write_part(&format!("jackets/{id}.webp"), bytes)
    }

    pub fn copy_jackets(&self, source: &Path, core: &Core) -> Result<()> {
        plain_dir(source)?;
        plain_dir(&source.join("jackets"))?;
        for song in &core.songs {
            let bytes = read_limited(
                &source.join("jackets").join(format!("{}.webp", song.id)),
                MAX_JACKET_BYTES,
            )?;
            self.write_jacket(song.id, &bytes)?;
        }
        Ok(())
    }

    pub fn write_features(&self, bytes: &[u8]) -> Result<()> {
        self.write_part("features.bin", bytes)
    }

    fn write_part(&self, name: &str, bytes: &[u8]) -> Result<()> {
        plain_dir(&self.path)?;
        let path = self.path.join(name);
        let part = self.path.join(format!("{name}.part"));
        normal_or_missing(&path)?;
        write_synced(&part, bytes)?;
        replace_file(&path, &part, None)?;
        Ok(())
    }

    /// 전체 검증과 호출부 준비를 마친 세대만 완료 표시 후 이동
    pub fn finish<T>(
        self,
        prepare: impl FnOnce(&Generation) -> Result<T>,
    ) -> Result<(Generation, T)> {
        plain_dir(&self.root)?;
        let mut generation = read_data(&self.path, false)?;
        let target = self
            .root
            .join(generation_name(&generation.manifest.release_tag)?);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(Error::invalid("generation already exists"));
        }
        let prepared = prepare(&generation)?;
        write_synced(
            &self.path.join(COMPLETE),
            generation.manifest_hash.as_bytes(),
        )?;
        sync_directory(&self.path)?;
        fs::rename(&self.path, &target)?;
        sync_directory(&self.root)?;
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
    plain_dir(root)?;
    let pointer = Pointer::new(&pointer.active, pointer.previous.as_deref())?;
    for name in std::iter::once(&pointer.active).chain(pointer.previous.iter()) {
        let path = root.join(name);
        plain_dir(&path)?;
        let manifest = read_limited(&path.join("manifest.json"), MAX_MANIFEST_BYTES)?;
        if read_limited(&path.join(COMPLETE), 64)? != sha256(&manifest).as_bytes() {
            return Err(Error::invalid("pointer generation is not complete"));
        }
    }
    let current = root.join("pointer.json");
    let backup = root.join("pointer.bak");
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

/// data 바로 아래 `gen-`·`staging-` 항목 중 현재 pointer의 두 세대만 남김
/// 링크는 대상에 들어가지 않고 링크만 삭제
pub fn cleanup(permit: &CleanupPermit) -> Result<usize> {
    plain_dir(&permit.root)?;
    let mut removed = 0usize;
    for entry in fs::read_dir(&permit.root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with("gen-") && !name.starts_with("staging-") {
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
        if is_link(metadata) {
            fs::remove_dir(path)?;
        } else {
            fs::remove_dir_all(path)?;
        }
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
