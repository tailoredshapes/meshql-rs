//! Opt-in capacity metering: what a workload actually costs, from DynamoDB's own
//! accounting rather than from an estimate.
//!
//! DynamoDB will report the capacity a request consumed, but only if the request
//! asks — `ReturnConsumedCapacity`. Nothing in this crate asked, which is why the
//! certification suites attest *semantics* and say nothing about *cost*. This
//! module closes that gap for an operator who wants to know what their own
//! traffic bills.
//!
//! # Off by default, and it stays off
//!
//! A [`CapacityMeter`] is attached explicitly:
//!
//! ```no_run
//! # async fn example() -> meshql_core::Result<()> {
//! use meshql_dynamo::{CapacityMeter, DynamoRepository, DynamoSearcher, Rates};
//!
//! let meter = CapacityMeter::new();
//! let repo = DynamoRepository::new(None, "farms").await?.with_meter(meter.clone());
//! let searcher = DynamoSearcher::new(None, "farms").await?.with_meter(meter.clone());
//!
//! // ... serve some traffic ...
//!
//! let report = meter.snapshot();
//! println!("{report}");
//! println!("that traffic cost ${:.6}", report.cost_usd(&Rates::ON_DEMAND_US_EAST_1));
//! # Ok(())
//! # }
//! ```
//!
//! With no meter attached, `ReturnConsumedCapacity` is never set, so the request
//! on the wire is byte-identical to an unmetered build and there is nothing to
//! pay for and nothing to synchronise. With a meter attached the only difference
//! is a field in the *response*: `ReturnConsumedCapacity` cannot change which
//! items a request matches, so metering cannot change semantics. Pinned by
//! `tests/capacity_cost.rs::metering_does_not_change_results`.
//!
//! # Predicting, as well as measuring
//!
//! [`item_size_bytes`], [`write_units`] and [`read_units`] are the same
//! arithmetic DynamoDB bills by, so an operator can compute a bill for a
//! workload they have not run yet. Every one of them is checked against the real
//! meter in `tests/capacity_cost.rs` — a prediction nobody compared to a bill is
//! a guess.
//!
//! # What the numbers mean
//!
//! Capacity is reported in *units*, and units are what AWS prices:
//!
//! - a **write request unit** (WRU) covers one write of an item up to 1 KB;
//! - a **read request unit** (RRU) covers one *strongly* consistent read of up
//!   to 4 KB, or **two** eventually consistent reads of up to 4 KB — so an
//!   eventually consistent 4 KB read is **0.5** RRU.
//!
//! This crate never sets `ConsistentRead`, so every `Query` and `Scan` is
//! eventually consistent and reads are charged at the half rate. That is a
//! deliberate part of the cost model, not an oversight: version resolution reads
//! its own writes only as far as the replica lag, which the `at:` contract
//! already tolerates.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use aws_sdk_dynamodb::types::{AttributeValue, ConsumedCapacity, ReturnConsumedCapacity};

/// Which DynamoDB API a charge came from.
///
/// Deliberately the *API*, not the meshql operation: the point of the breakdown
/// is that a `Scan` and a `Query` cost differently by three orders of magnitude,
/// and an operator reading a report needs to see which one their traffic
/// provoked. `read(id, ..)` and a search with an `"id"` key are both one
/// [`Op::Query`], which is the whole claim the searcher's pushdown makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// `PutItem` — one per version written, including tombstones.
    PutItem,
    /// `Query` — `query_latest`, i.e. every by-id read and the searcher's one
    /// safe pushdown.
    Query,
    /// `Scan` — `scan_latest`, i.e. `list` and every search without an `"id"`.
    Scan,
}

impl Op {
    /// The three ops, in the order a report prints them.
    pub const ALL: [Op; 3] = [Op::PutItem, Op::Query, Op::Scan];

    /// Whether this op's capacity is read capacity (RRU) or write capacity (WRU).
    pub fn is_read(self) -> bool {
        matches!(self, Op::Query | Op::Scan)
    }

    fn label(self) -> &'static str {
        match self {
            Op::PutItem => "PutItem",
            Op::Query => "Query",
            Op::Scan => "Scan",
        }
    }
}

