# Reviewing an ingress connector branch

For the Salesforce, HubSpot and SAP branches, and every connector after them.
Ordered by how expensive the defect is to find later. Stop at the first **hard
gate** that fails.

References: `docs/ingress-connector-contract.md` for the reasoning,
`.claude/skills/building-ingress-connectors/SKILL.md` for the rules.

---

## Hard gates — reject the branch if any of these fail

### 1. Id derivation is one pure function, and it is tested for its collision

Find the function. There must be exactly one, and it must be reachable from a
unit test without a network.

- [ ] Two encodings/spellings of one source record produce **one** id.
      (Salesforce: the 15-char and 18-char forms of the same record.)
- [ ] Two records of different object types sharing a numeric id produce **two**
      ids. (HubSpot: this is the one to check first.)
- [ ] Composite-key components cannot be re-associated across the separator:
      `("A","BC")` and `("AB","C")` produce two ids. (SAP.)
- [ ] The id contains **no mutable field** — no modification timestamp, no
      status, no name, no owner. Read the function and check every input.
- [ ] The id is reproducible from whatever a **delete** notification carries.

Why this is gate 1: an id defect is unfixable by replay. Every envelope already
on the topic carries the wrong identity, and meshql has already interpreted the
duplicates as version chains.

### 2. The cursor is not a bare timestamp — **and the keyset has a ceiling**

- [ ] Find the value assigned to `SourceInfo::position`. If it is a lone
      `updated_at` / `SystemModstamp` / `lastmodifieddate`, **reject**.
- [ ] There is a test in which three records share one modification timestamp,
      and all three are delivered exactly once with distinct positions.
- [ ] The query predicate is checked against that test. `> cursor` on a bare
      timestamp skips ties permanently; `>= cursor` replays forever.
- [ ] **A composite `(ts, id)` keyset has a ceiling** — `AND ts <= now - lag` —
      and `lag` is configuration. Without it, a record committed into the open
      time bucket out of id order is skipped **permanently**. This is the fix for
      the watermark bug reintroducing the watermark bug, and it is the defect
      most likely to be present, because a composite keyset *looks* correct.
      There must be two tests: open-bucket record withheld until the ceiling
      passes, and a late low-id record into a passed bucket still delivered.
- [ ] **Timestamps normalised to UTC at the boundary**, with the source's actual
      timezone checked per endpoint rather than assumed. A DST spring-forward on
      an unnormalised cursor is a permanent skip.

### 2b. `created_at` is the modification timestamp

- [ ] Not `Utc::now()`, and not the record's creation date. `envelope_order`
      (`meshql-core/src/lib.rs:64-69`) makes `created_at` decide which version
      wins; wall-clock on an unordered feed lets the stale version win every read
      permanently, and the creation date makes every version tie.
- [ ] There is a test delivering two versions **out of order** and asserting the
      newer one still wins.
- [ ] Tombstone `created_at` sorts after the last live version.

### 2c. Fan-out positions

- [ ] Where one source event yields several `ChangeRecord`s (a merge, a batched
      page), only the **last** carries a position; the rest carry `None`. A crash
      between them otherwise loses the remainder permanently (`sink.rs:311-323`).

### 3. Contract tests run, against `wiremock`, in CI

**Know before reviewing:** `cert::certify` **cannot pass as written** for a
connector following the id rule — it hardcodes `"pre-1"`, `"live-1"`,
`"during-downtime"`, `"after-start"` (`cert.rs:123/135/215/243`) and a derived id
fails the comparison first. So:

- [ ] `certify_positions_are_present_and_distinct` (asserts nothing about ids)
      runs **as-is**.
- [ ] The other three are **transliterated** into the connector's test file,
      asserting the same properties against derived ids, with a comment saying
      they are pending `CertStore::envelope_id` (contract §5 G9).
- [ ] **Reject** a branch that made ids pass by injecting an identity derivation
      into the test — that disables the assertion that matters most.
- [ ] **Reject** a branch that edited the shared `cert.rs` to make its own tests
      pass. That change needs to land on its own, reviewed by all three.
- [ ] No `#[ignore]`. No `#[cfg(feature = "live")]`. No real tenant, no
      credentials read from the reviewer's environment.
- [ ] `certify_positions_are_present_and_distinct` is included — this is the one
      that catches gate 2.
- [ ] The poll interval is configurable and the test sets it low. If the test
      passes only because of a `sleep`, the interval is hardcoded.

### 4. No secrets in the config variant

- [ ] The new `SourceConfig` variant (`config.rs:112-155`) holds instance URL,
      object type, portal/account id — and nothing that would be redacted in a
      support ticket.
- [ ] Every credential is read from the environment, and the error message names
      the variables (the shape at `main.rs:83-88`).

### 5. Errors are classified correctly

Read every construction of `CdcError` in the new source.

- [ ] `UnusablePosition` is returned **only** for a cursor the server refused —
      expired replay id, expired delta token, unknown token, a cursor from
      another tenant.
- [ ] A **429** is not `UnusablePosition`. Under `snapshot_mode = "when_needed"`
      that turns a throttle into a full org re-snapshot, which produces more
      throttling.
