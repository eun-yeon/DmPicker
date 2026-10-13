//! 새 exe 버전 확인과 릴리스 페이지 주소

use std::fmt;

use crate::{Error, Result, EXE_RELEASE_BASE};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExeRelease {
    tag: String,
    version: Version,
}

impl ExeRelease {
    /// 태그가 릴리스 페이지 주소에 들어가서 정식 버전 꼴만 받음
    pub fn from_tag(tag: &str) -> Result<Self> {
        Ok(Self {
            tag: tag.to_owned(),
            version: Version::parse(tag)?,
        })
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// 검사한 태그로만 만든 릴리스 페이지 주소
    pub fn page_url(&self) -> String {
        format!("{EXE_RELEASE_BASE}{}", self.tag)
    }
}

/// 첫 302의 정확한 릴리스 경로에서만 태그 추출
pub fn parse_exe_location(location: &str) -> Result<ExeRelease> {
    let tag = location
        .strip_prefix(EXE_RELEASE_BASE)
        .ok_or_else(|| Error::invalid("invalid executable release location"))?;
    ExeRelease::from_tag(tag)
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