/// Capacity is always a multiple of 0.5 units, so units are accumulated as
/// thousandths in a `u64` — exact, lock-free, and no float atomics.
const MILLI: f64 = 1000.0;

#[derive(Debug, Default)]
struct OpCounters {
    /// Round trips: one per API call, so a paginated `Scan` counts every page.
    requests: AtomicU64,
    /// Consumed capacity units × 1000.
    milli_units: AtomicU64,
}

/// A shared, lock-free accumulator of the capacity a workload consumed.
///
/// Cheap to clone (`Arc` internally) and safe to share across tasks, so one
/// meter can cover a repository and a searcher over the same table, or every
/// table in a process.
///
/// Nothing here is a cost *estimate*. Every number came back from DynamoDB.
#[derive(Debug, Default)]
pub struct CapacityMeter {
    put_item: OpCounters,
    query: OpCounters,
    scan: OpCounters,
}

impl CapacityMeter {
    /// A fresh meter. Returned in an `Arc` because the useful thing to do with a
    /// meter is share it.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn counters(&self, op: Op) -> &OpCounters {
        match op {
            Op::PutItem => &self.put_item,
            Op::Query => &self.query,
            Op::Scan => &self.scan,
        }
    }

    /// Record one round trip and whatever capacity it reported.
    ///
    /// The round trip is counted even when `consumed` is `None`, because a
    /// request that reported no capacity still cost a round trip — and DynamoDB
    /// Local reports no capacity at all, which is precisely the failure this
    /// module exists to make visible rather than silent.
    pub fn record(&self, op: Op, consumed: Option<&ConsumedCapacity>) {
        let counters = self.counters(op);
        counters.requests.fetch_add(1, Ordering::Relaxed);
        if let Some(units) = consumed.and_then(|c| c.capacity_units()) {
            counters
                .milli_units
                .fetch_add((units * MILLI).round() as u64, Ordering::Relaxed);
        }
    }

    /// A consistent-enough point-in-time read of the counters.
    ///
    /// Counters are read one at a time under `Relaxed`, so a snapshot taken
    /// while requests are in flight can straddle them. That is the right
    /// trade for a meter: an operator sampling every minute cares about totals,
    /// not about a serialisable instant, and paying for a lock on the hot path
    /// to tidy up a boundary nobody can observe would be the wrong bargain.
    pub fn snapshot(&self) -> CapacityReport {
        let read = |op: Op| {
            let c = self.counters(op);
            OpStats {
                requests: c.requests.load(Ordering::Relaxed),
                capacity_units: c.milli_units.load(Ordering::Relaxed) as f64 / MILLI,
            }
        };
        CapacityReport {
            put_item: read(Op::PutItem),
            query: read(Op::Query),
            scan: read(Op::Scan),
        }
    }

    /// Zero every counter. Useful for per-request or per-interval accounting;
    /// [`CapacityReport::minus`] is the alternative that does not race with
    /// concurrent traffic.
    pub fn reset(&self) {
        for op in Op::ALL {
            let c = self.counters(op);
            c.requests.store(0, Ordering::Relaxed);
            c.milli_units.store(0, Ordering::Relaxed);
        }
    }
}

/// Requests and capacity for one [`Op`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OpStats {
    /// Round trips to DynamoDB. For a paginated `Scan` this is the page count,
    /// which is the thing that sets latency.
    pub requests: u64,
    /// Capacity units DynamoDB reported for those requests.
    pub capacity_units: f64,
}

impl OpStats {
    fn minus(self, earlier: Self) -> Self {
        Self {
            requests: self.requests.saturating_sub(earlier.requests),
            capacity_units: self.capacity_units - earlier.capacity_units,
        }
    }
}

/// A snapshot of a [`CapacityMeter`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CapacityReport {
    pub put_item: OpStats,
    pub query: OpStats,
    pub scan: OpStats,
}

impl CapacityReport {
    pub fn get(&self, op: Op) -> OpStats {
        match op {
            Op::PutItem => self.put_item,
            Op::Query => self.query,
            Op::Scan => self.scan,
        }
    }

    /// Read capacity units — `Query` plus `Scan`.
    pub fn read_units(&self) -> f64 {
        self.query.capacity_units + self.scan.capacity_units
    }

