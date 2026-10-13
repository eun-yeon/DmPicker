//! dmpicker-nav: DJMAX RESPECT V 선곡 화면의 순환 곡 목록을 이동하는 로직
//!
//! Windows API 없음, I/O는 [`GameIo`]로만 함
//! * [`ListModel`] / [`Group`] / [`Entry`]: 학습한 목록과 제목 그룹
//!   (`A..Z`, RANDOM, Kanji, Hangul, Symbol, Digit 순으로 순환)
//! * [`step`]: 게임의 키 동작 (Up/Down, 글자, PgUp/PgDn, LShift+RShift 리셋)
//! * [`Planner`] / [`plan`]: 최소 비용 키 순서 (Dijkstra)
//! * [`Navigator`]: 화면으로 확인하고 다시 계획하는 실행기
//!   hold/gap 타이밍 자동 조정 포함 ([`TimingTuner`])
//! * [`diff_keys`] / [`mode_key`]: 난이도·버튼 모드 도우미
#![forbid(unsafe_code)]

mod diff;
mod exec;
mod keys;
mod model;
mod plan;

pub use diff::{diff_keys, diff_keys_from, mode_key};
pub use exec::{
    Abort, GameIo, NavConfig, NavError, Navigator, Observation, Outcome, TimingTuner,
};
pub use keys::{step, ButtonMode, Key};
pub use model::{order_key, Anomaly, Entry, Group, GroupRun, ListModel, ModelError, OrderKey};
pub use plan::{plan, CostWeights, DistField, Planner};
