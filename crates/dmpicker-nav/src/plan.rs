//! 목록 그래프 위 최소 비용 키 계획 (Dijkstra)

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::keys::{step, Key};
use crate::model::ListModel;

/// 키별 비용, 임의 정수 단위 (기본: 방향키 한 번 = 100)
///
/// 글자·페이지 키는 기본값을 방향키보다 살짝 높게 둠
/// 누르는 횟수가 같으면 긴 스크롤 점프가 적은 계획이 이기게
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CostWeights {
    pub arrow: u32,
    pub letter: u32,
    pub page: u32,
    /// `None`이면 LShift+RShift를 계획에 안 씀 (기본값, 한쪽이 누락되면 탭이 바뀜)
    pub reset: Option<u32>,
}

impl Default for CostWeights {
    fn default() -> Self {
        CostWeights { arrow: 100, letter: 105, page: 105, reset: None }
    }
}

impl CostWeights {
    /// 모든 키 비용 1 (누른 횟수만 셈)
    pub fn presses() -> Self {
        CostWeights { arrow: 1, letter: 1, page: 1, reset: None }
    }
    pub fn weight(&self, key: Key) -> Option<u32> {
        match key {
            Key::Up | Key::Down => Some(self.arrow),
            Key::Letter(_) => Some(self.letter),
            Key::PgUp | Key::PgDn => Some(self.page),
            Key::Reset => self.reset,
            Key::Left | Key::Right | Key::Mode(_) => None,
        }
    }
}

/// [`ListModel`] 하나의 이동 그래프를 미리 계산해 둠, 계획은 1 ms보다 훨씬 빨리 나옴
#[derive(Clone, Debug)]
pub struct Planner {
    n: usize,
    /// 쓸 수 있는 키, 동점이면 앞 키 우선
    keys: Vec<Key>,
    wts: Vec<u32>,
    /// `next[p * keys.len() + j]` = `p`에서 `keys[j]`를 누른 뒤 위치
    next: Vec<u16>,
    rev_start: Vec<u32>,
    rev_src: Vec<u16>,
    rev_key: Vec<u8>,
}

impl Planner {
    pub fn new(model: &ListModel, weights: CostWeights) -> Planner {
        let n = model.len();
        let mut keys = vec![Key::Up, Key::Down];
        for c in 'A'..='Z' {
            if model.letter_run_index(c).is_some() {
                keys.push(Key::Letter(c));
            }
        }
        keys.extend([Key::PgUp, Key::PgDn, Key::Reset]);
        keys.retain(|k| weights.weight(*k).is_some());
        let wts = keys.iter().map(|k| weights.weight(*k).unwrap()).collect();
        let k = keys.len();
        let mut next = vec![0u16; n * k];
        for p in 0..n {
            for (j, key) in keys.iter().enumerate() {
                next[p * k + j] = step(model, p, *key) as u16;
            }
        }
        let mut pl = Planner { n, keys, wts, next, rev_start: vec![], rev_src: vec![], rev_key: vec![] };
        pl.rebuild_reverse();
        pl
    }

    fn rebuild_reverse(&mut self) {
        let (n, k) = (self.n, self.keys.len());
        let mut count = vec![0u32; n + 1];
        for p in 0..n {
            for j in 0..k {
                let q = self.next[p * k + j] as usize;
                if q != p {
                    count[q + 1] += 1;
                }
            }
        }
        for i in 0..n {
            count[i + 1] += count[i];
        }
        let total = count[n] as usize;
        let mut fill = count.clone();
        let mut src = vec![0u16; total];
        let mut key = vec![0u8; total];
        for p in 0..n {
            for j in 0..k {
                let q = self.next[p * k + j] as usize;
                if q != p {
                    let at = fill[q] as usize;
                    src[at] = p as u16;
                    key[at] = j as u8;
                    fill[q] += 1;
                }
            }
        }
        self.rev_start = count;
        self.rev_src = src;
        self.rev_key = key;
    }

    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    /// 플래너 그래프 (고쳐졌을 수 있음) 기준으로 `pos`에서 `key`를 누른 뒤 위치
    /// 플래너가 모르는 키는 위치를 안 바꿈
    pub fn apply(&self, pos: usize, key: Key) -> usize {
        match self.keys.iter().position(|k| *k == key) {
            Some(j) => self.next[pos * self.keys.len() + j] as usize,
            None => pos,
        }
    }
    /// 키 순서 전체를 적용
    pub fn apply_all(&self, pos: usize, keys: &[Key]) -> usize {
        keys.iter().fold(pos, |p, k| self.apply(p, *k))
    }

    /// 이동 하나를 바꿔 넣음 (게임이 모델과 다를 때 배운 수정)
    /// `key`가 플래너 키가 아니면 `false`
    pub fn override_edge(&mut self, from: usize, key: Key, to: usize) -> bool {
        let Some(j) = self.keys.iter().position(|k| *k == key) else { return false };
        if from >= self.n || to >= self.n {
            return false;
        }
        self.next[from * self.keys.len() + j] = to as u16;
        self.rebuild_reverse();
        true
    }

    /// 모든 위치에서 `target`까지 남은 비용 (역방향 Dijkstra)
    pub fn distances_to(&self, target: usize) -> DistField {
        let mut dist = vec![u32::MAX; self.n];
        let mut heap = BinaryHeap::new();
        dist[target] = 0;
        heap.push(Reverse((0u32, target as u16)));
        while let Some(Reverse((d, v))) = heap.pop() {
            let v = v as usize;
            if d > dist[v] {
                continue;
            }
            let (a, b) = (self.rev_start[v] as usize, self.rev_start[v + 1] as usize);
            for e in a..b {
                let u = self.rev_src[e] as usize;
                let nd = d + self.wts[self.rev_key[e] as usize];
                if nd < dist[u] {
                    dist[u] = nd;
                    heap.push(Reverse((nd, u as u16)));
                }
            }
        }
        DistField { target, dist }
    }

    /// `from`에서 `to`까지 최소 비용 키 순서
    pub fn plan(&self, from: usize, to: usize) -> Vec<Key> {
        self.distances_to(to).plan(self, from)
    }
}

/// 목표 하나까지 남은 비용, 어느 시작 위치에서든 다시 씀
/// 그래서 어긋난 뒤 재계획이 O(계획 길이)로 끝남
#[derive(Clone, Debug)]
pub struct DistField {
    target: usize,
    dist: Vec<u32>,
}

impl DistField {
    /// `from`에서의 최적 계획을 뽑음 (동점이면 플래너 키 순서상 앞 키)
    pub fn plan(&self, pl: &Planner, from: usize) -> Vec<Key> {
        let k = pl.keys.len();
        let mut out = Vec::new();
        let mut p = from;
        if self.dist[p] == u32::MAX {
            return out;
        }
        while p != self.target {
            let mut moved = false;
            for j in 0..k {
                let q = pl.next[p * k + j] as usize;
                if q != p && self.dist[q] != u32::MAX && pl.wts[j] + self.dist[q] == self.dist[p] {
                    out.push(pl.keys[j]);
                    p = q;
                    moved = true;
                    break;
                }
            }
            if !moved {
                debug_assert!(false, "inconsistent distance field");
                break;
            }
        }
        out
    }
}

/// 기본 가중치로 최소 비용 계획 (일회용 [`Planner`]를 만듦)
/// 여러 번 물을 거면 `Planner`를 재사용해야 함
pub fn plan(model: &ListModel, from: usize, to: usize) -> Vec<Key> {
    Planner::new(model, CostWeights::default()).plan(from, to)
}
