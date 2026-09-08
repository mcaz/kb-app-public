//! 日別の集計も通常の要約と同じ検証・成功判定を通す。

use super::*;
use crate::client_surface::ClientFamily;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ObservationTrendFilter {
    #[default]
    All,
    Claude,
    Gpt,
}

impl ObservationTrendFilter {
    fn includes(self, surface: ClientSurface) -> bool {
        match self {
            Self::All => true,
            Self::Claude => surface.family() == ClientFamily::Claude,
            Self::Gpt => surface.family() == ClientFamily::Gpt,
        }
    }
}

pub(crate) struct DailySummary {
    pub has_observations: bool,
    pub days: Vec<SurfaceSummary>,
}

pub(crate) fn summarize(
    boundaries: &[i64; 15],
    until_ms: i64,
    filter: ObservationTrendFilter,
    workspace_id: &str,
) -> Result<DailySummary> {
    summarize_at(&runtime_path()?, boundaries, until_ms, filter, workspace_id)
}

pub(crate) fn summarize_at(
    path: &Path,
    boundaries: &[i64; 15],
    until_ms: i64,
    filter: ObservationTrendFilter,
    workspace_id: &str,
) -> Result<DailySummary> {
    let workspace = validate_workspace_id(workspace_id)?;
    let query = SummaryQuery {
        since_ms: boundaries[0],
        until_ms,
        workspace_id: Some(workspace.clone()),
    };
    // SurfaceSummaryを外へ返さないため、内部のsurface欄は日別合算に使わない。
    let mut days: Vec<_> = (0..14)
        .map(|_| Accumulator::new(ClientSurface::Unknown))
        .collect();
    let mut has_observations = false;
    scan_validated_events(path, &query, Some(&workspace), false, |event, state| {
        // family別でも不正なpayloadやreceiptを隠さないよう、共通検証を通った後に選別する。
        if !filter.includes(event.surface) {
            return Ok(());
        }
        let index = boundaries
            .partition_point(|boundary| *boundary <= event.observed_at_ms)
            .checked_sub(1)
            .context("日別観測の時刻が集計期間外")?;
        days.get_mut(index)
            .context("日別観測の時刻が集計期間外")?
            .observe(event, state)?;
        has_observations = true;
        Ok(())
    })?;
    Ok(DailySummary {
        has_observations,
        days: days.into_iter().map(Accumulator::finish).collect(),
    })
}
