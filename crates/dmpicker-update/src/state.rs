//! 업데이트 상태 파일과 확인 시점 판단

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::exe::Version;
use crate::model::validate_release_tag;
use crate::store::{atomic_write, read_limited, validate_generation_name};
use crate::time::{DAY, HOUR, retry_after_deadline};
use crate::{Error, Result, valid_hash};

pub const STATE_FILE: &str = "update-state.json";
pub const STATE_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckError {
    pub stage: String,
    pub http_status: Option<u16>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpdateState {
    pub last_check_start: Option<i64>,
    pub last_success: Option<i64>,
    pub failure_count: u32,
    pub next_eligible_at: Option<i64>,
    pub retry_after_until: Option<i64>,
    pub last_exe_check: Option<i64>,
    pub latest_version: Option<String>,
    pub notified_version: Option<Version>,
    pub applied_tag: Option<String>,
    pub confirmed_tag: Option<String>,
    pub confirmed_active_tag: Option<String>,
    pub preparing: Option<String>,
    pub loading: Option<String>,
    pub last_error: Option<CheckError>,
    pub notified_schema: Option<u64>,
}

fn optional<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).filter(|v| !v.is_null())
}

fn optional_time(object: &Map<String, Value>, name: &str) -> Result<Option<i64>> {
    optional(object, name)
        .map(|v| {
            v.as_i64()
                .ok_or_else(|| Error::invalid(format!("invalid {name}")))
        })
        .transpose()
}

fn optional_string(object: &Map<String, Value>, name: &str) -> Result<Option<String>> {
    optional(object, name)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid(format!("invalid {name}")))
        })
        .transpose()
}

