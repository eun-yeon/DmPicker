//! 곡 데이터 검증과 로컬 업데이트 상태

pub mod archive;
pub mod exe;
pub mod model;
pub mod notify;
pub mod state;
pub mod store;
pub mod time;

pub use exe::{TRUSTED_COMMENT_PREFIX, TRUSTED_COMMENT_SUFFIX};
pub use model::{
    Core, CoreSong, Difficulty, Dynamic, Manifest, Mode, Needs, Pattern, PatternAxes, PatternInfo,
    PatternKey,
};

pub const DATA_LATEST_URL: &str =
    "https://github.com/eun-yeon/DmPicker-data/releases/latest/download/manifest.json";
pub const DATA_RELEASE_BASE: &str = "https://github.com/eun-yeon/DmPicker-data/releases/download/";
pub const EXE_LATEST_URL: &str = "https://github.com/eun-yeon/DmPicker/releases/latest";
pub const EXE_RELEASE_BASE: &str = "https://github.com/eun-yeon/DmPicker/releases/tag/";
pub const CORE_FILE: &str = "core.json.gz";
pub const DYNAMIC_FILE: &str = "dynamic.json.gz";
pub const JACKETS_FILE: &str = "jackets.zip";
pub const EXE_PUBLIC_KEY: &str = "RWR60PowI9+nVnDAt2j9Wc3LIZeoSpafouuAfAS2CTtWnx2N2UtoNEeJ";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_GZIP_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CORE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DYNAMIC_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ZIP_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_JACKET_BYTES: usize = 64 * 1024;
pub const MAX_EXE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = 4 * 1024;
pub const MAX_SONGS: usize = 4096;
pub const MAX_STRING_BYTES: usize = 1024;
pub const MAX_ARRAY_ITEMS: usize = 256;
pub const MAX_REDIRECTS: usize = 5;
pub const MANIFEST_TIMEOUT_SECS: u64 = 10;
pub const JSON_TIMEOUT_SECS: u64 = 30;
pub const JACKETS_TIMEOUT_SECS: u64 = 300;
pub const EXE_TIMEOUT_SECS: u64 = 120;

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    UnsupportedSchema(u64),
    Io(std::io::Error),
}

impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::UnsupportedSchema(schema) => write!(f, "unsupported schema: {schema}"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Invalid(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// 바이트의 소문자 SHA-256
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 실제 길이와 해시를 함께 확인
pub fn validate_bytes(bytes: &[u8], hash: &str, expected: usize, cap: usize) -> Result<()> {
    if bytes.len() > cap
        || bytes.len() != expected
        || !valid_hash(hash)
        || sha256(bytes) != hash.to_ascii_lowercase()
    {
        return Err(Error::invalid("asset size or hash mismatch"));
    }
    Ok(())
}
