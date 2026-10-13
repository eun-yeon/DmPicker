//! 원격 바이트라 인덱싱 대신 `range`로만 읽음

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use flate2::bufread::GzDecoder;

use crate::model::{Core, JacketAsset, JsonAsset};
use crate::{sha256, Error, Result, MAX_GZIP_BYTES, MAX_JACKET_BYTES, MAX_ZIP_BYTES};

/// 압축본과 원문을 모두 확인한 JSON 바이트
pub fn decode_json_gzip(bytes: &[u8], asset: &JsonAsset, limit: usize) -> Result<Vec<u8>> {
    if bytes.len() > MAX_GZIP_BYTES || bytes.len() != asset.gz_bytes {
        return Err(Error::invalid("gzip size mismatch"));
    }
    if sha256(bytes) != asset.gz_sha256 {
        return Err(Error::invalid("gzip hash mismatch"));
    }
    if asset.bytes > limit {
        return Err(Error::invalid("JSON exceeds size limit"));
    }
    let mut decoder = GzDecoder::new(bytes);
    let mut output = Vec::with_capacity(asset.bytes);
    let mut buffer = [0; 8192];
    loop {
        let count = decoder.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let size = add(output.len(), count)?;
        if size > limit {
            return Err(Error::invalid("JSON exceeds size limit"));
        }
        output.extend_from_slice(range(&buffer, 0, count)?);
    }
    // EOF까지 읽어서 gzip 꼬리의 CRC와 원문 길이 검사도 끝남
    // 뒤에 남은 바이트나 두 번째 멤버는 거부
    if !decoder.into_inner().is_empty() {
        return Err(Error::invalid("trailing gzip data"));
    }
    if output.len() != asset.bytes || sha256(&output) != asset.sha256 {
        return Err(Error::invalid("JSON size or hash mismatch"));
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug)]
pub struct Jacket<'a> {
    pub id: u32,
    pub bytes: &'a [u8],
}

/// 이름·해시·헤더를 모두 검사, 하나라도 틀리면 전체 실패
/// id 순으로 반환
pub fn read_jackets<'a>(
    bytes: &'a [u8],
    asset: &JacketAsset,
    core: &Core,
) -> Result<Vec<Jacket<'a>>> {
    if bytes.len() > MAX_ZIP_BYTES || bytes.len() != asset.bytes {
        return Err(Error::invalid("zip size mismatch"));
    }
    if sha256(bytes) != asset.sha256 {
        return Err(Error::invalid("zip hash mismatch"));
    }
    let end = end_record(bytes)?;
    let eocd = range(bytes, end, 22)?;
    let count = usize::from(u16_at(eocd, 10)?);
    // 분할 zip과 ZIP64 표시(0xffff) 거부
    if u16_at(eocd, 4)? != 0
        || u16_at(eocd, 6)? != 0
        || usize::from(u16_at(eocd, 8)?) != count
        || count == usize::from(u16::MAX)
        || count != asset.count
        || count != core.songs.len()
    {
        return Err(Error::invalid("zip entry count or disk mismatch"));
    }
    let directory_size = u32_at(eocd, 12)?;
    let directory_start = u32_at(eocd, 16)?;
    if directory_size == u32::MAX || directory_start == u32::MAX {
        return Err(Error::invalid("ZIP64 is unsupported"));
    }
    let directory_size = usize32(directory_size)?;
    let directory_start = usize32(directory_start)?;
    if add(directory_start, directory_size)? != end {
        return Err(Error::invalid("invalid zip directory range"));
    }
    let mut directory = range(bytes, directory_start, directory_size)?;
    let files = range(bytes, 0, directory_start)?;
    let expected: BTreeMap<_, _> = core
        .songs
        .iter()
        .map(|song| (song.id, song.jacket.as_str()))
        .collect();
    let mut seen = BTreeSet::new();
    let mut total = 0;
    let mut jackets = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = central_entry(directory)?;
        directory = directory
            .get(entry.length..)
            .ok_or_else(|| Error::invalid("truncated zip directory"))?;
        let id = jacket_id(entry.name)?;
        let hash = expected
            .get(&id)
            .ok_or_else(|| Error::invalid("unexpected jacket id"))?;
        if !seen.insert(id) {
            return Err(Error::invalid("duplicate jacket id"));
        }
        let payload = local_entry(files, &entry)?;
        total = add(total, payload.len())?;
        if total > MAX_ZIP_BYTES {
            return Err(Error::invalid("jackets exceed size limit"));
        }
        if sha256(payload) != *hash {
            return Err(Error::invalid("jacket hash mismatch"));
        }
        validate_webp(payload)?;
        jackets.push(Jacket { id, bytes: payload });
    }
    if !directory.is_empty() {
        return Err(Error::invalid("trailing zip directory data"));
    }
    jackets.sort_unstable_by_key(|jacket| jacket.id);
    Ok(jackets)
}