    /// Write capacity units — `PutItem`.
    pub fn write_units(&self) -> f64 {
        self.put_item.capacity_units
    }

    /// Total round trips across every op. The latency number, as
    /// [`Self::cost_usd`] is the money number.
    pub fn round_trips(&self) -> u64 {
        self.put_item.requests + self.query.requests + self.scan.requests
    }

    /// What this much capacity costs, in US dollars, at `rates`.
    ///
    /// Request capacity only. Storage is a function of the table, not of the
    /// traffic, so it is not in a traffic meter — see [`Rates::storage_usd`].
    pub fn cost_usd(&self, rates: &Rates) -> f64 {
        self.read_units() * rates.read_request_unit_usd
            + self.write_units() * rates.write_request_unit_usd
    }

    /// `self` minus an earlier snapshot, for interval accounting that does not
    /// need [`CapacityMeter::reset`] and therefore cannot lose a concurrent
    /// request's charge.
    pub fn minus(&self, earlier: &Self) -> Self {
        Self {
            put_item: self.put_item.minus(earlier.put_item),
            query: self.query.minus(earlier.query),
            scan: self.scan.minus(earlier.scan),
        }
    }
}

impl fmt::Display for CapacityReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{:<8}  {:>10}  {:>14}", "op", "requests", "capacity")?;
        for op in Op::ALL {
            let s = self.get(op);
            writeln!(
                f,
                "{:<8}  {:>10}  {:>14.1}",
                op.label(),
                s.requests,
                s.capacity_units
            )?;
        }
        write!(
            f,
            "{:<8}  {:>10}  {:>14}",
            "total",
            self.round_trips(),
            format!("{:.1} R / {:.1} W", self.read_units(), self.write_units())
        )
    }
}

/// Published AWS prices, so a report can be turned into money without the
/// caller hardcoding a rate that will be wrong next year.
///
/// Rates are per *unit*, not per million, because that is what a report counts.
#[derive(Debug, Clone, Copy)]
pub struct Rates {
    /// USD per read request unit.
    pub read_request_unit_usd: f64,
    /// USD per write request unit.
    pub write_request_unit_usd: f64,
    /// USD per GiB-month of table storage, before any free allowance.
    pub storage_usd_per_gib_month: f64,
    /// GiB of storage that are free each month. Zero if the account has no such
    /// allowance — see the note on [`Rates::ON_DEMAND_US_EAST_1`].
    pub free_storage_gib: f64,
}

impl Rates {
    /// DynamoDB on-demand, `us-east-1`, as returned by the **AWS Price List
    /// Query API on 2026-07-30**:
    ///
    /// | rate | usagetype | price |
    /// |---|---|---|
    /// | read request unit  | `ReadRequestUnits`  | $0.125 per million |
    /// | write request unit | `WriteRequestUnits` | $0.625 per million |
    /// | storage            | `TimedStorage-ByteHrs` | $0.25 per GB-month |
    ///
    /// The 25 GiB free storage allowance is **not** in the Price List API — it
    /// is an AWS Free Tier term, and for accounts opened after AWS restructured
    /// the free tier it may not apply at all. Set [`Self::free_storage_gib`] to
    /// zero to price a workload without it; a cost model that quietly assumes an
    /// allowance the client does not have is worse than no model.
    pub const ON_DEMAND_US_EAST_1: Rates = Rates {
        read_request_unit_usd: 0.125 / 1_000_000.0,
        write_request_unit_usd: 0.625 / 1_000_000.0,
        storage_usd_per_gib_month: 0.25,
        free_storage_gib: 25.0,
    };

    /// Monthly storage cost for a table of `bytes`, after the free allowance.
    pub fn storage_usd(&self, bytes: u64) -> f64 {
        let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        (gib - self.free_storage_gib).max(0.0) * self.storage_usd_per_gib_month
    }
}

// ---- prediction ----

/// One write request unit covers an item up to this many bytes.
pub const WRITE_UNIT_BYTES: u64 = 1024;

/// One read request unit covers this many bytes, strongly consistent — or twice
/// as many eventually consistent.
pub const READ_UNIT_BYTES: u64 = 4096;

