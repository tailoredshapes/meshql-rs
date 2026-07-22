//! `hen_productivity` model + the pure recompute that derives it from a
//! hen's current lay_report set. Exact landed shape on all three
//! languages: `{id, henId, totalEggs, lastLaidAt}` — confirmed by reading
//! all three retrofits' `hen_productivity.schema.json`/`.graphql` directly.
//! No dedup-ledger field: see the reconciliation note at the top of this
//! plan for why a full recompute replaced the originally-planned
//! accumulate-plus-processedReportIds design.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HenProductivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "henId")]
    pub hen_id: String,
    #[serde(rename = "totalEggs")]
    pub total_eggs: i64,
    #[serde(rename = "lastLaidAt")]
    pub last_laid_at: String,
}

/// Pure recompute: derive the next hen_productivity state from `current`
/// (for its known id and last_laid_at baseline — `None` means this hen has
/// no productivity record yet) and `report_eggs`, the hen's FULL, freshly
/// fetched set of lay_report egg counts (never an incremental delta).
///
/// Idempotent by construction, which is what makes this safe under
/// at-least-once merkql delivery (domain-design.md: "A worker fold that
/// isn't deterministic/idempotent ... breaks ... at-least-once delivery"):
/// `total_eggs` is a sum over the CURRENT report set, so redelivering any
/// event recomputes the same total as long as the underlying lay_report
/// data hasn't changed — no dedup ledger needed. `last_laid_at` is
/// `max(current.last_laid_at, event_created_at_iso)`, a monotonic merge
/// that's idempotent for the same reason (never regresses, reapplying the
/// same input changes nothing). Both `event_created_at_iso` and any stored
/// `last_laid_at` must be fixed-offset ISO-8601 (`...Z`) for the string
/// comparison to agree with chronological order — the worker only ever
/// produces this format (see Task 8), so this holds by construction.
pub fn recompute(
    current: Option<&HenProductivity>,
    hen_id: &str,
    report_eggs: &[i64],
    event_created_at_iso: &str,
) -> HenProductivity {
    let total_eggs: i64 = report_eggs.iter().sum();
    let last_laid_at = match current {
        Some(c) if c.last_laid_at.as_str() >= event_created_at_iso => c.last_laid_at.clone(),
        _ => event_created_at_iso.to_string(),
    };
    HenProductivity {
        id: current.and_then(|c| c.id.clone()),
        hen_id: hen_id.to_string(),
        total_eggs,
        last_laid_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_report_on_a_hen_creates_fresh_state() {
        let next = recompute(None, "hen-1", &[3], "2026-07-22T08:00:00Z");
        assert_eq!(next.hen_id, "hen-1");
        assert_eq!(next.total_eggs, 3);
        assert_eq!(next.last_laid_at, "2026-07-22T08:00:00Z");
        assert_eq!(next.id, None);
    }

    #[test]
    fn second_distinct_report_recomputes_the_full_total_and_preserves_known_id() {
        let current = HenProductivity {
            id: Some("hp-99".to_string()),
            hen_id: "hen-1".to_string(),
            total_eggs: 3,
            last_laid_at: "2026-07-22T08:00:00Z".to_string(),
        };
        // Both reports' eggs, fetched fresh from the source — this is what
        // makes the recompute safe under redelivery: it never depends on
        // what was summed last time, only on what's true right now.
        let next = recompute(Some(&current), "hen-1", &[3, 2], "2026-07-23T08:00:00Z");
        assert_eq!(
            next.id,
            Some("hp-99".to_string()),
            "known id must be preserved"
        );
        assert_eq!(next.total_eggs, 5);
        assert_eq!(next.last_laid_at, "2026-07-23T08:00:00Z");
    }

    #[test]
    fn redelivering_the_same_event_over_unchanged_data_is_a_true_no_op() {
        // Proves the idempotency requirement domain-design.md demands for
        // at-least-once delivery: recomputing over the SAME report set with
        // the SAME (already-applied) event timestamp must reproduce
        // identical state, not double-count anything — no ledger needed.
        let current = HenProductivity {
            id: Some("hp-99".to_string()),
            hen_id: "hen-1".to_string(),
            total_eggs: 3,
            last_laid_at: "2026-07-22T08:00:00Z".to_string(),
        };
        let next = recompute(Some(&current), "hen-1", &[3], "2026-07-22T08:00:00Z");
        assert_eq!(
            next, current,
            "redelivery over unchanged source data must be a pure no-op"
        );
    }

    #[test]
    fn last_laid_at_never_regresses_when_an_older_event_is_redelivered() {
        let current = HenProductivity {
            id: Some("hp-99".to_string()),
            hen_id: "hen-1".to_string(),
            total_eggs: 5,
            last_laid_at: "2026-07-23T08:00:00Z".to_string(),
        };
        // An older event (e.g. the FIRST report, redelivered after the
        // second has already landed) must not move last_laid_at backwards.
        let next = recompute(Some(&current), "hen-1", &[3, 2], "2026-07-22T08:00:00Z");
        assert_eq!(next.last_laid_at, "2026-07-23T08:00:00Z");
        assert_eq!(next.total_eggs, 5);
    }
}
