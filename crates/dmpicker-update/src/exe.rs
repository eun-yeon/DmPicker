//! 새 exe 버전 확인과 minisign 서명 검증

use std::fmt;
use std::str::FromStr;

use minisign_verify::{PublicKey, Signature};
use serde_json::Value;

use crate::{Error, Result, EXE_PUBLIC_KEY, MAX_EXE_BYTES, MAX_SIGNATURE_BYTES};

pub const EXE_FILE: &str = "DmPicker.exe";
pub const SIGNATURE_FILE: &str = "DmPicker.exe.minisig";
pub const PREPARED_EXE_FILE: &str = "DmPicker.new.exe";
pub const PREPARED_SIGNATURE_FILE: &str = "DmPicker.new.exe.minisig";
pub const PREPARED_METADATA_FILE: &str = "DmPicker.new.json";
pub const OLD_EXE_FILE: &str = "DmPicker.old.exe";
pub const RELEASE_PAGE_PREFIX: &str = crate::EXE_RELEASE_BASE;
pub const EXE_ASSET_PREFIX: &str = "https://github.com/eun-yeon/DmPicker/releases/download/";
pub const TRUSTED_COMMENT_PREFIX: &str = "app=DmPicker;version=";
pub const TRUSTED_COMMENT_SUFFIX: &str = ";target=x86_64-pc-windows-msvc";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// 정식 x.y.z만 받음, 앞의 v 하나는 떼고 앞자리 0·pre-release·build는 거부
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut pieces = text.split('.');
        let major = version_part(pieces.next())?;
        let minor = version_part(pieces.next())?;
        let patch = version_part(pieces.next())?;
        if pieces.next().is_some() {
            return Err(Error::invalid("invalid release version"));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExeRelease {
    tag: String,
    version: Version,
}

impl ExeRelease {
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// 검사한 태그로만 만든 릴리스 페이지 주소
    pub fn page_url(&self) -> String {
        format!("{RELEASE_PAGE_PREFIX}{}", self.tag)
    }

    pub fn exe_url(&self) -> String {
        format!("{EXE_ASSET_PREFIX}{}/{EXE_FILE}", self.tag)
    }

    pub fn signature_url(&self) -> String {
        format!("{EXE_ASSET_PREFIX}{}/{SIGNATURE_FILE}", self.tag)
    }
}

/// 첫 302의 정확한 릴리스 경로에서만 태그 추출
pub fn parse_exe_location(location: &str) -> Result<ExeRelease> {
    let tag = location
        .strip_prefix(RELEASE_PAGE_PREFIX)
        .ok_or_else(|| Error::invalid("invalid executable release location"))?;
    let version = Version::parse(tag)?;
    Ok(ExeRelease {
        tag: tag.to_owned(),
        version,
    })
}

pub fn is_candidate(current: Version, candidate: Version, skipped: &[Version]) -> bool {
    candidate > current && !skipped.contains(&candidate)
}

pub fn trusted_comment(version: Version) -> String {
    format!("{TRUSTED_COMMENT_PREFIX}{version}{TRUSTED_COMMENT_SUFFIX}")
}

/// 버전·앱·대상이 모두 맞는 서명 문구만 허용
pub fn validate_trusted_comment(comment: &str, version: Version) -> Result<()> {
    if comment != trusted_comment(version) {
        return Err(Error::invalid("trusted comment mismatch"));
    }
    Ok(())
}

/// 다운로드 완료 때와 종료 직전에 같은 서명 검사 사용
pub fn verify_exe(
    bytes: &[u8],
    signature: &[u8],
    version: Version,
    current: Version,
) -> Result<()> {
    if version <= current {
        return Err(Error::invalid("executable version is not newer"));
    }
    verify_signed_bytes(bytes, signature, version, EXE_PUBLIC_KEY)
}

fn verify_signed_bytes(
    bytes: &[u8],
    signature: &[u8],
    version: Version,
    public_key: &str,
) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_EXE_BYTES || signature.len() > MAX_SIGNATURE_BYTES {
        return Err(Error::invalid("executable or signature size limit"));
    }
    let text =
        std::str::from_utf8(signature).map_err(|_| Error::invalid("signature is not UTF-8"))?;
    let signature = Signature::decode(text)
        .map_err(|error| Error::invalid(format!("invalid executable signature: {error}")))?;
    let key = PublicKey::from_base64(public_key)
        .map_err(|error| Error::invalid(format!("invalid executable public key: {error}")))?;
    // 파일 서명과 trusted comment의 전역 서명을 함께 확인
    key.verify(bytes, &signature, false)
        .map_err(|error| Error::invalid(format!("executable signature failed: {error}")))?;
    validate_trusted_comment(signature.trusted_comment(), version)
}

/// 준비 파일의 버전만 기록하며 신뢰 여부는 서명으로 확인
pub fn prepared_metadata(version: Version) -> Vec<u8> {
    serde_json::json!({ "version": version.to_string() })
        .to_string()
        .into_bytes()
}

pub fn parse_prepared_metadata(bytes: &[u8]) -> Result<Version> {
    if bytes.len() > MAX_SIGNATURE_BYTES {
        return Err(Error::invalid("prepared metadata size limit"));
    }
    let value: Value = serde_json::from_slice(bytes)?;
    let version = value
        .as_object()
        .and_then(|object| object.get("version"))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("missing prepared version"))?;
    Version::parse(version)
}

fn version_part(text: Option<&str>) -> Result<u64> {
    let text = text.ok_or_else(|| Error::invalid("missing release version part"))?;
    if text.is_empty()
        || (text.len() > 1 && text.starts_with('0'))
        || !text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::invalid("invalid release version part"));
    }
    text.parse()
        .map_err(|_| Error::invalid("release version part overflow"))
}