/// Write request units to store an item of `bytes`.
///
/// `PutItem` is billed on the *rounded-up kilobyte*, so an item one byte over
/// 1024 costs twice as much as one exactly at it. That cliff is why
/// `tests/capacity_cost.rs` writes items either side of the boundary rather
/// than a comfortable middling size.
pub fn write_units(bytes: u64) -> f64 {
    bytes.div_ceil(WRITE_UNIT_BYTES).max(1) as f64
}

/// Read request units to read `bytes`.
///
/// `eventually_consistent` halves the charge, and this crate never asks for a
/// consistent read, so the `true` case is what every meshql read costs.
///
/// The rounding is on the **aggregate** for `Query` and `Scan`, not per item:
/// DynamoDB sums the items a request processed and rounds that total up to a
/// 4 KB boundary. A `Scan` page examining 250 items of 1 KiB is charged
/// `ceil(256000/4096) * 0.5 = 31.5` RRU, not `250 * 0.5`. Getting this wrong
/// overstates a scan's cost eightfold, so
/// `tests/capacity_cost.rs::a_search_costs_the_aggregate_not_the_per_item_rounding`
/// pins it against the meter.
pub fn read_units(bytes: u64, eventually_consistent: bool) -> f64 {
    let units = bytes.div_ceil(READ_UNIT_BYTES).max(1) as f64;
    if eventually_consistent {
        units / 2.0
    } else {
        units
    }
}

/// The size DynamoDB bills an item at, in bytes.
///
/// Per the *Item sizes and formats* section of the DynamoDB developer guide: the
/// item size is the sum, over attributes, of the UTF-8 length of the attribute
/// name plus the size of its value, where
///
/// - `S` and `B` are their own byte length;
/// - `N` is about one byte per two significant digits, plus one, plus one more
///   for a sign;
/// - `BOOL` and `NULL` are one byte;
/// - `L` and `M` are 3 bytes of container overhead plus the sizes of the
///   elements, plus **1 byte per element**, and a map's keys count as strings.
///
/// This is the storage/write size. It excludes the ~100 bytes per item of index
/// overhead that DynamoDB adds for *storage* billing but not for capacity —
/// see [`storage_bytes_per_item`].
///
/// Approximate by construction (the `N` rule is documented as approximate), so
/// it is validated against real reported capacity in
/// `tests/capacity_cost.rs::the_predicted_item_size_matches_the_metered_write`
/// rather than trusted.
pub fn item_size_bytes(item: &std::collections::HashMap<String, AttributeValue>) -> u64 {
    item.iter()
        .map(|(name, value)| name.len() as u64 + attribute_size_bytes(value))
        .sum()
}

/// Size of one attribute *value*, excluding its name.
pub fn attribute_size_bytes(value: &AttributeValue) -> u64 {
    match value {
        AttributeValue::S(s) => s.len() as u64,
        AttributeValue::N(n) => number_size_bytes(n),
        AttributeValue::B(b) => b.as_ref().len() as u64,
        AttributeValue::Bool(_) => 1,
        AttributeValue::Null(_) => 1,
        AttributeValue::Ss(items) => 3 + items.iter().map(|s| s.len() as u64 + 1).sum::<u64>(),
        AttributeValue::Ns(items) => {
            3 + items.iter().map(|n| number_size_bytes(n) + 1).sum::<u64>()
        }
        AttributeValue::Bs(items) => {
            3 + items
                .iter()
                .map(|b| b.as_ref().len() as u64 + 1)
                .sum::<u64>()
        }
        AttributeValue::L(items) => {
            3 + items
                .iter()
                .map(|v| attribute_size_bytes(v) + 1)
                .sum::<u64>()
        }
        AttributeValue::M(map) => {
            3 + map
                .iter()
                .map(|(k, v)| k.len() as u64 + attribute_size_bytes(v) + 1)
                .sum::<u64>()
        }
        // A variant this SDK version does not know about. Zero is the honest
        // answer — a wrong guess would silently skew a cost model — and no
        // meshql envelope produces one, since `convert.rs` emits only S, N,
        // BOOL, NULL, L and M.
        _ => 0,
    }
}

