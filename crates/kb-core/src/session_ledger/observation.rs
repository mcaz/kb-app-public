//! 固定期間の観測を、実際に結合できる会話だけで集計する。ADR-0018参照。

use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementPurpose {
    Normal,
    Diagnostic,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationArm {
    GuiOn,
    GuiOff,
    EnvironmentOff,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FinalOutputFlags {
    pub status_line_present: bool,
    pub cadence_line_present: bool,
    pub status_omitted_for_budget: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateWarningFlags {
    pub body_reduced: bool,
    pub relations_reduced: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartSource {
    Launcher,
    HostStartEvent,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeasurementContext {
    pub purpose: MeasurementPurpose,
    pub arm: ObservationArm,
    pub session_started_at_ms: Option<i64>,
    pub session_start_source: Option<SessionStartSource>,
    pub session_start_generation: Option<i64>,
    pub launcher_started_at_ms: Option<i64>,
    pub final_output: Option<FinalOutputFlags>,
    pub update_warnings: Option<UpdateWarningFlags>,
}

impl MeasurementContext {
    pub fn from_environment(session_id: Option<&str>) -> Self {
        let purpose = match std::env::var("KB_APP_OBSERVATION_PURPOSE") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => Some(String::new()),
        };
        let launch_id = std::env::var("KB_APP_OBSERVATION_SESSION_ID").ok();
        let launch_time = std::env::var("KB_APP_OBSERVATION_SESSION_STARTED_AT_MS").ok();
        Self::from_hints(
            purpose.as_deref(),
            launch_id.as_deref(),
            launch_time.as_deref(),
            session_id,
            now_ms(),
        )
    }

    pub fn from_hints(
        purpose: Option<&str>,
        launch_session_id: Option<&str>,
        launch_started_at_ms: Option<&str>,
        session_id: Option<&str>,
        now: i64,
    ) -> Self {
        let purpose = match purpose {
            None | Some("normal") => MeasurementPurpose::Normal,
            Some("diagnostic") => MeasurementPurpose::Diagnostic,
            Some(_) => MeasurementPurpose::Unknown,
        };
        let session_started_at_ms = session_id
            .filter(|id| !id.is_empty() && id.len() <= 8_192)
            .filter(|id| Some(*id) == launch_session_id)
            .and_then(|_| launch_started_at_ms?.parse::<i64>().ok())
            .filter(|start| *start >= 0 && *start <= now);
        Self {
            purpose,
            session_started_at_ms,
            session_start_source: session_started_at_ms.map(|_| SessionStartSource::Launcher),
            ..Self::default()
        }
    }

    /// 環境変数は補助時刻に留める。失効・欠落したhost証拠をlauncherで復活させない。
    pub fn with_session_start(mut self, evidence: Option<SessionStartEvidence>) -> Self {
        if self.session_start_source == Some(SessionStartSource::Launcher) {
            self.launcher_started_at_ms = self.session_started_at_ms;
        }
        self.session_started_at_ms = evidence.map(|value| value.observed_at_ms);
        self.session_start_source = evidence.map(|_| SessionStartSource::HostStartEvent);
        self.session_start_generation = evidence.map(|value| value.generation);
        self
    }

    pub(super) fn validate(&self, event: &LedgerEvent) -> Result<()> {
        if let Some(start) = self.session_started_at_ms {
            ensure!(
                event.session_hash.is_some() && start >= 0 && start <= event.observed_at_ms,
                "観測の会話開始時刻または帰属が不正"
            );
        }
        ensure!(
            match self.session_start_source {
                Some(SessionStartSource::HostStartEvent) =>
                    self.session_started_at_ms.is_some()
                        && self
                            .session_start_generation
                            .is_some_and(|generation| generation > 0),
                Some(SessionStartSource::Launcher) =>
                    self.session_started_at_ms.is_some() && self.session_start_generation.is_none(),
                None => self.session_start_generation.is_none(),
            } && self
                .launcher_started_at_ms
                .is_none_or(|start| start >= 0 && start <= event.observed_at_ms),
            "観測の開始証拠の来歴が不正"
        );
        if let Some(flags) = self.final_output {
            ensure!(
                matches!(
                    event.observation,
                    Observation::Hook {
                        observation: HookObservation::OutputPrepared { .. }
                    }
                ) && !(flags.status_line_present && flags.status_omitted_for_budget),
                "観測の最終出力フラグが不正"
            );
        }
        ensure!(
            self.update_warnings.is_none()
                || matches!(
                    event.observation,
                    Observation::Write {
                        tool: WriteTool::Update,
                        outcome: WriteOutcome::Success,
                        ..
                    }
                ),
            "観測の削減警告は更新成功時だけ記録する"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionWindow {
    pub since_ms: i64,
    pub until_ms: i64,
}

#[derive(Clone, Debug)]
pub struct ObservationQuery {
    pub since_ms: i64,
    pub until_ms: i64,
    pub workspace_id: String,
    pub manual_exclusions: Vec<ExclusionWindow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionExclusionReason {
    Diagnostic,
    ManualExclusion,
    UnknownPurpose,
    UnknownArm,
    EnvironmentOff,
    MixedArm,
    UnknownPermission,
    MixedPermission,
    ExcludedPermission,
    UnverifiedStart,
    ConflictingStart,
    OutsideWindow,
    TooFewPrompts,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionExclusionCount {
    pub reason: SessionExclusionReason,
    pub sessions: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RateUnavailableReason {
    UnsupportedSurface,
    IncompletePeriod,
    UnverifiedWriteLinkage,
    NoQualifyingSessions,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ObservationQuality {
    pub unspecified_measurement_events: u64,
    pub unknown_purpose_events: u64,
    pub unknown_arm_events: u64,
    pub unverified_start_events: u64,
    pub host_start_event_timed_events: u64,
    pub launcher_timed_events: u64,
    pub legacy_start_timed_events: u64,
    pub sessionless_events: u64,
    pub unknown_hook_permission_events: u64,
    pub hook_observations_without_prompt_identity: u64,
    pub unknown_write_permission_events: u64,
    pub unfinalized_outputs: u64,
    pub unknown_final_output_events: u64,
    pub emitted_status_lines: u64,
    pub emitted_cadence_lines: u64,
    pub emitted_status_omissions: u64,
    pub update_warning_known: u64,
    pub update_warning_unknown: u64,
    pub body_reduced_updates: u64,
    pub relations_reduced_updates: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObservationStratum {
    pub surface: ClientSurface,
    pub arm: ObservationArm,
    pub permission_mode: Option<PermissionMode>,
    pub observed_events: u64,
    pub diagnostic_events: u64,
    pub manual_excluded_events: u64,
    // この件数は会話率の分母ではない。開始未確認の通常利用も含めて見えるようにする。
    pub normal_counts: SurfaceSummary,
    pub quality: ObservationQuality,
    pub normal_quality: ObservationQuality,
    pub observed_actual_sessions: u64,
    pub qualifying_sessions: u64,
    pub successful_write_sessions: u64,
    pub write_linkage_verified: bool,
    pub verified_write_linkage_events: u64,
    pub unverified_write_linkage_events: u64,
    pub session_write_rate: Option<f64>,
    pub rate_unavailable_reason: Option<RateUnavailableReason>,
    pub session_exclusions: Vec<SessionExclusionCount>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObservationReport {
    pub exists: bool,
    pub workspace_id: String,
    pub since_ms: i64,
    pub until_ms: i64,
    pub generated_at_ms: i64,
    pub retention_days: i64,
    pub retention_covers_period: bool,
    pub period_closed: bool,
    pub session_start_time_basis: &'static str,
    pub exact_host_start_time_verified: bool,
    pub lifecycle_hook_completeness_verified: bool,
    pub manual_exclusions: Vec<ExclusionWindow>,
    pub strata: Vec<ObservationStratum>,
}

pub fn observation_summary(query: &ObservationQuery) -> Result<ObservationReport> {
    observation_summary_at(&runtime_path()?, query)
}

pub fn observation_summary_at(path: &Path, query: &ObservationQuery) -> Result<ObservationReport> {
    observation_summary_at_time(path, query, now_ms())
}

struct ObservedEvent {
    event: LedgerEvent,
    state: Option<String>,
}

fn observation_summary_at_time(
    path: &Path,
    query: &ObservationQuery,
    now: i64,
) -> Result<ObservationReport> {
    ensure!(
        query.since_ms >= 0 && query.until_ms > query.since_ms,
        "観測の固定期間が不正"
    );
    ensure!(
        query.manual_exclusions.len() <= 64,
        "観測の手動除外期間が多すぎる"
    );
    for window in &query.manual_exclusions {
        ensure!(
            window.since_ms >= 0 && window.until_ms > window.since_ms,
            "観測の手動除外期間が不正"
        );
    }
    let workspace_id = validate_workspace_id(&query.workspace_id)?;
    let mut report = ObservationReport {
        exists: false,
        workspace_id,
        since_ms: query.since_ms,
        until_ms: query.until_ms,
        generated_at_ms: now,
        retention_days: RETENTION_DAYS,
        retention_covers_period: query.since_ms >= now.saturating_sub(RETENTION_DAYS * DAY_MS),
        period_closed: query.until_ms <= now,
        session_start_time_basis: "host_start_event_observed_at_or_legacy_launcher",
        exact_host_start_time_verified: false,
        lifecycle_hook_completeness_verified: false,
        manual_exclusions: query.manual_exclusions.clone(),
        strata: Vec::new(),
    };
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    conn.pragma_update(None, "query_only", true)?;
    let tx = conn.transaction()?;
    validate_schema(&tx)?;
    report.exists = true;
    // 期間外・診断である同一会話を発見するため、保持中の当該workspaceを通して読む。
    let mut statement = tx.prepare(
        "SELECT event_id, observed_at_ms, workspace_id, payload, emission_state, receipt_hash
         FROM ledger_events WHERE workspace_id = ?1 ORDER BY observed_at_ms, event_id",
    )?;
    let mut rows = statement.query([&report.workspace_id])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let payload: String = row.get(3)?;
        let event: LedgerEvent = serde_json::from_str(&payload).context("台帳event形式が不正")?;
        event.validate()?;
        ensure!(
            event.event_id == row.get::<_, String>(0)?
                && event.observed_at_ms == row.get::<_, i64>(1)?
                && event.workspace_id == row.get::<_, Option<String>>(2)?,
            "台帳の索引とeventが不一致"
        );
        let state: Option<String> = row.get(4)?;
        let token: Option<String> = row.get(5)?;
        if matches!(
            event.observation,
            Observation::Hook {
                observation: HookObservation::OutputPrepared { .. }
            }
        ) {
            ensure!(
                matches!(
                    state.as_deref(),
                    Some("prepared" | "emitted" | "stdout_failed")
                ) && token.as_deref() == Some(digest(&["prepared", &payload]).as_str()),
                "台帳の出力観測状態が不正"
            );
        } else {
            ensure!(
                state.is_none() && token.is_none(),
                "台帳の出力観測状態が不正"
            );
        }
        events.push(ObservedEvent { event, state });
    }
    report.strata = aggregate(
        &events,
        query,
        report.retention_covers_period && report.period_closed,
    )?;
    Ok(report)
}

fn in_window(at: i64, since: i64, until: i64) -> bool {
    at >= since && at < until
}

fn manually_excluded(event: &LedgerEvent, query: &ObservationQuery) -> bool {
    query
        .manual_exclusions
        .iter()
        .any(|window| in_window(event.observed_at_ms, window.since_ms, window.until_ms))
}

fn stratum_key(
    surface: ClientSurface,
    arm: ObservationArm,
    permission: Option<PermissionMode>,
) -> Result<String> {
    Ok(serde_json::to_string(&(surface, arm, permission))?)
}

struct StratumAccumulator {
    report: ObservationStratum,
    normal: Accumulator,
    exclusions: BTreeMap<SessionExclusionReason, u64>,
}

impl StratumAccumulator {
    fn new(
        surface: ClientSurface,
        arm: ObservationArm,
        permission_mode: Option<PermissionMode>,
    ) -> Self {
        Self {
            report: ObservationStratum {
                surface,
                arm,
                permission_mode,
                observed_events: 0,
                diagnostic_events: 0,
                manual_excluded_events: 0,
                normal_counts: SurfaceSummary::empty(surface),
                quality: ObservationQuality::default(),
                normal_quality: ObservationQuality::default(),
                observed_actual_sessions: 0,
                qualifying_sessions: 0,
                successful_write_sessions: 0,
                write_linkage_verified: false,
                verified_write_linkage_events: 0,
                unverified_write_linkage_events: 0,
                session_write_rate: None,
                rate_unavailable_reason: None,
                session_exclusions: Vec::new(),
            },
            normal: Accumulator::new(surface),
            exclusions: BTreeMap::new(),
        }
    }

    fn finish(mut self, complete_period: bool) -> ObservationStratum {
        self.report.normal_counts = self.normal.finish();
        self.report.rate_unavailable_reason = if self.report.surface != ClientSurface::ClaudeCode {
            Some(RateUnavailableReason::UnsupportedSurface)
        } else if !complete_period {
            Some(RateUnavailableReason::IncompletePeriod)
        } else if !self.report.write_linkage_verified {
            Some(RateUnavailableReason::UnverifiedWriteLinkage)
        } else if self.report.qualifying_sessions == 0 {
            Some(RateUnavailableReason::NoQualifyingSessions)
        } else {
            self.report.session_write_rate = Some(
                self.report.successful_write_sessions as f64
                    / self.report.qualifying_sessions as f64,
            );
            None
        };
        self.report.session_exclusions = self
            .exclusions
            .into_iter()
            .map(|(reason, sessions)| SessionExclusionCount { reason, sessions })
            .collect();
        self.report
    }
}

fn observe_quality(quality: &mut ObservationQuality, observed: &ObservedEvent) {
    let event = &observed.event;
    let metadata = event.measurement;
    quality.unspecified_measurement_events += u64::from(metadata == MeasurementContext::default());
    quality.unknown_purpose_events += u64::from(metadata.purpose == MeasurementPurpose::Unknown);
    quality.unknown_arm_events += u64::from(metadata.arm == ObservationArm::Unknown);
    quality.unverified_start_events += u64::from(metadata.session_started_at_ms.is_none());
    if metadata.session_started_at_ms.is_some() {
        match metadata.session_start_source {
            Some(SessionStartSource::HostStartEvent) => quality.host_start_event_timed_events += 1,
            Some(SessionStartSource::Launcher) => quality.launcher_timed_events += 1,
            None => quality.legacy_start_timed_events += 1,
        }
    }
    quality.sessionless_events += u64::from(event.session_hash.is_none());
    match &event.observation {
        Observation::Hook { observation } => {
            quality.unknown_hook_permission_events += u64::from(event.permission_mode.is_none());
            quality.hook_observations_without_prompt_identity +=
                u64::from(event.prompt_hash.is_none() && event.turn_hash.is_none());
            if matches!(observation, HookObservation::OutputPrepared { .. }) {
                quality.unfinalized_outputs +=
                    u64::from(observed.state.as_deref() == Some("prepared"));
                quality.unknown_final_output_events += u64::from(metadata.final_output.is_none());
                if observed.state.as_deref() == Some("emitted")
                    && let Some(flags) = metadata.final_output
                {
                    quality.emitted_status_lines += u64::from(flags.status_line_present);
                    quality.emitted_cadence_lines += u64::from(flags.cadence_line_present);
                    quality.emitted_status_omissions += u64::from(flags.status_omitted_for_budget);
                }
            }
        }
        Observation::Write { tool, outcome, .. } => {
            quality.unknown_write_permission_events += u64::from(event.permission_mode.is_none());
            if *tool == WriteTool::Update && *outcome == WriteOutcome::Success {
                if let Some(flags) = metadata.update_warnings {
                    quality.update_warning_known += 1;
                    quality.body_reduced_updates += u64::from(flags.body_reduced);
                    quality.relations_reduced_updates += u64::from(flags.relations_reduced);
                } else {
                    quality.update_warning_unknown += 1;
                }
            }
        }
    }
}

fn aggregate(
    events: &[ObservedEvent],
    query: &ObservationQuery,
    complete_period: bool,
) -> Result<Vec<ObservationStratum>> {
    let mut sessions = BTreeMap::<String, Vec<&ObservedEvent>>::new();
    for observed in events {
        if let Some(session) = &observed.event.session_hash {
            sessions.entry(session.clone()).or_default().push(observed);
        }
    }
    let diagnostic_sessions: HashSet<&String> = sessions
        .iter()
        .filter_map(|(key, observations)| {
            observations
                .iter()
                .any(|item| item.event.measurement.purpose == MeasurementPurpose::Diagnostic)
                .then_some(key)
        })
        .collect();
    let manual_sessions: HashSet<&String> = sessions
        .iter()
        .filter_map(|(key, observations)| {
            observations
                .iter()
                .any(|item| manually_excluded(&item.event, query))
                .then_some(key)
        })
        .collect();
    let mut groups = BTreeMap::<String, StratumAccumulator>::new();
    for observed in events
        .iter()
        .filter(|item| in_window(item.event.observed_at_ms, query.since_ms, query.until_ms))
    {
        let event = &observed.event;
        let metadata = event.measurement;
        let group = groups
            .entry(stratum_key(
                event.surface,
                metadata.arm,
                event.permission_mode,
            )?)
            .or_insert_with(|| {
                StratumAccumulator::new(event.surface, metadata.arm, event.permission_mode)
            });
        group.report.observed_events += 1;
        observe_quality(&mut group.report.quality, observed);
        let diagnostic = metadata.purpose == MeasurementPurpose::Diagnostic
            || event
                .session_hash
                .as_ref()
                .is_some_and(|session| diagnostic_sessions.contains(session));
        let manual = manually_excluded(event, query)
            || event
                .session_hash
                .as_ref()
                .is_some_and(|session| manual_sessions.contains(session));
        group.report.diagnostic_events += u64::from(diagnostic);
        group.report.manual_excluded_events += u64::from(manual);
        if !diagnostic
            && !manual
            && metadata.purpose == MeasurementPurpose::Normal
            && matches!(metadata.arm, ObservationArm::GuiOn | ObservationArm::GuiOff)
        {
            group.normal.observe(event, observed.state.as_deref())?;
            observe_quality(&mut group.report.normal_quality, observed);
        }
    }
    for observations in sessions.values().filter(|items| {
        items
            .iter()
            .any(|item| in_window(item.event.observed_at_ms, query.since_ms, query.until_ms))
    }) {
        let surface = observations[0].event.surface;
        let (arm, permission, exclusions, prompts, successful_write) =
            classify_session(observations, query);
        let group = groups
            .entry(stratum_key(surface, arm, permission)?)
            .or_insert_with(|| StratumAccumulator::new(surface, arm, permission));
        group.report.observed_actual_sessions += 1;
        for reason in &exclusions {
            *group.exclusions.entry(*reason).or_default() += 1;
        }
        if exclusions.is_empty() && surface == ClientSurface::ClaudeCode && prompts >= 3 {
            group.report.qualifying_sessions += 1;
            group.report.successful_write_sessions += u64::from(successful_write);
        }
    }
    for group in groups.values_mut() {
        if group.report.surface != ClientSurface::ClaudeCode {
            continue;
        }
        for observed in events.iter().filter(|observed| {
            let event = &observed.event;
            event.surface == ClientSurface::ClaudeCode
                && matches!(event.observation, Observation::Write { .. })
                && event.measurement.purpose != MeasurementPurpose::Diagnostic
                && (event.measurement.arm == group.report.arm
                    || event.measurement.arm == ObservationArm::Unknown)
                && in_window(event.observed_at_ms, query.since_ms, query.until_ms)
                && !manually_excluded(event, query)
                && !event.session_hash.as_ref().is_some_and(|session| {
                    diagnostic_sessions.contains(session) || manual_sessions.contains(session)
                })
        }) {
            let event = &observed.event;
            let linked = event.measurement.purpose == MeasurementPurpose::Normal
                && event
                    .session_hash
                    .as_ref()
                    .and_then(|session| sessions.get(session))
                    .is_some_and(|observations| {
                        observations.iter().any(|candidate| {
                            let hook = &candidate.event;
                            matches!(hook.observation, Observation::Hook { .. })
                                && hook.measurement.purpose == MeasurementPurpose::Normal
                                && hook.measurement.arm == event.measurement.arm
                                && hook.measurement.session_started_at_ms.is_some()
                                && hook.measurement.session_started_at_ms
                                    == event.measurement.session_started_at_ms
                                && hook.measurement.session_start_source
                                    == event.measurement.session_start_source
                                && hook.measurement.session_start_generation
                                    == event.measurement.session_start_generation
                        })
                    });
            if linked {
                group.report.verified_write_linkage_events += 1;
            } else {
                group.report.unverified_write_linkage_events += 1;
            }
        }
        // session無しwriteを「書かなかった会話」へ置き換えない。受信元との結合が未確認なら率は未確認。
        group.report.write_linkage_verified = group.report.verified_write_linkage_events > 0
            && group.report.unverified_write_linkage_events == 0;
    }
    Ok(groups
        .into_values()
        .map(|group| group.finish(complete_period))
        .collect())
}

fn classify_session(
    observations: &[&ObservedEvent],
    query: &ObservationQuery,
) -> (
    ObservationArm,
    Option<PermissionMode>,
    Vec<SessionExclusionReason>,
    u64,
    bool,
) {
    use SessionExclusionReason as Reason;
    let mut reasons = std::collections::BTreeSet::new();
    let first_arm = observations[0].event.measurement.arm;
    let mut mixed_arm = false;
    let mut permission = None;
    let mut mixed_permission = false;
    let mut start = None;
    let mut prompts = 0;
    let mut successful_write = false;
    for observed in observations {
        let event = &observed.event;
        let metadata = event.measurement;
        match metadata.purpose {
            MeasurementPurpose::Diagnostic => {
                reasons.insert(Reason::Diagnostic);
            }
            MeasurementPurpose::Unknown => {
                reasons.insert(Reason::UnknownPurpose);
            }
            MeasurementPurpose::Normal => {}
        }
        match metadata.arm {
            ObservationArm::Unknown => {
                reasons.insert(Reason::UnknownArm);
            }
            ObservationArm::EnvironmentOff => {
                reasons.insert(Reason::EnvironmentOff);
            }
            _ => {}
        }
        mixed_arm |= metadata.arm != first_arm;
        if manually_excluded(event, query) {
            reasons.insert(Reason::ManualExclusion);
        }
        if !in_window(event.observed_at_ms, query.since_ms, query.until_ms) {
            reasons.insert(Reason::OutsideWindow);
        }
        if let Some(current) = metadata.session_started_at_ms {
            if !in_window(current, query.since_ms, query.until_ms) {
                reasons.insert(Reason::OutsideWindow);
            }
            let identity = (
                current,
                metadata.session_start_source,
                metadata.session_start_generation,
            );
            if start.is_some_and(|first| first != identity) {
                reasons.insert(Reason::ConflictingStart);
            }
            start = Some(identity);
        } else {
            reasons.insert(Reason::UnverifiedStart);
        }
        match &event.observation {
            Observation::Hook { observation } => {
                if let Some(current) = event.permission_mode {
                    mixed_permission |= permission.is_some_and(|first| first != current);
                    permission = Some(current);
                    if matches!(
                        current,
                        PermissionMode::BypassPermissions
                            | PermissionMode::DontAsk
                            | PermissionMode::DangerFullAccess
                    ) {
                        reasons.insert(Reason::ExcludedPermission);
                    }
                } else {
                    reasons.insert(Reason::UnknownPermission);
                }
                if !matches!(observation, HookObservation::Filtered { .. })
                    && metadata.purpose == MeasurementPurpose::Normal
                {
                    prompts += 1;
                }
            }
            Observation::Write { outcome, .. } => {
                successful_write |= *outcome == WriteOutcome::Success;
            }
        }
    }
    if permission.is_none() {
        reasons.insert(Reason::UnknownPermission);
    }
    if mixed_arm {
        reasons.insert(Reason::MixedArm);
    }
    if mixed_permission {
        reasons.insert(Reason::MixedPermission);
    }
    if prompts < 3 {
        reasons.insert(Reason::TooFewPrompts);
    }
    (
        if mixed_arm {
            ObservationArm::Unknown
        } else {
            first_arm
        },
        if mixed_permission { None } else { permission },
        reasons.into_iter().collect(),
        prompts,
        successful_write,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_delivery::render_hook_delivery;
    use serde_json::json;

    const WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const OTHER_WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn query() -> ObservationQuery {
        ObservationQuery {
            since_ms: 1_000,
            until_ms: 2_000,
            workspace_id: WORKSPACE.into(),
            manual_exclusions: Vec::new(),
        }
    }

    fn context<'a>(session: Option<&'a str>, turn: Option<&'a str>) -> EventContext<'a> {
        EventContext {
            surface: ClientSurface::ClaudeCode,
            workspace_id: Some(WORKSPACE),
            session_id: session,
            prompt_id: None,
            turn_id: turn,
            permission_mode: Some(PermissionMode::Default),
        }
    }

    fn metadata() -> MeasurementContext {
        MeasurementContext {
            purpose: MeasurementPurpose::Normal,
            arm: ObservationArm::GuiOn,
            session_started_at_ms: Some(1_000),
            ..MeasurementContext::default()
        }
    }

    /// 2026-09-06: launcherの時刻で失効した開始証拠を復活させず、来歴も混ぜない。
    #[test]
    fn host_start_evidence_is_required_and_its_time_basis_is_explicit() {
        let launch = MeasurementContext::from_hints(
            None,
            Some("actual"),
            Some("900"),
            Some("actual"),
            1_000,
        );
        let unresolved = launch.with_session_start(None);
        assert_eq!(unresolved.launcher_started_at_ms, Some(900));
        assert_eq!(unresolved.session_started_at_ms, None);
        assert_eq!(unresolved.purpose, MeasurementPurpose::Normal);
        let resolved = unresolved.with_session_start(Some(SessionStartEvidence {
            observed_at_ms: 1_000,
            generation: 1,
        }));
        assert_eq!(resolved.session_started_at_ms, Some(1_000));
        assert_eq!(
            resolved.session_start_source,
            Some(SessionStartSource::HostStartEvent)
        );
        assert_eq!(resolved.launcher_started_at_ms, Some(900));
        assert_eq!(
            resolved.with_session_start(None).session_started_at_ms,
            None
        );
    }

    /// 2026-09-06: 観測時刻の境界とhost実開始の証明を区別し、来歴が違うwriteを結合しない。
    #[test]
    fn observed_start_boundary_and_generation_control_the_cohort() {
        let query = ObservationQuery {
            since_ms: 1_000,
            until_ms: 5_000,
            workspace_id: WORKSPACE.into(),
            manual_exclusions: vec![],
        };
        let host = metadata().with_session_start(Some(SessionStartEvidence {
            observed_at_ms: 1_000,
            generation: 1,
        }));
        let events = [1_100, 1_200, 1_300]
            .into_iter()
            .enumerate()
            .map(|(index, at)| ObservedEvent {
                event: hook(
                    context(Some("host"), Some(&format!("turn-{index}"))),
                    at,
                    host,
                ),
                state: Some("emitted".into()),
            })
            .collect::<Vec<_>>();
        let refs = events.iter().collect::<Vec<_>>();
        assert!(classify_session(&refs, &query).2.is_empty());
        let mut outside = events;
        for event in &mut outside {
            event.event.measurement.session_started_at_ms = Some(999);
        }
        assert!(
            classify_session(&outside.iter().collect::<Vec<_>>(), &query)
                .2
                .contains(&SessionExclusionReason::OutsideWindow)
        );
        outside[2].event.measurement.session_start_generation = Some(2);
        assert!(
            classify_session(&outside.iter().collect::<Vec<_>>(), &query)
                .2
                .contains(&SessionExclusionReason::ConflictingStart)
        );
        let mut quality = ObservationQuality::default();
        observe_quality(&mut quality, &outside[0]);
        assert_eq!(quality.host_start_event_timed_events, 1);
        assert_eq!(quality.launcher_timed_events, 0);
    }

    fn hook(context: EventContext<'_>, at: i64, mut metadata: MeasurementContext) -> LedgerEvent {
        let delivery = render_hook_delivery(
            &json!({"hits": [{"id": "notes/secret-title"}], "documents": [{"id": "notes/secret-title", "text": "本文秘密"}]}),
            context.surface.hook_output_budget(),
        )
        .unwrap();
        metadata.final_output = Some(FinalOutputFlags {
            status_line_present: metadata.arm == ObservationArm::GuiOn,
            cadence_line_present: false,
            status_omitted_for_budget: false,
        });
        LedgerEvent::hook(
            context,
            at,
            HookObservation::OutputPrepared {
                stats: delivery.stats,
                timings: HookTimings::default(),
                cap_assumption: CapAssumption::for_surface(context.surface),
            },
        )
        .unwrap()
        .with_measurement(metadata)
        .unwrap()
    }

    fn write(
        session: Option<&str>,
        at: i64,
        tool: WriteTool,
        outcome: WriteOutcome,
    ) -> LedgerEvent {
        let mut context = context(session, None);
        context.permission_mode = None;
        let mut metadata = metadata();
        if session.is_none() {
            metadata.session_started_at_ms = None;
        }
        LedgerEvent::write(context, at, &format!("call-{at}"), tool, outcome, None)
            .unwrap()
            .with_measurement(metadata)
            .unwrap()
    }

    fn append(path: &Path, event: &LedgerEvent) -> AppendOutcome {
        let outcome = append_at_time(path, event, 3_000).unwrap();
        if let Some(receipt) = &outcome.receipt {
            finalize_hook_at(path, receipt, HookEmissionOutcome::Emitted).unwrap();
        }
        outcome
    }

    fn add_session(path: &Path, session: &str, metadata: MeasurementContext) {
        for index in 0..3 {
            append(
                path,
                &hook(
                    context(Some(session), Some(&format!("turn-{index}"))),
                    1_100 + index,
                    metadata,
                ),
            );
        }
    }

    fn rate_group(report: &ObservationReport) -> &ObservationStratum {
        report
            .strata
            .iter()
            .find(|group| {
                group.surface == ClientSurface::ClaudeCode
                    && group.arm == ObservationArm::GuiOn
                    && group.permission_mode == Some(PermissionMode::Default)
            })
            .unwrap()
    }

    fn excluded(report: &ObservationReport, reason: SessionExclusionReason) -> u64 {
        report
            .strata
            .iter()
            .flat_map(|group| &group.session_exclusions)
            .filter(|entry| entry.reason == reason)
            .map(|entry| entry.sessions)
            .sum()
    }

    #[test]
    fn launch_hints_require_exact_host_session_without_retaining_raw_identity() {
        let context = MeasurementContext::from_hints(
            None,
            Some("session-secret"),
            Some("1000"),
            Some("session-secret"),
            2_000,
        );
        assert_eq!(context.purpose, MeasurementPurpose::Normal);
        assert_eq!(context.session_started_at_ms, Some(1_000));
        assert!(
            !serde_json::to_string(&context)
                .unwrap()
                .contains("session-secret")
        );
        for (launch_id, time, host_id) in [
            (Some("other"), Some("1000"), Some("session-secret")),
            (Some("session-secret"), Some("-1"), Some("session-secret")),
            (Some("session-secret"), Some("2001"), Some("session-secret")),
            (
                Some("session-secret"),
                Some("invalid"),
                Some("session-secret"),
            ),
            (Some("session-secret"), Some("1000"), None),
        ] {
            assert_eq!(
                MeasurementContext::from_hints(None, launch_id, time, host_id, 2_000)
                    .session_started_at_ms,
                None
            );
        }
        assert_eq!(
            MeasurementContext::from_hints(Some("diagnostic"), None, None, None, 2_000).purpose,
            MeasurementPurpose::Diagnostic
        );
        assert_eq!(
            MeasurementContext::from_hints(Some("invalid"), None, None, None, 2_000).purpose,
            MeasurementPurpose::Unknown
        );
        let legacy: MeasurementContext = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.purpose, MeasurementPurpose::Unknown);
    }

    #[test]
    fn fixed_window_joins_successes_to_same_denominator_and_deduplicates_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        add_session(&path, "successful-session", metadata());
        add_session(&path, "failure-session", metadata());
        let success = write(
            Some("successful-session"),
            1_300,
            WriteTool::Update,
            WriteOutcome::Success,
        );
        append(&path, &success);
        assert_eq!(
            append(&path, &success).disposition,
            AppendDisposition::Deduplicated
        );
        append(
            &path,
            &write(
                Some("failure-session"),
                1_400,
                WriteTool::Propose,
                WriteOutcome::Error,
            ),
        );
        append(
            &path,
            &write(
                Some("write-only-session"),
                1_500,
                WriteTool::Propose,
                WriteOutcome::Success,
            ),
        );
        append(
            &path,
            &hook(
                context(Some("write-only-session"), Some("one-turn")),
                1_450,
                metadata(),
            ),
        );
        let mut unrelated = context(Some("other-kb"), Some("turn"));
        unrelated.workspace_id = Some(OTHER_WORKSPACE);
        append(&path, &hook(unrelated, 1_500, metadata()));
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        let group = rate_group(&report);
        assert_eq!(group.qualifying_sessions, 2);
        assert_eq!(group.successful_write_sessions, 1);
        assert_eq!(group.session_write_rate, Some(0.5));
        assert_eq!(excluded(&report, SessionExclusionReason::TooFewPrompts), 1);
        assert_eq!(
            report
                .strata
                .iter()
                .map(|group| group.observed_events)
                .sum::<u64>(),
            10
        );
        let text = serde_json::to_string(&report).unwrap();
        for secret in [
            "successful-session",
            "failure-session",
            "session_hash",
            "event_id",
            "secret-title",
            "本文秘密",
            OTHER_WORKSPACE,
        ] {
            assert!(!text.contains(secret), "{secret}");
        }
    }

    #[test]
    fn codex_counts_remain_separate_from_actual_session_rates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        for index in 0..3 {
            let turn = format!("turn-{index}");
            let mut context = context(Some("codex-session"), Some(&turn));
            context.surface = ClientSurface::CodexCli;
            append(&path, &hook(context, 1_100 + index, metadata()));
        }
        let mut event = write(None, 1_300, WriteTool::Propose, WriteOutcome::Success);
        event.surface = ClientSurface::CodexCli;
        append(&path, &event);
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert!(
            report
                .strata
                .iter()
                .all(|group| group.session_write_rate.is_none()
                    && group.rate_unavailable_reason
                        == Some(RateUnavailableReason::UnsupportedSurface))
        );
        assert_eq!(
            report
                .strata
                .iter()
                .map(|group| group.normal_counts.propose_successes)
                .sum::<u64>(),
            1
        );
        assert_eq!(
            report
                .strata
                .iter()
                .map(|group| group.normal_counts.hook_output_emitted)
                .sum::<u64>(),
            3
        );
    }

    #[test]
    fn diagnostic_or_manual_event_excludes_its_entire_retained_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        add_session(&path, "diagnostic-session", metadata());
        let mut diagnostic = metadata();
        diagnostic.purpose = MeasurementPurpose::Diagnostic;
        append(
            &path,
            &hook(
                context(Some("diagnostic-session"), Some("later")),
                2_100,
                diagnostic,
            ),
        );
        add_session(&path, "manual-session", metadata());
        append(
            &path,
            &write(
                Some("manual-session"),
                1_500,
                WriteTool::Propose,
                WriteOutcome::Success,
            ),
        );
        let mut query = query();
        query.manual_exclusions.push(ExclusionWindow {
            since_ms: 1_500,
            until_ms: 1_501,
        });
        let report = observation_summary_at_time(&path, &query, 3_000).unwrap();
        assert_eq!(excluded(&report, SessionExclusionReason::Diagnostic), 1);
        assert_eq!(
            excluded(&report, SessionExclusionReason::ManualExclusion),
            1
        );
        assert_eq!(excluded(&report, SessionExclusionReason::OutsideWindow), 1);
        assert_eq!(
            report
                .strata
                .iter()
                .map(|group| group.diagnostic_events)
                .sum::<u64>(),
            3
        );
        assert_eq!(
            report
                .strata
                .iter()
                .map(|group| group.manual_excluded_events)
                .sum::<u64>(),
            4
        );
        assert!(
            report
                .strata
                .iter()
                .all(|group| group.normal_counts.hook_output_emitted == 0
                    && group.normal_counts.propose_successes == 0
                    && group.qualifying_sessions == 0)
        );
    }

    #[test]
    fn unknown_mixed_and_boundary_sessions_are_not_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let variants = [
            (
                "unknown-purpose",
                MeasurementContext {
                    purpose: MeasurementPurpose::Unknown,
                    ..metadata()
                },
            ),
            (
                "unknown-arm",
                MeasurementContext {
                    arm: ObservationArm::Unknown,
                    ..metadata()
                },
            ),
            (
                "environment-off",
                MeasurementContext {
                    arm: ObservationArm::EnvironmentOff,
                    ..metadata()
                },
            ),
            (
                "unknown-start",
                MeasurementContext {
                    session_started_at_ms: None,
                    ..metadata()
                },
            ),
            (
                "starts-before",
                MeasurementContext {
                    session_started_at_ms: Some(999),
                    ..metadata()
                },
            ),
        ];
        for (session, metadata) in variants {
            add_session(&path, session, metadata);
        }
        add_session(&path, "mixed-arm", metadata());
        append(
            &path,
            &hook(
                context(Some("mixed-arm"), Some("off")),
                1_400,
                MeasurementContext {
                    arm: ObservationArm::GuiOff,
                    ..metadata()
                },
            ),
        );
        add_session(&path, "mixed-permission", metadata());
        let mut changed = context(Some("mixed-permission"), Some("plan"));
        changed.permission_mode = Some(PermissionMode::Plan);
        append(&path, &hook(changed, 1_400, metadata()));
        add_session(&path, "unknown-permission", metadata());
        let mut unknown = context(Some("unknown-permission"), Some("unknown"));
        unknown.permission_mode = None;
        append(&path, &hook(unknown, 1_400, metadata()));
        add_session(&path, "excluded-permission", metadata());
        let mut excluded_context = context(Some("excluded-permission"), Some("bypass"));
        excluded_context.permission_mode = Some(PermissionMode::BypassPermissions);
        append(&path, &hook(excluded_context, 1_400, metadata()));
        add_session(&path, "crosses-end", metadata());
        append(
            &path,
            &hook(context(Some("crosses-end"), Some("end")), 2_000, metadata()),
        );
        add_session(&path, "conflicting-start", metadata());
        append(
            &path,
            &hook(
                context(Some("conflicting-start"), Some("new-start")),
                1_400,
                MeasurementContext {
                    session_started_at_ms: Some(1_001),
                    ..metadata()
                },
            ),
        );
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        for reason in [
            SessionExclusionReason::UnknownPurpose,
            SessionExclusionReason::UnknownArm,
            SessionExclusionReason::EnvironmentOff,
            SessionExclusionReason::UnverifiedStart,
            SessionExclusionReason::MixedArm,
            SessionExclusionReason::UnknownPermission,
            SessionExclusionReason::ExcludedPermission,
            SessionExclusionReason::ConflictingStart,
        ] {
            assert_eq!(excluded(&report, reason), 1, "{reason:?}");
        }
        assert_eq!(excluded(&report, SessionExclusionReason::OutsideWindow), 2);
        assert_eq!(
            excluded(&report, SessionExclusionReason::MixedPermission),
            2
        );
        assert!(
            report
                .strata
                .iter()
                .all(|group| group.qualifying_sessions == 0)
        );
    }

    #[test]
    fn filtered_prompts_do_not_qualify_but_output_failure_is_an_observed_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        for index in 0..2 {
            append(
                &path,
                &hook(
                    context(Some("filtered"), Some(&format!("t-{index}"))),
                    1_100 + index,
                    metadata(),
                ),
            );
        }
        let filtered = LedgerEvent::hook(
            context(Some("filtered"), Some("filtered")),
            1_200,
            HookObservation::Filtered {
                reason: HookFilterReason::SlashCommand,
                timings: HookTimings::default(),
            },
        )
        .unwrap()
        .with_measurement(metadata())
        .unwrap();
        append(&path, &filtered);
        for index in 0..3 {
            let event = hook(
                context(Some("failed-output"), Some(&format!("t-{index}"))),
                1_200 + index,
                metadata(),
            );
            let outcome = append_at_time(&path, &event, 3_000).unwrap();
            if index == 1 {
                finalize_hook_at(
                    &path,
                    &outcome.receipt.unwrap(),
                    HookEmissionOutcome::StdoutFailed,
                )
                .unwrap();
            }
        }
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        let group = rate_group(&report);
        assert_eq!(group.qualifying_sessions, 1);
        assert_eq!(group.normal_counts.hook_output_prepared, 2);
        assert_eq!(group.normal_counts.hook_stdout_failed, 1);
        assert_eq!(group.quality.unfinalized_outputs, 2);
        assert_eq!(group.quality.emitted_status_lines, 2);
        assert_eq!(excluded(&report, SessionExclusionReason::TooFewPrompts), 1);
    }

    #[test]
    fn missing_old_schema_and_retention_do_not_turn_unknown_into_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("ledger.sqlite3");
        let missing = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert!(!missing.exists);
        assert!(missing.strata.is_empty());
        assert!(!path.parent().unwrap().exists());
        add_session(&path, "session", metadata());
        let old = hook(context(Some("old-session"), Some("old")), 1_400, metadata());
        append(&path, &old);
        let conn = Connection::open(&path).unwrap();
        let mut payload = serde_json::to_value(&old).unwrap();
        payload.as_object_mut().unwrap().remove("measurement");
        let payload = serde_json::to_string(&payload).unwrap();
        conn.execute(
            "UPDATE ledger_events SET payload=?1, receipt_hash=?2 WHERE event_id=?3",
            params![payload, digest(&["prepared", &payload]), old.event_id],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert_eq!(
            report
                .strata
                .iter()
                .map(|group| group.quality.unspecified_measurement_events)
                .sum::<u64>(),
            1
        );
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            excluded(&report, SessionExclusionReason::UnverifiedStart),
            1
        );
        let retained =
            observation_summary_at_time(&path, &query(), (RETENTION_DAYS + 1) * DAY_MS).unwrap();
        assert!(!retained.retention_covers_period);
        assert_eq!(
            rate_group(&retained).rate_unavailable_reason,
            Some(RateUnavailableReason::IncompletePeriod)
        );
        let open = observation_summary_at_time(&path, &query(), 1_999).unwrap();
        assert!(!open.period_closed);
        assert!(rate_group(&open).session_write_rate.is_none());
    }

    #[test]
    fn reductions_have_known_denominators_and_diagnostic_quality_is_separate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let mut reduced = write(
            Some("normal"),
            1_300,
            WriteTool::Update,
            WriteOutcome::Success,
        );
        reduced.measurement.update_warnings = Some(UpdateWarningFlags {
            body_reduced: true,
            relations_reduced: true,
        });
        append(&path, &reduced);
        append(
            &path,
            &write(
                Some("unknown"),
                1_400,
                WriteTool::Update,
                WriteOutcome::Success,
            ),
        );
        let mut diagnostic = write(
            Some("diagnostic"),
            1_500,
            WriteTool::Update,
            WriteOutcome::Success,
        );
        diagnostic.measurement.purpose = MeasurementPurpose::Diagnostic;
        diagnostic.measurement.update_warnings = Some(UpdateWarningFlags {
            body_reduced: true,
            relations_reduced: false,
        });
        append(&path, &diagnostic);
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        let group = &report.strata[0];
        assert_eq!(group.quality.body_reduced_updates, 2);
        assert_eq!(group.normal_quality.body_reduced_updates, 1);
        assert_eq!(group.normal_quality.update_warning_known, 1);
        assert_eq!(group.normal_quality.update_warning_unknown, 1);
    }

    #[test]
    fn invalid_metadata_and_windows_are_rejected_without_creating_storage() {
        let event = write(
            Some("session"),
            1_300,
            WriteTool::Propose,
            WriteOutcome::Success,
        );
        assert!(
            event
                .clone()
                .with_measurement(MeasurementContext {
                    session_started_at_ms: Some(1_301),
                    ..metadata()
                })
                .is_err()
        );
        assert!(
            event
                .clone()
                .with_measurement(MeasurementContext {
                    final_output: Some(FinalOutputFlags::default()),
                    ..metadata()
                })
                .is_err()
        );
        assert!(
            event
                .with_measurement(MeasurementContext {
                    update_warnings: Some(UpdateWarningFlags::default()),
                    ..metadata()
                })
                .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.sqlite3");
        let mut invalid = query();
        invalid.manual_exclusions.push(ExclusionWindow {
            since_ms: 1_001,
            until_ms: 1_000,
        });
        assert!(observation_summary_at_time(&path, &invalid, 3_000).is_err());
        assert!(!path.exists());
    }

    /// 2026-09-06: hostがwriteの会話IDを渡さない場合を、書込率0%へ読み替えない。
    #[test]
    fn unjoinable_writes_and_missing_linkage_suppress_a_misleading_zero_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        add_session(&path, "normal-session", metadata());
        let absent = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert_eq!(rate_group(&absent).qualifying_sessions, 1);
        assert_eq!(
            rate_group(&absent).rate_unavailable_reason,
            Some(RateUnavailableReason::UnverifiedWriteLinkage)
        );
        append(
            &path,
            &write(
                Some("normal-session"),
                1_300,
                WriteTool::Propose,
                WriteOutcome::Error,
            ),
        );
        let linked = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert_eq!(rate_group(&linked).session_write_rate, Some(0.0));
        assert!(rate_group(&linked).write_linkage_verified);
        append(
            &path,
            &write(None, 1_400, WriteTool::Update, WriteOutcome::Success),
        );
        let unlinked = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert_eq!(
            rate_group(&unlinked).rate_unavailable_reason,
            Some(RateUnavailableReason::UnverifiedWriteLinkage)
        );
        assert_eq!(rate_group(&unlinked).verified_write_linkage_events, 1);
        assert_eq!(rate_group(&unlinked).unverified_write_linkage_events, 1);
        assert!(rate_group(&unlinked).session_write_rate.is_none());
    }

    #[test]
    fn unknown_arm_unjoinable_writes_invalidate_each_observed_arm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        add_session(&path, "on-session", metadata());
        add_session(
            &path,
            "off-session",
            MeasurementContext {
                arm: ObservationArm::GuiOff,
                ..metadata()
            },
        );
        append(
            &path,
            &write(
                Some("on-session"),
                1_300,
                WriteTool::Propose,
                WriteOutcome::Success,
            ),
        );
        let mut off = write(
            Some("off-session"),
            1_400,
            WriteTool::Propose,
            WriteOutcome::Success,
        );
        off.measurement.arm = ObservationArm::GuiOff;
        append(&path, &off);
        let mut unknown = write(None, 1_500, WriteTool::Propose, WriteOutcome::Success);
        unknown.measurement.arm = ObservationArm::Unknown;
        append(&path, &unknown);
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert!(
            report
                .strata
                .iter()
                .filter(|group| group.qualifying_sessions > 0)
                .all(|group| group.rate_unavailable_reason
                    == Some(RateUnavailableReason::UnverifiedWriteLinkage))
        );
    }

    #[test]
    fn unknown_purpose_write_cannot_disappear_behind_one_verified_error_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        add_session(&path, "normal-session", metadata());
        append(
            &path,
            &write(
                Some("normal-session"),
                1_300,
                WriteTool::Propose,
                WriteOutcome::Error,
            ),
        );
        let mut unknown = write(None, 1_400, WriteTool::Propose, WriteOutcome::Success);
        unknown.measurement = MeasurementContext::default();
        append(&path, &unknown);
        let report = observation_summary_at_time(&path, &query(), 3_000).unwrap();
        assert_eq!(rate_group(&report).qualifying_sessions, 1);
        assert_eq!(rate_group(&report).successful_write_sessions, 0);
        assert_eq!(rate_group(&report).unverified_write_linkage_events, 1);
        assert_eq!(
            rate_group(&report).rate_unavailable_reason,
            Some(RateUnavailableReason::UnverifiedWriteLinkage)
        );
        assert!(rate_group(&report).session_write_rate.is_none());
    }
}
