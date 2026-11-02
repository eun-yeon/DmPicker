//! 저장한 검색과 검색어가 쓰는 채보 설정

use dmpicker_search::{Diff, Filter, Mode};
use serde_json::{json, Value};

use crate::catalog::Catalog;
use crate::record_filter::{Direction, Options, Play, Sort};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryPattern {
    Hidden,
    Level(Option<Mode>, Diff),
    Mode(Option<Mode>),
    Settings,
}

impl QueryPattern {
    pub fn read(catalog: &Catalog, query: &str) -> Self {
        if crate::playlist::id(query).is_some() {
            return Self::Hidden;
        }
        match catalog.parse_filter(query) {
            Some(Filter { level: Some(level), .. }) => Self::Level(level.mode, level.diff),
            Some(filter) if filter.floor.is_some() || !filter.tags.is_empty() => Self::Mode(filter.mode()),
            None if catalog.has_tag_syntax(query) => Self::Mode(None),
            _ => Self::Settings,
        }
    }

    pub fn settings(self, mode: Mode, diff: Diff) -> (Option<Mode>, Option<Diff>) {
        match self {
            Self::Hidden | Self::Level(Some(_), _) | Self::Mode(Some(_)) => (None, None),
            Self::Level(None, _) | Self::Mode(None) => (Some(mode), None),
            Self::Settings => (Some(mode), Some(diff)),
        }
    }

    pub fn label(self, mode: Mode, diff: Diff) -> Option<String> {
        match self {
            Self::Hidden => None,
            Self::Level(explicit, diff) => Some(format!("{} · {}", explicit.unwrap_or(mode).label(), diff.label())),
            Self::Mode(explicit) => Some(explicit.unwrap_or(mode).label().to_owned()),
            Self::Settings => Some(format!("{} · {}", mode.label(), diff.label())),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub query: String,
    pub filter: Options,
    pub mode: Option<Mode>,
    pub diff: Option<Diff>,
}

impl Preset {
    pub fn capture(catalog: &Catalog, query: &str, filter: Options, mode: Mode, diff: Diff) -> Option<Self> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        let (mode, diff) = QueryPattern::read(catalog, query).settings(mode, diff);
        Some(Self { query: query.to_owned(), filter, mode, diff })
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let query = value["query"].as_str()?.trim();
        if query.is_empty() {
            return None;
        }
        // 필터는 설정 파일의 필터와 같은 규칙으로 읽음, 모르는 값만 기본값이고 프리셋은 살림
        value["filter"].as_object()?;
        let filter = Options::from_json(&value["filter"]);
        let mode = match value.get("mode") {
            None | Some(Value::Null) => None,
            Some(value) => Some(Mode::from_label(value.as_str()?)?),
        };
        let diff = match value.get("diff") {
            None | Some(Value::Null) => None,
            Some(value) => Some(Diff::from_label(value.as_str()?)?),
        };
        if diff.is_some() && mode.is_none() {
            return None;
        }
        Some(Self { query: query.to_owned(), filter, mode, diff })
    }

    pub fn json(&self) -> Value {
        json!({
            "query": self.query, "filter": self.filter.json(),
            "mode": self.mode.map(Mode::label), "diff": self.diff.map(Diff::label),
        })
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(mode) = self.mode {
            parts.push(mode.label().to_owned());
        }
        if let Some(diff) = self.diff {
            parts.push(diff.label().to_owned());
        }
        if self.filter.play != Play::All {
            parts.push(self.filter.play.label().to_owned());
        }
        match (self.filter.lower, self.filter.upper) {
            (Some(lo), Some(hi)) => parts.push(format!("레이트 {lo}~{hi}")),
            (Some(lo), None) => parts.push(format!("레이트 {lo} 이상")),
            (None, Some(hi)) => parts.push(format!("레이트 {hi} 이하")),
            (None, None) => {}
        }
        match (self.filter.sort, self.filter.direction) {
            (Sort::Title, Direction::Ascending) => {}
            (Sort::Title, Direction::Descending) => parts.push("제목 역순".to_owned()),
            (sort, direction) => {
                let order = if direction == Direction::Ascending {
                    "낮은 순"
                } else {
                    "높은 순"
                };
                parts.push(format!("{} {order}", sort.label()));
            }
        }
        if self.filter == Options::default() {
            parts.push("필터 없음".to_owned());
        }
        parts.join(" · ")
    }
}
