//! 업데이트 상태 파일과 확인 시점 판단

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::exe::Version;
use crate::model::validate_release_tag;
use crate::store::{atomic_write, read_limited, validate_generation_name};
use crate::time::{retry_after_deadline, DAY, HOUR};
use crate::{valid_hash, Error, Result};

pub const STATE_FILE: &str = "update-state.json";
pub const STATE_MAX_BYTES: usize = 64 * 1024;
pub const APPEARANCE_GAP: i64 = 10 * 60;

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
    pub applied_tag: Option<String>,
    pub confirmed_tag: Option<String>,
    pub confirmed_active_tag: Option<String>,
    pub preparing: Option<String>,
    pub loading: Option<String>,
    pub last_error: Option<CheckError>,
    pub skipped_versions: Vec<Version>,
    pub applied_version: Option<Version>,
    pub notified_versions: Vec<Version>,
    pub notified_schema: Option<u64>,
    pub exe_apply_failed: bool,
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
        let versions = |name| -> Result<Vec<Version>> {
            let mut parsed = Vec::new();
            if let Some(value) = optional(object, name) {
                for version in value
                    .as_array()
                    .ok_or_else(|| Error::invalid(format!("invalid {name}")))?
                {
                    let version = Version::parse(
                        version
                            .as_str()
                            .ok_or_else(|| Error::invalid("invalid version"))?,
                    )?;
                    if !parsed.contains(&version) {
                        parsed.push(version);
                    }
                }
            }
            Ok(parsed)
        };
        let skipped_versions = versions("skippedVersions")?;
        let notified_versions = versions("notifiedVersions")?;
        let notified_schema = optional(object, "notifiedSchema")
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| Error::invalid("invalid notifiedSchema"))
            })
            .transpose()?;
        let exe_apply_failed = optional(object, "exeApplyFailed")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| Error::invalid("invalid exeApplyFailed"))
            })
            .transpose()?
            .unwrap_or(false);
        let applied_version = optional_string(object, "appliedVersion")?
            .map(|s| Version::parse(&s))
            .transpose()?;
        Ok(Self {
            last_check_start: optional_time(object, "lastCheckStart")?,
            last_success: optional_time(object, "lastSuccess")?,
            failure_count,
            next_eligible_at: optional_time(object, "nextEligibleAt")?,
            retry_after_until: optional_time(object, "retryAfterUntil")?,
            last_exe_check: optional_time(object, "lastExeCheck")?,
            applied_tag,
            confirmed_tag,
            confirmed_active_tag,
            preparing,
            loading,
            last_error,
            skipped_versions,
            applied_version,
            notified_versions,
            notified_schema,
            exe_apply_failed,
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
            "appliedTag": self.applied_tag,
            "confirmedTag": self.confirmed_tag,
            "confirmedActiveTag": self.confirmed_active_tag,
            "preparing": self.preparing,
            "loading": self.loading,
            "lastError": self.last_error.as_ref().map(|error| json!({
                "stage": error.stage,
                "httpStatus": error.http_status,
            })),
            "skippedVersions": self.skipped_versions.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "appliedVersion": self.applied_version.map(|v| v.to_string()),
            "notifiedVersions": self.notified_versions.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "notifiedSchema": self.notified_schema,
            "exeApplyFailed": self.exe_apply_failed,
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

    /// exe 확인 요청을 실제로 시작할 때 기록
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

    pub fn skip_version(&mut self, version: Version) {
        if !self.skipped_versions.contains(&version) {
            self.skipped_versions.push(version);
        }
    }

    pub fn needs_exe_notice(&self, version: Version) -> bool {
        !self.notified_versions.contains(&version)
    }

    /// 준비 알림을 실제로 보여 준 뒤 기록
    pub fn record_exe_notice(&mut self, version: Version) {
        if self.needs_exe_notice(version) {
            self.notified_versions.push(version);
        }
    }

    pub fn needs_schema_notice(&self, schema: u64) -> bool {
        self.notified_schema != Some(schema)
    }

    /// 마지막으로 알린 schema 값 하나만 기록
    pub fn record_schema_notice(&mut self, schema: u64) {
        self.notified_schema = Some(schema);
    }

    /// 다음 실행에서 적용 실패 알림을 보여 준 뒤 지움
    pub fn clear_exe_apply_failure(&mut self) {
        self.exe_apply_failed = false;
    }

    pub fn record_exe_apply_failure(&mut self) {
        self.exe_apply_failed = true;
    }

    /// 새 exe를 원래 이름으로 바꾼 결과만 기록
    /// 먼저 기록하면 이름 바꾸기 전에 꺼졌을 때 되돌림으로 오판함
    pub fn record_exe_rename(&mut self, version: Version, succeeded: bool) {
        self.exe_apply_failed = !succeeded;
        if succeeded {
            self.applied_version = Some(version);
        }
    }

    /// 수동 되돌림이면 문제 버전만 건너뜀
    pub fn detect_rollback(&mut self, current: Version) -> Option<Version> {
        let applied = self.applied_version.take()?;
        if current < applied {
            self.skip_version(applied);
            Some(applied)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tick {
    pub game_present: bool,
    pub jobs_busy: bool,
    pub appearance_delay: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CheckAction {
    #[default]
    None,
    Start {
        check_exe: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Scheduler {
    game_present: bool,
    appearance_at: Option<i64>,
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
    /// 게임 창이 새로 생기면 0~10초 뒤 (마지막 확인 시작이 10분 안이면 건너뜀)
    /// 게임 창이 있는 동안엔 `next_eligible_at`이 지나면, Retry-After는 둘 다 지킴
    /// 반환 후 상태를 저장하고 worker 시작, 틱에서는 파일이나 네트워크에 접근하지 않음
    pub fn advance(&mut self, state: &mut UpdateState, now: i64, event: Tick) -> CheckAction {
        let appeared = event.game_present && !self.game_present;
        self.game_present = event.game_present;
        if !event.game_present {
            self.appearance_at = None;
            return CheckAction::None;
        }
        if appeared {
            self.appearance_at = if recent(state.last_check_start, now, APPEARANCE_GAP) {
                None
            } else {
                Some(now.saturating_add(i64::from(event.appearance_delay.min(10))))
            };
        }
        if self.in_flight || event.jobs_busy || waiting(state.retry_after_until, now) {
            return CheckAction::None;
        }
        let appearance_due = match self.appearance_at {
            Some(at) if at > now && at.saturating_sub(now) <= 10 => return CheckAction::None,
            Some(_) => true,
            None => false,
        };
        if !appearance_due && waiting(state.next_eligible_at, now) {
            return CheckAction::None;
        }
        self.appearance_at = None;
        self.in_flight = true;
        state.last_check_start = Some(now);
        let check_exe = !recent(state.last_exe_check, now, DAY);
        CheckAction::Start { check_exe }
    }

    pub fn finish(&mut self) {
        self.in_flight = false;
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }
}