/// `N` costs roughly one byte per two significant digits, plus one, plus one
/// more for a sign.
///
/// **Leading zeroes are trimmed; trailing ones are not.** The developer guide
/// says both are, and for a decimal fraction that is clearly right — `0.5000` is
/// `0.5`. Applying it to a large integer is not: it predicts that `ca_ns` =
/// `1785412800000000000` costs 5 bytes rather than 11, because eleven of its
/// digits are trailing zeroes.
///
/// That prediction is wrong, and it was caught by measurement rather than by
/// reading: an envelope the trimming model called exactly 1024 bytes — built at
/// a round instant, so its nanosecond timestamp ended in zeroes — metered **2**
/// write units, not 1. Removing the trailing-zero trim brings the same item to
/// 1030 bytes, which is consistent. See
/// `tests/gsi_cost.rs::write_amplification_is_one_unit_per_index`.
///
/// Not trimming is also the right default for a *cost* model independently of
/// which reading of the documentation is correct: it can over-predict a bill by
/// a few bytes per number and can never under-predict one.
fn number_size_bytes(n: &str) -> u64 {
    let (negative, digits) = match n.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, n),
    };
    let significant = digits
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .trim_start_matches('0')
        .len()
        .max(1) as u64;
    significant.div_ceil(2) + 1 + u64::from(negative)
}

/// Bytes a stored item occupies for **storage** billing: its item size plus
/// DynamoDB's per-item overhead.
///
/// The developer guide's *Best practices for storage* puts this overhead at
/// ~100 bytes per item, and it is the reason append-only storage compounds a
/// little faster than `V × S` suggests: at 1 KiB items it is a 10% surcharge on
/// every version, forever.
pub const ITEM_STORAGE_OVERHEAD_BYTES: u64 = 100;

/// Item size plus [`ITEM_STORAGE_OVERHEAD_BYTES`].
pub fn storage_bytes_per_item(item_size: u64) -> u64 {
    item_size + ITEM_STORAGE_OVERHEAD_BYTES
}

// ---- plumbing ----

