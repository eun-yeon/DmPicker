//! 로컬 곡 목록과 검색 (dmpicker-search), 재킷 폴더 위치

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use dmpicker_search::Song;
use dmpicker_search::{Diff, Filter, Mode, Searcher, TagNames};
use dmpicker_update::store::JACKETS_DIR;

use crate::keywords::{Keywords, UNREADABLE};

pub struct Catalog {
    pub songs: Vec<Song>,
    searcher: Searcher,
    tag_names: TagNames,
    pub jacket_dir: PathBuf,
    keywords: Option<Keywords>,
}

impl Catalog {
    /// 곡 데이터를 받기 전의 빈 검색 목록
    pub fn empty(data_dir: &Path) -> Self {
        let cat = dmpicker_search::Catalog { songs: Vec::new() };
        let searcher = Searcher::new(&cat);
        Self { songs: cat.songs, searcher, tag_names: TagNames::default(), jacket_dir: data_dir.join(JACKETS_DIR), keywords: None }
    }

    /// 검증된 세대의 곡과 패턴을 검색 목록으로 준비
    pub fn from_feed(core: &dmpicker_update::Core, dynamic: &dmpicker_update::Dynamic, data_dir: &Path) -> Result<Self, String> {
        let cat = dmpicker_search::Catalog::from_feed(core, dynamic)?;
        let searcher = Searcher::new(&cat);
        let tag_names = TagNames::from_songs(&cat.songs);
        Ok(Self { songs: cat.songs, searcher, tag_names, jacket_dir: data_dir.join(JACKETS_DIR), keywords: None })
    }

    /// 곡 id별 제목, 작업 스레드와 나눠 씀
    pub fn titles(&self) -> Arc<HashMap<u32, String>> {
        Arc::new(self.songs.iter().map(|song| (song.id, song.name.clone())).collect())
    }

    /// 교체 직전의 키워드와 저장 경로를 옮긴 뒤 새 곡에 다시 적용
    pub fn inherit_keywords(&mut self, previous: &mut Self) {
        self.keywords = previous.keywords.take();
        self.reindex();
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

    /// 검색어가 조건 검색이면 그 조건, 패턴 특성 이름은 지금 곡 데이터에 있는 것만
    pub fn parse_filter(&self, query: &str) -> Option<Filter> {
        Filter::parse(query, &self.tag_names)
    }

    /// 특성 조건을 쓴 검색어, 조건이 틀려도 일반 검색 대신 빈 결과로
    pub fn has_tag_syntax(&self, query: &str) -> bool {
        self.tag_names.has_tag_syntax(query)
    }

    /// 조건 검색: `f`에 맞는 (`songs` 인덱스, 패턴 난이도) 전부 (개수 제한 없음), 패턴은 모드 `mode`로 봄
    pub fn filter(&self, f: &Filter, mode: Mode, owned: Option<&HashSet<u32>>) -> Vec<(usize, Option<Diff>)> {
        f.find(&self.songs, mode, |i| self.allowed(i, owned))
    }

    /// 곡 id가 카탈로그에 있고 고를 수 있으면 그 `songs` 인덱스
    pub fn pick(&self, id: u32, owned: Option<&HashSet<u32>>) -> Option<usize> {
        self.songs.iter().position(|s| s.id == id).filter(|&i| self.allowed(i, owned))
    }
}
