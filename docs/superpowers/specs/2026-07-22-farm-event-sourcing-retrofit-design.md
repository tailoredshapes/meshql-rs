# Farm event-sourcing retrofit

**Date:** 2026-07-22
**Status:** Brainstormed, NOT yet spec-reviewed or planned — captured under time pressure ahead of a context compaction. Treat as a faithful record of a long design conversation, not a final, vetted spec. Run it through the normal spec-review loop before writing an implementation plan.
**Depends on:** manifest parity (done, `2026-07-22-manifest-parity-design.md`)
**Blocks:** the `domain` field work (needs a real event-sourced example to group events by, not artificial tags on plain CRUD entities), which blocks the TS client library.
**Companion spec:** `2026-07-22-merkql-worker-pipeline-design.md` — the CDC bridge + worker that reads `lay_report` events and writes `hen_productivity`. That spec is Rust-only, shared across all three farm deployments; this spec is per-language (Rust, Java, TS each need their own retrofit).

## Motivation

`examples/farm` predates the actors/events/projections pattern this ecosystem later worked out via `examples/egg-economy` (Rust, Java only — TS has no equivalent). It's currently plain CRUD across all three languages: every entity is directly create/read/update/delete-able via REST, no event/projection split. That's now understood to be the wrong shape for the framework's own reference example, and it blocks two things: the `domain` field (grouping many events under an aggregate needs *real* events to group, not artificial tags bolted onto CRUD entities) and, eventually, the FE client library, which is explicitly modeled around "the only interaction with the system is via domain events" (pure event-driven design, confirmed 2026-07-22).

## Domain redesign

- **Actors — unchanged, stay directly CRUD-writable via REST**: `farm` (id, name), `coop` (id, name, farm_id), `hen` (id, name, coop_id, dob). Long-lived reference/identity data, no event history needed.
- **Event — repurpose the existing `lay_report` entity**: stops being directly updatable/deletable. Becomes write-only (create-only) and immutable — "a hen laid N eggs at this time." Submitted by the FE as a domain event: `POST /lay_report/api` with `{henId, eggs, timeOfDay}`. Once created, a `lay_report` record is never mutated or removed — a correction, if ever needed, is a new event, not an edit (matches "you can't update an entity, you can only submit an event to request a change" — confirmed 2026-07-22, and "CUDs are mostly administrative" — undo is itself just another event).
- **New projection — `hen_productivity`**: read-only from the FE's perspective (nothing ever POSTs to it *as a human/FE action*), built by folding `lay_report` events per hen (total eggs, last-laid timestamp, etc. — exact aggregate fields are an implementation decision, not settled here). Written by the worker described in the companion spec, via that entity's own restlette — never touched directly by any database write outside the normal REST path (see "single writer" below).

## The "single writer" invariant (framework principle, not new to this retrofit)

Confirmed 2026-07-22: **REST is the only write path in this architecture, for everyone, always — including workers.** Workers never get direct database access. The full path for any write, including one initiated by a backend worker, is: `REST (event) → store → CDC → worker → REST (domain)`. This retrofit's `hen_productivity` restlette is a completely ordinary restlette; the only thing unusual about it is *who* calls it (a worker process instead of a human via a browser), not *how* it's called.

## Manifest visibility: always advertise both surfaces

**Correction to the existing `egg-economy` manifest generator convention** (its "nouns must not advertise a rest surface" rule, built during the original `meshql-changes` work): that convention was based on a misunderstanding — since every meshlette's restlette exists regardless (workers need it), hiding it from the manifest doesn't reflect reality, it just obscures it. New rule, confirmed 2026-07-22: **the manifest is honest about what exists — it always advertises both `graph` and `api` surfaces for every entity that has both**, `hen_productivity` included. Restricting *who* can actually write there (FE vs. worker) is an authorization concern, not a documentation concern.