/// 단일 VP8 청크와 키프레임 헤더의 128×128 크기 확인
/// VP8L·VP8X는 거부, VP8X는 디코더가 바깥 크기와 상관없이 안쪽 그림 크기만큼 메모리를 먼저 잡음
pub fn validate_webp(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_JACKET_BYTES
        || range(bytes, 0, 4)? != b"RIFF"
        || range(bytes, 8, 4)? != b"WEBP"
        || range(bytes, 12, 4)? != b"VP8 "
    {
        return Err(Error::invalid("unsupported jacket format"));
    }
    if add(usize32(u32_at(bytes, 4)?)?, 8)? != bytes.len() {
        return Err(Error::invalid("invalid RIFF length"));
    }
    let length = usize32(u32_at(bytes, 16)?)?;
    let padded = add(length, length & 1)?;
    if add(20, padded)? != bytes.len() || length < 10 {
        return Err(Error::invalid("invalid VP8 chunk length"));
    }
    let frame = range(bytes, 20, length)?;
    let first = frame
        .first()
        .ok_or_else(|| Error::invalid("missing VP8 frame tag"))?;
    if first & 1 != 0
        || range(frame, 3, 3)? != b"\x9d\x01\x2a"
        || u16_at(frame, 6)? & 0x3fff != 128
        || u16_at(frame, 8)? & 0x3fff != 128
    {
        return Err(Error::invalid("invalid VP8 frame header"));
    }
    Ok(())
}

struct CentralEntry<'a> {
    name: &'a [u8],
    size: usize,
    offset: usize,
    length: usize,
}

fn central_entry(bytes: &[u8]) -> Result<CentralEntry<'_>> {
    let header = range(bytes, 0, 46)?;
    let compressed = u32_at(header, 20)?;
    let size = u32_at(header, 24)?;
    let offset = u32_at(header, 42)?;
    // 플래그 0만 허용해서 암호화와 data descriptor를 따로 해석하지 않음
    if range(header, 0, 4)? != b"PK\x01\x02"
        || u16_at(header, 8)? != 0
        || u16_at(header, 10)? != 0
        || u16_at(header, 34)? != 0
        || size == u32::MAX
        || offset == u32::MAX
        || compressed != size
        || usize32(size)? > MAX_JACKET_BYTES
    {
        return Err(Error::invalid("unsupported zip entry"));
    }
    let name_length = usize::from(u16_at(header, 28)?);
    let extra_length = usize::from(u16_at(header, 30)?);
    let comment_length = usize::from(u16_at(header, 32)?);
    let name = range(bytes, 46, name_length)?;
    let extra_at = add(46, name_length)?;
    range(bytes, extra_at, extra_length)?;
    let length = add(add(extra_at, extra_length)?, comment_length)?;
    range(bytes, 0, length)?;
    Ok(CentralEntry {
        name,
        size: usize32(size)?,
        offset: usize32(offset)?,
        length,
    })
}

fn local_entry<'a>(bytes: &'a [u8], entry: &CentralEntry<'_>) -> Result<&'a [u8]> {
    let local = bytes
        .get(entry.offset..)
        .ok_or_else(|| Error::invalid("invalid zip local offset"))?;
    let header = range(local, 0, 30)?;
    if range(header, 0, 4)? != b"PK\x03\x04"
        || u16_at(header, 6)? != 0
        || u16_at(header, 8)? != 0
        || usize32(u32_at(header, 18)?)? != entry.size
        || usize32(u32_at(header, 22)?)? != entry.size
    {
        return Err(Error::invalid("zip header mismatch"));
    }
    let name_length = usize::from(u16_at(header, 26)?);
    let extra_length = usize::from(u16_at(header, 28)?);
    if range(local, 30, name_length)? != entry.name {
        return Err(Error::invalid("zip name mismatch"));
    }
    let extra_at = add(30, name_length)?;
    range(local, extra_at, extra_length)?;
    let data_at = add(extra_at, extra_length)?;
    range(local, data_at, entry.size)
}

fn end_record(bytes: &[u8]) -> Result<usize> {
    let last = bytes
        .len()
        .checked_sub(22)
        .ok_or_else(|| Error::invalid("truncated zip"))?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    for at in (first..=last).rev() {
        if range(bytes, at, 4)? == b"PK\x05\x06" {
            let header = range(bytes, at, 22)?;
            let comment = usize::from(u16_at(header, 20)?);
            if add(add(at, 22)?, comment)? == bytes.len() {
                return Ok(at);
            }
        }
    }
    Err(Error::invalid("missing zip end record"))
}

fn jacket_id(name: &[u8]) -> Result<u32> {
    let text = std::str::from_utf8(name).map_err(|_| Error::invalid("invalid jacket name"))?;
    let digits = text
        .strip_suffix(".webp")
        .ok_or_else(|| Error::invalid("invalid jacket name"))?;
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::invalid("invalid jacket id"));
    }
    digits
        .parse()
        .map_err(|_| Error::invalid("jacket id overflow"))
}

fn add(left: usize, right: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| Error::invalid("archive offset overflow"))
}

fn range(bytes: &[u8], start: usize, length: usize) -> Result<&[u8]> {
    bytes
        .get(start..add(start, length)?)
        .ok_or_else(|| Error::invalid("truncated archive"))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    let raw: [u8; 2] = range(bytes, at, 2)?
        .try_into()
        .map_err(|_| Error::invalid("truncated integer"))?;
    Ok(u16::from_le_bytes(raw))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let raw: [u8; 4] = range(bytes, at, 4)?
        .try_into()
        .map_err(|_| Error::invalid("truncated integer"))?;
    Ok(u32::from_le_bytes(raw))
}

fn usize32(value: u32) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::invalid("archive integer overflow"))
}