impl UpdateState {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > STATE_MAX_BYTES {
            return Err(Error::invalid("update state too large"));
        }
        let value: Value = serde_json::from_slice(bytes)?;
        let object = value
            .as_object()
            .ok_or_else(|| Error::invalid("invalid update state"))?;
        let applied_tag = optional_string(object, "appliedTag")?;
        let confirmed_tag = optional_string(object, "confirmedTag")?;
        let confirmed_active_tag = optional_string(object, "confirmedActiveTag")?;
        for tag in [&applied_tag, &confirmed_tag, &confirmed_active_tag]
            .into_iter()
            .flatten()
        {
            validate_release_tag(tag)?;
        }
        let preparing = optional_string(object, "preparing")?;
        if preparing.as_deref().is_some_and(|hash| !valid_hash(hash)) {
            return Err(Error::invalid("invalid preparing hash"));
        }
        let loading = optional_string(object, "loading")?;
        if let Some(name) = &loading {
            validate_generation_name(name)?;
        }
        let failure_count = optional(object, "failureCount")
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| Error::invalid("invalid failureCount"))
            })
            .transpose()?
            .unwrap_or(0);
        let last_error = optional(object, "lastError")
            .map(|v| {
                let error = v
                    .as_object()
                    .ok_or_else(|| Error::invalid("invalid lastError"))?;
                let stage = optional_string(error, "stage")?
                    .ok_or_else(|| Error::invalid("missing error stage"))?;
                let http_status = optional(error, "httpStatus")
                    .map(|v| {
                        v.as_u64()
                            .filter(|n| (100..=599).contains(n))
                            .and_then(|n| u16::try_from(n).ok())
                            .ok_or_else(|| Error::invalid("invalid HTTP status"))
                    })
                    .transpose()?;
                Ok::<_, Error>(CheckError { stage, http_status })
            })
            .transpose()?;
        let version = |name| -> Result<Option<Version>> {
            optional_string(object, name)?
                .map(|value| Version::parse(&value))
                .transpose()
        };
        let latest_version = optional_string(object, "latestVersion")?;
        if let Some(tag) = &latest_version {
            Version::parse(tag)?;
        }
        let notified_schema = optional(object, "notifiedSchema")
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| Error::invalid("invalid notifiedSchema"))
            })
            .transpose()?;
        Ok(Self {
            last_check_start: optional_time(object, "lastCheckStart")?,
            last_success: optional_time(object, "lastSuccess")?,
            failure_count,
            next_eligible_at: optional_time(object, "nextEligibleAt")?,
            retry_after_until: optional_time(object, "retryAfterUntil")?,
            last_exe_check: optional_time(object, "lastExeCheck")?,
            latest_version,
            notified_version: version("notifiedVersion")?,
            applied_tag,
            confirmed_tag,
            confirmed_active_tag,
            preparing,
            loading,
            last_error,
            notified_schema,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec_pretty(&json!({
            "lastCheckStart": self.last_check_start,
            "lastSuccess": self.last_success,
            "failureCount": self.failure_count,
            "nextEligibleAt": self.next_eligible_at,
            "retryAfterUntil": self.retry_after_until,
            "lastExeCheck": self.last_exe_check,
            "latestVersion": self.latest_version,
            "notifiedVersion": self.notified_version.map(|version| version.to_string()),
            "appliedTag": self.applied_tag,
            "confirmedTag": self.confirmed_tag,
            "confirmedActiveTag": self.confirmed_active_tag,
            "preparing": self.preparing,
            "loading": self.loading,
            "lastError": self.last_error.as_ref().map(|error| json!({
                "stage": error.stage,
                "httpStatus": error.http_status,
            })),
            "notifiedSchema": self.notified_schema,
        }))?;
        if bytes.len() > STATE_MAX_BYTES {
            return Err(Error::invalid("update state too large"));
        }
        Ok(bytes)
    }

    /// 파일이 없으면 초기 상태, 손상된 내용은 오류로 반환
    pub fn load(path: &Path) -> Result<Self> {
        if !path.try_exists()? {
            return Ok(Self::default());
        }
        Self::parse(&read_limited(path, STATE_MAX_BYTES)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write(path, &self.to_bytes()?)
    }

    /// active 태그거나 지금 세대 기준으로 내용이 같다고 확인한 태그면 받지 않음
    pub fn needs_manifest(&self, latest_tag: &str, active_tag: Option<&str>) -> Result<bool> {
        validate_release_tag(latest_tag)?;
        let confirmed = self.confirmed_tag.as_deref() == Some(latest_tag)
            && active_tag.is_some()
            && self.confirmed_active_tag.as_deref() == active_tag;
        Ok(active_tag != Some(latest_tag) && !confirmed)
    }

    /// 내용이 같다고 검증한 태그와 비교 기준인 active 태그를 함께 기록
    pub fn confirm_unchanged(&mut self, tag: &str, active_tag: &str) -> Result<()> {
        validate_release_tag(tag)?;
        validate_release_tag(active_tag)?;
        self.confirmed_tag = Some(tag.to_owned());
        self.confirmed_active_tag = Some(active_tag.to_owned());
        Ok(())
    }

    /// pointer를 바꾼 뒤 호출
    pub fn record_data_applied(&mut self, tag: &str) -> Result<()> {
        validate_release_tag(tag)?;
        self.applied_tag = Some(tag.to_owned());
        self.confirmed_tag = None;
        self.confirmed_active_tag = None;
        Ok(())
    }

    pub fn record_success(&mut self, now: i64) {
        self.last_success = Some(now);
        self.failure_count = 0;
        self.next_eligible_at = Some(now.saturating_add(HOUR));
        self.retry_after_until = None;
        self.last_error = None;
    }

    /// exe 확인을 끝까지 마친 뒤 기록, 취소되면 기록 안 함
    pub fn record_exe_check(&mut self, now: i64) {
        self.last_exe_check = Some(now);
    }

    pub fn record_failure(&mut self, now: i64, error: CheckError, retry_after: Option<&str>) {
        self.failure_count = self.failure_count.saturating_add(1);
        let interval = if self.failure_count == 1 {
            2 * HOUR
        } else {
            6 * HOUR
        };
        self.retry_after_until = retry_after.and_then(|header| retry_after_deadline(header, now));
        self.next_eligible_at = Some(
            now.saturating_add(interval)
                .max(self.retry_after_until.unwrap_or(now)),
        );
        self.last_error = Some(error);
    }

    /// 준비 중에 꺼진 manifest는 다시 준비하지 않음, 다음 다른 태그부터 다시 시도
    pub fn can_prepare(&self, manifest_hash: &str) -> bool {
        self.preparing.as_deref() != Some(manifest_hash)
    }

    /// 표식을 디스크에 쓴 뒤에만 준비 시작
    pub fn begin_preparing(&mut self, path: &Path, manifest_hash: &str) -> Result<bool> {
        if !valid_hash(manifest_hash) {
            return Err(Error::invalid("invalid preparing hash"));
        }
        if !self.can_prepare(manifest_hash) {
            return Ok(false);
        }
        let mut next = self.clone();
        next.preparing = Some(manifest_hash.to_owned());
        next.save(path)?;
        *self = next;
        Ok(true)
    }

    /// 성공과 일반 실패 모두 표식을 지움
    pub fn finish_preparing(&mut self, path: &Path) -> Result<()> {
        let mut next = self.clone();
        next.preparing = None;
        next.save(path)?;
        *self = next;
        Ok(())
    }

    pub fn blocked_generation(&self) -> Option<&str> {
        self.loading.as_deref()
    }

    /// 남은 표식과 같은 세대는 건너뜀
    pub fn begin_loading(&mut self, path: &Path, generation: &str) -> Result<bool> {
        validate_generation_name(generation)?;
        if self.loading.as_deref() == Some(generation) {
            return Ok(false);
        }
        let mut next = self.clone();
        next.loading = Some(generation.to_owned());
        next.save(path)?;
        *self = next;
        Ok(true)
    }

    pub fn finish_loading(&mut self, path: &Path) -> Result<()> {
        let mut next = self.clone();
        next.loading = None;
        next.save(path)?;
        *self = next;
        Ok(())
    }

    /// 지금 앱보다 높고 이미 확인한 버전보다 새로울 때만 기록
    pub fn record_exe_available(&mut self, tag: &str, current: Version) -> Result<()> {
        let version = Version::parse(tag)?;
        if version <= current {
            return Ok(());
        }
        if let Some(latest) = &self.latest_version {
            if version <= Version::parse(latest)? {
                return Ok(());
            }
        }
        self.latest_version = Some(tag.to_owned());
        Ok(())
    }

    pub fn needs_exe_notice(&self, version: Version) -> bool {
        self.notified_version
            .is_none_or(|notified| version > notified)
    }

    /// 새 버전 알림을 실제로 보여 준 뒤 기록
    pub fn record_exe_notice(&mut self, version: Version) {
        if self.needs_exe_notice(version) {
            self.notified_version = Some(version);
        }
    }

    pub fn needs_schema_notice(&self, schema: u64) -> bool {
        self.notified_schema != Some(schema)
    }

    /// 마지막으로 알린 schema 값 하나만 기록
    pub fn record_schema_notice(&mut self, schema: u64) {
        self.notified_schema = Some(schema);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CheckAction {
    #[default]
    None,
    Start {
        check_data: bool,
        check_exe: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Scheduler {
    in_flight: bool,
}

/// 시계가 뒤로 가서 기록이 미래면 최근으로 보지 않음
fn recent(then: Option<i64>, now: i64, interval: i64) -> bool {
    then.and_then(|then| now.checked_sub(then))
        .is_some_and(|elapsed| (0..interval).contains(&elapsed))
}

/// 24시간보다 먼 기한은 시계가 바뀐 걸로 보고 무시, 확인이 멈추지 않게
fn waiting(deadline: Option<i64>, now: i64) -> bool {
    deadline.is_some_and(|deadline| deadline > now && deadline.saturating_sub(now) <= DAY)
}

impl Scheduler {
    /// 앱 시작은 Retry-After만 지키고 성공·실패 간격은 무시
    pub fn startup(&mut self, state: &mut UpdateState, now: i64, jobs_busy: bool) -> CheckAction {
        self.start(
            state,
            now,
            jobs_busy,
            !waiting(state.retry_after_until, now),
        )
    }

    /// 검색창 열기에서 데이터와 exe 기한을 따로 확인
    /// 받은 세대가 없어도 Retry-After는 지키고 성공·실패 간격만 무시
    pub fn due(
        &mut self,
        state: &mut UpdateState,
        now: i64,
        jobs_busy: bool,
        force_data: bool,
    ) -> CheckAction {
        let check_data = !waiting(state.retry_after_until, now)
            && (force_data
                || (!waiting(state.next_eligible_at, now)
                    && !recent(state.last_success, now, HOUR)));
        self.start(state, now, jobs_busy, check_data)
    }

    fn start(
        &mut self,
        state: &mut UpdateState,
        now: i64,
        jobs_busy: bool,
        check_data: bool,
    ) -> CheckAction {
        if self.in_flight || jobs_busy {
            return CheckAction::None;
        }
        let check_exe = !recent(state.last_exe_check, now, DAY);
        if !check_data && !check_exe {
            return CheckAction::None;
        }
        self.in_flight = true;
        if check_data {
            state.last_check_start = Some(now);
        }
        CheckAction::Start {
            check_data,
            check_exe,
        }
    }

    pub fn finish(&mut self) {
        self.in_flight = false;
    }
}