/// `ReturnConsumedCapacity::Total` when metering, `None` when not.
///
/// A `None` return means the builder is left untouched and the request goes out
/// exactly as an unmetered build would send it.
pub(crate) fn return_consumed_capacity(
    meter: Option<&CapacityMeter>,
) -> Option<ReturnConsumedCapacity> {
    meter.map(|_| ReturnConsumedCapacity::Total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn write_units_round_up_at_the_kilobyte() {
        assert_eq!(write_units(1), 1.0);
        assert_eq!(write_units(1024), 1.0);
        assert_eq!(
            write_units(1025),
            2.0,
            "one byte over 1 KB doubles the charge"
        );
        assert_eq!(write_units(2048), 2.0);
        assert_eq!(write_units(2049), 3.0);
        // An empty item is not free.
        assert_eq!(write_units(0), 1.0);
    }

    #[test]
    fn an_eventually_consistent_read_is_half_a_unit_per_four_kilobytes() {
        assert_eq!(read_units(1, true), 0.5);
        assert_eq!(read_units(4096, true), 0.5);
        assert_eq!(read_units(4097, true), 1.0);
        assert_eq!(read_units(4096, false), 1.0);
        // The aggregate rule: 250 items of 1 KiB in one page is 31.5 RRU, and
        // *not* 125 — the eightfold error that makes a scan look survivable.
        assert_eq!(read_units(250 * 1024, true), 31.5);
        assert_ne!(read_units(250 * 1024, true), 125.0);
    }

    #[test]
    fn number_sizes_follow_the_documented_rule() {
        assert_eq!(number_size_bytes("0"), 2); // one significant digit
        assert_eq!(number_size_bytes("7"), 2);
        assert_eq!(number_size_bytes("42"), 2);
        assert_eq!(number_size_bytes("1234"), 3);
        assert_eq!(number_size_bytes("-42"), 3, "a sign costs a byte");
        // Leading zeroes are trimmed...
        assert_eq!(number_size_bytes("0042"), 2);
        // ...trailing ones are NOT. `100` is three digits, not one: trimming
        // them predicted 5 bytes for a 19-digit `ca_ns` ending in zeroes and
        // the meter said 11.
        assert_eq!(number_size_bytes("100"), 3);
        let ca_ns = "1785412800000000000";
        assert_eq!(
            number_size_bytes(ca_ns),
            11,
            "a round nanosecond timestamp costs the same as any other"
        );
    }

    #[test]
    fn a_map_costs_its_keys_its_values_and_its_overhead() {
        let mut inner = HashMap::new();
        inner.insert("name".to_string(), AttributeValue::S("alpha".to_string()));
        // 3 container + (4 key + 5 value + 1 element) = 13
        assert_eq!(attribute_size_bytes(&AttributeValue::M(inner)), 13);
        // An empty map is just the overhead.
        assert_eq!(attribute_size_bytes(&AttributeValue::M(HashMap::new())), 3);
        // A list of two 1-byte strings: 3 + (1+1) + (1+1) = 7
        assert_eq!(
            attribute_size_bytes(&AttributeValue::L(vec![
                AttributeValue::S("a".to_string()),
                AttributeValue::S("b".to_string()),
            ])),
            7
        );
    }

    #[test]
    fn a_meter_with_no_reported_capacity_still_counts_the_round_trip() {
        // This is DynamoDB Local's behaviour, and the reason a report shows
        // requests and capacity separately: 4 round trips at zero capacity is
        // the signature of an endpoint that does not meter, which must look
        // different from 4 free requests.
        let meter = CapacityMeter::new();
        for _ in 0..4 {
            meter.record(Op::Scan, None);
        }
        let report = meter.snapshot();
        assert_eq!(report.scan.requests, 4);
        assert_eq!(report.scan.capacity_units, 0.0);
        assert_eq!(report.round_trips(), 4);
    }

    #[test]
    fn a_report_splits_read_capacity_from_write_capacity() {
        let meter = CapacityMeter::new();
        let consumed = |units: f64| ConsumedCapacity::builder().capacity_units(units).build();
        meter.record(Op::PutItem, Some(&consumed(2.0)));
        meter.record(Op::Query, Some(&consumed(0.5)));
        meter.record(Op::Scan, Some(&consumed(31.5)));

        let report = meter.snapshot();
        assert_eq!(report.write_units(), 2.0);
        assert_eq!(report.read_units(), 32.0);
        assert_eq!(report.round_trips(), 3);

        // 32 RRU at $0.125/M plus 2 WRU at $0.625/M.
        let expected = 32.0 * 0.125e-6 + 2.0 * 0.625e-6;
        let actual = report.cost_usd(&Rates::ON_DEMAND_US_EAST_1);
        assert!((actual - expected).abs() < 1e-15, "{actual} vs {expected}");
    }

    #[test]
    fn minus_gives_an_interval_without_resetting() {
        let meter = CapacityMeter::new();
        let consumed = ConsumedCapacity::builder().capacity_units(0.5).build();
        meter.record(Op::Query, Some(&consumed));
        let mark = meter.snapshot();
        meter.record(Op::Query, Some(&consumed));
        meter.record(Op::Query, Some(&consumed));

        let interval = meter.snapshot().minus(&mark);
        assert_eq!(interval.query.requests, 2);
        assert_eq!(interval.query.capacity_units, 1.0);
    }

    #[test]
    fn no_meter_means_no_return_consumed_capacity_on_the_wire() {
        assert!(return_consumed_capacity(None).is_none());
        assert_eq!(
            return_consumed_capacity(Some(&CapacityMeter::default())),
            Some(ReturnConsumedCapacity::Total)
        );
    }

    #[test]
    fn storage_is_free_until_the_allowance_runs_out() {
        let rates = Rates::ON_DEMAND_US_EAST_1;
        let gib = 1024u64 * 1024 * 1024;
        assert_eq!(rates.storage_usd(20 * gib), 0.0);
        // 45 GiB is 20 GiB billable at $0.25.
        assert!((rates.storage_usd(45 * gib) - 5.0).abs() < 1e-9);

        // ...and with the allowance turned off, every byte is billable.
        let no_free = Rates {
            free_storage_gib: 0.0,
            ..rates
        };
        assert!((no_free.storage_usd(20 * gib) - 5.0).abs() < 1e-9);
    }
}