- [ ] A **401** is not `UnusablePosition`, and does not reach the stream at all —
      the client refreshes and retries.
- [ ] `NoFeed` is used where a change feed exists but could not be established,
      not as a generic connection error.
- [ ] Nothing swallows an error and continues the stream. A retry that loses its
      place is a gap.

---

## Correctness review

### The envelope

- [ ] `authorized_tokens` come from **config**, not from the source record.
- [ ] `deleted: true` is a **new version with the same id**, never a dropped
      record and never `after: None` (`sink.rs:236-244`).
- [ ] The source system's own timestamp is in the **payload**, as a named field.
      `Envelope::created_at` is connector wall-clock unless the struct is built
      literally (`sqlite.rs:219-225`), and `source.ts_ms` does not survive a
      repository sink (`sink.rs:26-39`).
- [ ] If a worker needs to distinguish backfill from live traffic, that is
      materialised in the payload — `op: r` does not reach a non-merkql queue.

### Snapshot and resume

- [ ] The streaming position is captured **before** the backfill query runs
      (`source.rs:119-129`). Reversed, this is the classic CDC gap and it is
      invisible in every structural assertion.
- [ ] The final backfill record is tagged `Snapshot::Last`; earlier ones are
      `Snapshot::True` and may carry `position: None` (`mongo.rs:224-234`).
- [ ] The branch does **not** stage a mid-snapshot position with
      `snapshot_in_progress == false` to smuggle a resumable backfill past
      `offsets.rs:103-108`. Grep for how `Snapshot` flags are assigned. This is
      the tempting, wrong fix.
- [ ] The README states the backfill cost and that a failure restarts it.

### Rate limits and tokens

- [ ] Retry lives in the HTTP client, respects `Retry-After`, has jitter and a
      bounded attempt count.
- [ ] Token refresh is proactive (clock-based) as well as reactive (one 401 →
      one retry), and guarded so concurrent 401s cause one refresh.
- [ ] A failed *refresh* is fatal `CdcError::Backend` naming the credential —
      correct, and not something to soften.
- [ ] No new `CdcError` variant was added.

### The API's own limits

- [ ] The **result-window cap** is handled by windowing the query, not by paging
      until empty. Ask directly: "what happens at record 10,001?" There must be a
      test.
- [ ] The **field-selection** behaviour is handled: if the API returns only
      requested fields, the property list is refreshed periodically and the delta
      logged. There must be a test where the fake starts returning a new field.
- [ ] Pagination boundaries tested: exactly one full page, and one page plus one.

---

## Shared-file conflicts — check these on every branch, they all touch them

| File | What to verify |
|---|---|
| `config.rs:112-155` | variant added; no secrets |
| `config.rs:205-219` | arms added to `entity()` **and** `connector_name()` |
| `lib.rs:101-110` | `#[cfg(feature)] pub mod` |
| `main.rs:104-150` | arm in `open_source`; otherwise it compiles and fails at runtime |
| `Cargo.toml:39-44`, `:46-62` | feature; `reqwest` promoted from dev-dep if used at runtime; `wiremock` added to dev-deps |

- [ ] Whoever merges second actually re-ran the **other** connectors' tests, not
      just their own. A textual merge of `config.rs` can compile and still drop an
      arm.
- [ ] Did the branch join `default = [...]` (`Cargo.toml:40`)? If so, is dragging
      an HTTP/gRPC stack into every build of the binary intended?

---

## Documentation review — this is where the operator's runbook lives

The module doc comment must answer, in the house style (why, and the failure each
rule prevents — `sqlite.rs:1-53` and `postgres.rs:1-78` are the standard):

- [ ] **What the cursor is and when it expires.** This is the connector's MTTR
      budget. Salesforce CDC replay ids: 72 hours. SAP delta tokens: backend-
      defined, so the doc must say "unknown, confirm per system" rather than
      inventing a number.
- [ ] **Whether deletes surface**, and if not, that the projection only grows.
      Nothing downstream can infer this.
- [ ] **Whether it polls, and how often**, and — if it polls — the sentence
      explaining that the API has no change feed. Logged at startup too
      (`main.rs:44-50`).
- [ ] **The quota arithmetic**: requests per poll × polls per day × N object
      types against one org.
- [ ] Nothing described in the present tense that is not built
      (`meshql-patterns`, "Skills describe what exists").

---

## Cross-branch questions to ask once all three have landed

1. **Did the three id-derivation functions converge on one shape?** If they did
   not, either the rule in the skill is wrong or one of them is. Find out which
   before the fourth connector is written.
2. **Did any of them need something the trait could not express?** Compare
   against `docs/ingress-connector-contract.md` §5. A gap that two of three
   builders worked around independently is a framework change, not a coincidence.
3. **Did any of them make `run_connector`, `Resume` or `CdcError` changes?** Those
   are shared. A change landed beside one connector is a change the other two did
   not review.
4. **Do the three READMEs answer the same operator questions?** If a reader has
   to learn a different vocabulary per connector, the skill did not do its job.
5. **Was `cert::certify` genuinely run by all three**, or did one write its own
   approximation? An approximation is the thing `cert.rs:1-9` exists to prevent.
