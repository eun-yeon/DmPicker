//! 내 키워드 파일, 곡 데이터 갱신과 따로 두고 지금 없는 곡 id도 남김

use std::collections::BTreeMap;
use std::path::PathBuf;

use dmpicker_search::Song;
use serde_json::{json, Value};

pub const UNREADABLE: &str = "키워드 파일을 읽지 못해서 저장할 수 없어요";
const SAVE_FAILED: &str = "키워드를 저장하지 못했어요";

#[derive(Clone)]
struct Entry {
    name: String,
    words: Vec<String>,
}

pub struct Keywords {
    path: PathBuf,
    entries: BTreeMap<u32, Entry>,
    writable: bool,
}

impl Keywords {
    /// 읽지 못한 파일은 덮어쓰면 키워드를 잃으니 이번 실행 동안 저장 안 함
    pub fn load(path: PathBuf) -> Self {
        let parsed = match std::fs::read(&path) {
            Ok(raw) => Self::parse(&raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(BTreeMap::new()),
            Err(_) => None,
        };
        let writable = parsed.is_some();
        Self { path, entries: parsed.unwrap_or_default(), writable }
    }

    fn parse(raw: &[u8]) -> Option<BTreeMap<u32, Entry>> {
        let v: Value = serde_json::from_slice(raw).ok()?;
        let mut entries = BTreeMap::new();
        for e in v.as_array()? {
            let id = u32::try_from(e.get("id")?.as_u64()?).ok()?;
            let words = e.get("keywords")?.as_array()?.iter().map(|w| w.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?;
            // 곡은 id로 찾음, 이름은 파일을 열어 볼 때 알아보라고 같이 둠
            let name = e.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            if entries.insert(id, Entry { name, words }).is_some() {
                return None;
            }
        }
        Some(entries)
    }

    pub fn apply(&self, songs: &mut [Song]) {
        for song in songs {
            song.user_keys = self.entries.get(&song.id).map(|e| e.words.clone()).unwrap_or_default();
        }
    }

    /// `id` 곡의 키워드를 `words`로 바꿔 저장, 다 지운 곡은 항목째 뺌
    /// 파일에 쓴 뒤에만 메모리도 바꿈, 실패하면 원래 키워드 그대로
    pub fn set(&mut self, id: u32, name: &str, words: Vec<String>) -> Result<(), &'static str> {
        if !self.writable {
            return Err(UNREADABLE);
        }
        let mut entries = self.entries.clone();
        entries.insert(id, Entry { name: name.to_owned(), words });
        entries.retain(|_, e| !e.words.is_empty());
        let rows: Vec<Value> = entries.iter().map(|(id, e)| json!({ "id": id, "name": e.name, "keywords": e.words })).collect();
        let raw = serde_json::to_vec_pretty(&rows).map_err(|_| SAVE_FAILED)?;
        if let Err(e) = crate::sys::save_file(&self.path, &raw) {
            eprintln!("save keywords: {e}");
            return Err(SAVE_FAILED);
        }
        self.entries = entries;
        Ok(())
    }
}