This reflects a broader framework principle stated directly: *"this is a framework — if a user decides to expose everything to open web, we're to assume it's for good reason. Likewise if they lock down individual verbs to given casbin roles — also their prerogative. Our mission is to make the right thing for them easy, not impose our own standards."* Don't build paternalistic hiding into the framework; make correct authorization easy instead (see below).

**Consequence for `examples/egg-economy`**: this same correction likely applies there too (drop its `ALL_VERBS`-based REST-surface filtering), but that's **out of scope for this spec** — flagging it as a related follow-up, not doing it here. Egg-economy's manifest generator and conformance tests (`manifest_conformance.rs`'s "nouns must not advertise a rest surface" assertion) would need updating if/when that's picked up.

## Auth: wire existing Casbin, don't build new framework capability

No new auth mechanism needed — `CasbinAuth` already exists and works in all three languages (Java `casbin_auth`, TS `casbin_auth`, Rust `meshql-casbin`). This retrofit just **configures** it for farm, replacing `NoAuth`:

- A `worker` role, authorized to write to `hen_productivity`.
- General/FE callers (no token, or a non-worker token) authorized to write to `farm`, `coop`, `hen`, `lay_report` (the actor CRUD + event submission surfaces) but **not** `hen_productivity`.
- Reads (GraphQL) stay open to everyone, matching the existing farm examples' current behavior — this retrofit is about write authorization, not read restriction.

Exact policy file format is per-language (Casbin model.conf/policy.csv in Java, equivalent config in TS/Rust) — a config/example task, not new engineering.

## Manifest generator changes (all three languages)

1. **Drop verb/noun filtering.** The generators built during manifest parity (Java's `ManifestGenerator.java`, TS's `manifest.ts`) already emit an `api` surface conditionally on whether a `.schema.json` file exists for that entity — that conditional logic **stays** (it's about whether a REST surface exists at all, which is still true — every entity here has one). What's being corrected is a *different, not-yet-built* filtering step (egg-economy's `ALL_VERBS`-style noun/verb hiding) that was never ported to farm's generators in the first place, so there's actually nothing to remove in the code delivered by manifest parity — this is confirming farm's *existing* generators (from the parity work) are already correct here, not a new change. Worth a conformance-test assertion confirming it stays that way as the retrofit lands (`hen_productivity` should show both surfaces, same as every other entity).
2. **Rust's `examples/farm` needs a manifest generator built from scratch.** The original manifest-parity work ported the *concept and reference algorithm* from `egg-economy`'s Rust generator to Java's and TS's `farm` — but Rust's own `farm` example was never given one; egg-economy already had it and farm didn't need it until now. This retrofit should add `ManifestGenerator`-equivalent Rust code (mirroring `egg-economy/src/manifest.rs`) to `examples/farm`, generate its `config/manifest.json`, wire `GET /manifest` via `run_ext` (matching egg-economy's existing pattern), and add the same three-test conformance suite Java/TS already have. This closes farm out to full three-language parity on the manifest itself, not just the domain retrofit.

## What's explicitly not decided here (needs settling during spec review or implementation planning)

- Exact `hen_productivity` aggregate fields (total eggs? per-day breakdown? last-laid timestamp? some combination?).
- Exact Casbin policy file shape per language (the *rule* is settled — worker-writes-hen_productivity, FE-writes-everything-else — the file format isn't).
- Whether existing farm integration tests (BDD/cucumber suites in all three languages) need rewriting given `lay_report`'s write contract changes (no more update/delete) and `hen_productivity` becomes a new read surface.
- Whether farm's existing README/docs (all three languages) need updating to describe the new event-sourced shape — very likely yes, not scoped in detail here.

## Out of scope

- `examples/egg-economy`'s own manifest generator/conformance-test correction (flagged above as a likely follow-up, not part of this work).
- The `domain` field itself — this spec exists specifically to unblock it, but adding the field is separate work, sequenced after this retrofit lands.
- The merkql CDC bridge and worker implementation — see the companion spec.
- The TS client library.
