//! 로컬 곡 목록과 검색 (dmpicker-search), 재킷 폴더 위치

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use dmpicker_search::Song;
use dmpicker_search::{Diff, Filter, Mode, Searcher, TagNames};
use dmpicker_update::store::JACKETS_DIR;

use crate::keywords::{Keywords, UNREADABLE};

/// 설정에 보이는 팩 하나, `count`는 고를 수 있는 곡 중 그 팩 곡 수
#[derive(Debug, PartialEq, Eq)]
pub struct DlcPack {
    pub code: String,
    pub name: String,
    pub count: usize,
}

pub struct Catalog {
    pub songs: Vec<Song>,
    searcher: Searcher,
    tag_names: TagNames,
    pub jacket_dir: PathBuf,
    keywords: Option<Keywords>,
    /// 검색에서 뺀 팩 코드, 곡 데이터를 바꿔도 이어 감
    hidden_dlc: HashSet<String>,
    /// 데이터의 팩 이름 표 (코드, 이름), 표시 순서대로
    dlc_names: Vec<(String, String)>,
}

impl Catalog {
    /// 곡 데이터를 받기 전의 빈 검색 목록
    pub fn empty(data_dir: &Path) -> Self {
        let cat = dmpicker_search::Catalog { songs: Vec::new() };
        let searcher = Searcher::new(&cat);
        Self {
            songs: cat.songs,
            searcher,
            tag_names: TagNames::default(),
            jacket_dir: data_dir.join(JACKETS_DIR),
            keywords: None,
            hidden_dlc: HashSet::new(),
            dlc_names: Vec::new(),
        }
    }

    /// 검증된 세대의 곡과 패턴을 검색 목록으로 준비
    pub fn from_feed(core: &dmpicker_update::Core, dynamic: &dmpicker_update::Dynamic, data_dir: &Path) -> Result<Self, String> {
        let cat = dmpicker_search::Catalog::from_feed(core, dynamic)?;
        let searcher = Searcher::new(&cat);
        let tag_names = TagNames::from_songs(&cat.songs);
        Ok(Self {
            songs: cat.songs,
            searcher,
            tag_names,
            jacket_dir: data_dir.join(JACKETS_DIR),
            keywords: None,
            hidden_dlc: HashSet::new(),
            dlc_names: core.dlcs.clone(),
        })
    }

    /// 곡 id별 제목, 작업 스레드와 나눠 씀
    pub fn titles(&self) -> Arc<HashMap<u32, String>> {
        Arc::new(self.songs.iter().map(|song| (song.id, song.name.clone())).collect())
    }

    /// 교체 직전 목록의 키워드·저장 경로와 꺼 둔 팩을 옮긴 뒤 새 곡에 다시 적용
    pub fn inherit(&mut self, previous: &mut Self) {
        self.keywords = previous.keywords.take();
        self.hidden_dlc = std::mem::take(&mut previous.hidden_dlc);
        self.reindex();
    }

    pub fn hidden_dlc(&self) -> &HashSet<String> {
        &self.hidden_dlc
    }

    pub fn set_hidden_dlc(&mut self, hidden: HashSet<String>) {
        self.hidden_dlc = hidden;
    }

    /// 팩 하나를 켜거나 끔, 켜졌으면 true
    pub fn toggle_dlc(&mut self, code: &str) -> bool {
        if self.hidden_dlc.remove(code) {
            true
        } else {
            self.hidden_dlc.insert(code.to_owned());
            false
        }
    }

    /// 결과 아랫줄의 팩 이름, 이름 표에 없으면 데이터의 `dlc` 그대로
    pub fn dlc_name<'a>(&'a self, song: &'a Song) -> &'a str {
        self.dlc_names.iter().find(|(code, _)| code == &song.dlc_code)
            .map(|(_, name)| name.as_str()).unwrap_or(&song.dlc)
    }

    /// `owned`에 든 팩과 곡 수, 이름 표 순서 뒤에 표에 없는 팩은 이름순
    pub fn dlc_packs(&self, owned: &HashSet<u32>) -> Vec<DlcPack> {
        let mut names: HashMap<&str, HashSet<&str>> = HashMap::new();
        let mut counts: HashMap<&str, (&Song, usize)> = HashMap::new();
        for song in &self.songs {
            names.entry(&song.dlc).or_default().insert(&song.dlc_code);
            if owned.contains(&song.id) {
                counts.entry(&song.dlc_code).or_insert((song, 0)).1 += 1;
            }
        }
        let order: HashMap<_, _> = self.dlc_names.iter().enumerate()
            .map(|(i, (code, _))| (code.as_str(), i)).collect();
        let mut packs: Vec<_> = counts.into_values().map(|(song, count)| {
            // 표에 없는 이름을 여러 코드가 같이 쓰면 (콜라보, PLI) 코드를 붙여 구분
            let name = if order.contains_key(song.dlc_code.as_str()) {
                self.dlc_name(song).to_owned()
            } else if names.get(song.dlc.as_str()).is_some_and(|codes| codes.len() > 1) {
                format!("{} ({})", song.dlc, song.dlc_code)
            } else {
                song.dlc.clone()
            };
            DlcPack { code: song.dlc_code.clone(), name, count }
        }).collect();
        packs.sort_by_cached_key(|pack| (
            order.get(pack.code.as_str()).copied().unwrap_or(usize::MAX),
            pack.name.to_lowercase(),
            pack.code.clone(),
        ));
        packs
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

    /// 고를 수 있는 곡, `owned`가 없으면 전부 (첫 인식 전), 꺼 둔 팩 곡은 늘 뺌
    fn allowed(&self, i: usize, owned: Option<&HashSet<u32>>) -> bool {
        owned.is_none_or(|o| o.contains(&self.songs[i].id)) && !self.hidden_dlc.contains(&self.songs[i].dlc_code)
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
