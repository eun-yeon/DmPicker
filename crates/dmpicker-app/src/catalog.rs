//! 로컬 곡 목록과 검색 (dmpicker-search), 재킷 폴더 위치

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use dmpicker_search::Song;
use dmpicker_search::{Filter, Mode, Searcher};

use crate::keywords::{Keywords, UNREADABLE};

pub struct Catalog {
    pub songs: Vec<Song>,
    searcher: Searcher,
    pub jacket_dir: PathBuf,
    keywords: Option<Keywords>,
}

impl Catalog {
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let cat = dmpicker_search::Catalog::load(data_dir.join("djmaxgg_songs.json"))?;
        let searcher = Searcher::new(&cat);
        Ok(Self { songs: cat.songs, searcher, jacket_dir: data_dir.join("jackets"), keywords: None })
    }

    /// 곡 데이터와 별도로 읽은 내 키워드를 검색 색인에 합침
    pub fn load_keywords(&mut self, path: PathBuf) {
        self.keywords = Some(Keywords::load(path));
        self.reindex();
    }

    fn reindex(&mut self) {
        let mut cat = dmpicker_search::Catalog { songs: std::mem::take(&mut self.songs) };
        if let Some(keys) = &self.keywords {
            keys.apply(&mut cat.songs);
        }
        self.searcher = Searcher::new(&cat);
        self.songs = cat.songs;
    }

    /// `songs[song]`의 키워드를 `words`로 저장하고 색인을 다시 만듦, 실패하면 보여 줄 이유
    pub fn save_keywords(&mut self, song: usize, words: Vec<String>) -> Result<(), &'static str> {
        let s = &self.songs[song];
        let keys = self.keywords.as_mut().ok_or(UNREADABLE)?;
        keys.set(s.id, &s.name, words)?;
        self.reindex();
        Ok(())
    }

    /// 고를 수 있는 곡, `owned`가 없으면 전부 (첫 인식 전)
    fn allowed(&self, i: usize, owned: Option<&HashSet<u32>>) -> bool {
        owned.is_none_or(|o| o.contains(&self.songs[i].id))
    }

    /// `songs` 인덱스를 잘 맞는 순으로, `owned`가 있으면 그 곡 id 안에서만
    /// 키 입력마다 불림 (IME 조합 중 포함)
    pub fn search(&self, query: &str, limit: usize, owned: Option<&HashSet<u32>>) -> Vec<usize> {
        self.searcher.search_where(query, limit, |i| self.allowed(i, owned)).into_iter().map(|h| h.index).collect()
    }

    /// 조건 검색: `f`에 맞는 곡의 `songs` 인덱스 전부 (개수 제한 없음), 레벨 조건은 모드 `mode`로 봄
    pub fn filter(&self, f: &Filter, mode: Mode, owned: Option<&HashSet<u32>>) -> Vec<usize> {
        f.find(&self.songs, mode, |i| self.allowed(i, owned))
    }

    /// 곡 id가 카탈로그에 있고 고를 수 있으면 그 `songs` 인덱스
    pub fn pick(&self, id: u32, owned: Option<&HashSet<u32>>) -> Option<usize> {
        self.songs.iter().position(|s| s.id == id).filter(|&i| self.allowed(i, owned))
    }
}
