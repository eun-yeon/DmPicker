//! 로컬 곡 목록과 검색 (dmpicker-search), 재킷 폴더 위치

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use dmpicker_search::Song;
use dmpicker_search::{Filter, Mode, Searcher};

pub struct Catalog {
    pub songs: Vec<Song>,
    searcher: Searcher,
    pub jacket_dir: PathBuf,
}

impl Catalog {
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let cat = dmpicker_search::Catalog::load(data_dir.join("djmaxgg_songs.json"))?;
        let searcher = Searcher::new(&cat);
        Ok(Self { songs: cat.songs, searcher, jacket_dir: data_dir.join("jackets") })
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

    /// 곡 id들 중 카탈로그에 있고 고를 수 있는 곡의 `songs` 인덱스, 순서 그대로
    pub fn pick_out(&self, ids: &[u32], owned: Option<&HashSet<u32>>) -> Vec<usize> {
        ids.iter().filter_map(|&id| self.songs.iter().position(|s| s.id == id)).filter(|&i| self.allowed(i, owned)).collect()
    }
}
