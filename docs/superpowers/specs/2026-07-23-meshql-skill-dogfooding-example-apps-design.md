# Dogfooding the meshql skill suite: four example apps, built and overseen to harden the skills

**Date:** 2026-07-23
**Status:** Approved design, pre-implementation
**Depends on:** `meshql-iron` installed and validated in `meshql-rs`, `meshql`, `meshobj` (`docs/superpowers/specs/2026-07-23-meshql-iron-skill-design.md`, complete). `meshql-patterns` and `merkql-architecture` currently exist only in `meshql-rs` — porting them to `meshql` and `meshobj` is a prerequisite of this project, not already-done groundwork (see below).
**Unblocks:** nothing downstream yet.

## Motivation

The actual thing being validated here is not four applications — it's whether the meshql skill suite (`meshql-patterns` for backend authoring, `meshql-iron` for frontend consumption, `merkql-architecture` for cross-language event-pipeline reasoning) is *sufficient*, on its own, for an LLM agent with minimal prompting to build a real, working application on meshql without additional hand-holding. The four apps (CMS, Twitter Clone, Asset Management, CRM) are the test harness, not the deliverable. User's own framing: "make sure that an LLM can take the skill, and run with it to create fully formed applications with minimal prompting," with the author (this session, the agent with the most context on the skill suite) explicitly overseeing each build and using whatever friction surfaces to improve the skills.

This is the same acceptance-test methodology already used to validate `meshql-iron` itself (`docs/superpowers/specs/2026-07-23-meshql-iron-skill-design.md`'s Validation section — dispatch a fresh, context-free agent, see if the skill alone gets it right) — scaled from "one page against an existing deployment" to "a whole application, backend and frontend, from nothing."

Starter-kit-quality example apps are a welcome byproduct of this process, not its goal. If a build comes out polished enough to serve as a real reference example (like `logistics` in `meshql`), that's a bonus, not a requirement.

## Non-Goals

- **Pre-designing each app's entity model, event/domain split, or architecture.** Doing this myself before dispatching a builder agent would hand it the exact reasoning that's supposed to be under test. The builder agent derives its own domain model from the skill suite plus a one- or two-sentence brief.
- **Cloud-native ports of the worker/queue layer (AWS/Azure).** Explicitly future work, not part of this project. Per the user: only the worker + queue implementation should need to change when that day comes ("porting the workers from merkql to cloud native techs") — the event/domain-mesh shape of each app shouldn't need to change at all. Nothing in this project should be built in a way that makes that future port harder, but building it now is out of scope.
- **A second, polished rebuild pass per app.** Considered as "Approach C" during brainstorming and rejected — one autonomous build per app, reviewed and used to patch the skill, is the whole mechanism. A polish pass on any individual app can be decided later, per app, once its raw output is seen — not committed to up front.
- **Building all four apps in every language (12 builds).** Rejected as scope explosion relative to the actual goal. One language per app, with a Rust `merkql` sidecar where the app's main language isn't Rust, gives real cross-language coverage of all three `meshql-iron`/`meshql-patterns` installations at 4x cost instead of 12x.

## Prerequisite: port `meshql-patterns` and `merkql-architecture` to `meshql` and `meshobj`

Checked directly: only `meshql-rs/.claude/skills/` has `meshql-patterns` and `merkql-architecture`; `meshql/.claude/skills/` and `meshobj/.claude/skills/` have only `meshql-iron`. For the two apps assigned to those repos (Twitter Clone, Asset Management — see below), a builder agent would have frontend-consumption guidance but *nothing* teaching it the backend event/domain-mesh pattern or the `merkql` cross-language constraint. That's not a fair test of the skill suite; it's a test of a skill that doesn't exist there yet. Both apps also need `merkql-architecture` specifically, since both require a Rust `merkql` sidecar per the "Merkql scope" decision below.

**This is a bigger lift than porting `meshql-iron` was.** `meshql-iron` is language-agnostic by construction — it teaches consuming HTTP surfaces (manifest, REST, GraphQL), not how a server is wired, so its three installations are near-identical modulo two citation lines. `meshql-patterns` teaches *authoring* a backend, and its content is inherently Rust-specific: real code (`RootConfig::builder()`, `Arc::new(MongoRepository::new(...))`), real file paths (`meshql-core/src/lib.rs`), and a fluent builder API that Java and TypeScript don't share verbatim. Porting it means preserving the architectural content (the six invariants, the domain-design methodology, the decision guide, the anti-patterns) while replacing every code example with that language's actual wiring shape — closer to writing two new skills using `meshql-patterns` as a structural template than to a copy-and-patch job.

**Known head start, to confirm and use during implementation, not fully resolve here:** both `meshql/CLAUDE.md` and `meshobj/CLAUDE.md` already contain substantial backend-authoring documentation for their own languages (`meshql/CLAUDE.md` has a full "MeshQL Integration Guide for AI Agents" section — Envelope shape, `RootConfig`/`QueryConfig` records, resolver configs, storage configs, GraphQL schema pattern, REST endpoint table, honesty headers, common patterns, anti-patterns, all in Java; `meshobj/CLAUDE.md` is 298 lines covering its own Lerna/Yarn-based structure). These should be read closely and adapted into proper `.claude/skills/meshql-patterns/` installations rather than written from scratch — likely most of the real content already exists, just not packaged as an auto-loading Skill with the invariant-first structure `meshql-patterns` uses. Confirming how much of each CLAUDE.md transfers directly is implementation work, not something to pre-solve in this spec.

Both ports get `merkql-architecture` too — that skill's content (the embedded-library constraint, the "connector + worker must be Rust, projection can be any language" rule) is language-agnostic prose about an architectural boundary, not language-specific code, so it should port far more like `meshql-iron` did: near-verbatim, no restructuring needed.

## Merkql scope: every app uses merkql, via a Rust sidecar where the app isn't Rust

Confirmed against `merkql-architecture`: merkql is an embedded Rust library, not a network service. Two pieces touch it directly and must be Rust — the storage connector for the event meshlette, and the worker that reads events off it. What the worker writes to (the projection's REST surface) is language-agnostic, since that's a plain HTTP call.

Decision (user-confirmed): **every one of the four apps uses merkql for its event→worker→domain slice**, not just the Rust ones. For Twitter Clone (TS) and Asset Management (Java), this means the event meshlette + worker are a small Rust process/crate (living in that app's own directory, or wherever the builder agent judges appropriate), while the domain projection it writes to — and everything else in the app — stays in the app's assigned language. This is a real, deliberate test of the exact cross-language composition `merkql-architecture` exists to describe, not an edge case to avoid.

## The four apps: assignment and build order

| Order | App | Repo / language | Why this pairing |
|---|---|---|---|
| 1 | CMS | `meshql-rs` (Rust) | Draft → revise → publish maps naturally onto event-mesh; fully Rust, no cross-language complexity — establishes the baseline loop cheaply. |
| 2 | CRM | `meshql-rs` (Rust) | Mixed-mode test: contacts/companies are plain CRUD, an activity/interaction log naturally event-sources into an engagement-score-style projection. Still fully Rust — validates the "don't force event ceremony onto plain CRUD entities" guidance before adding cross-language difficulty. |
| 3 | Twitter Clone | `meshobj` (TS) + Rust merkql sidecar | Tweets are naturally create-only events; feeds/timelines are projections — same pattern as CMS/CRM, different domain, first cross-language build. |
| 4 | Asset Management | `meshql` (Java) + Rust merkql sidecar | Mostly CRUD (assets, categories, locations) with an assignment/checkout event feeding an availability projection — second cross-language build, benefits from whatever the Twitter Clone build taught about the sidecar pattern. |

Order is a deliberate complexity ramp: prove the oversight loop mechanics on pure-Rust apps first (cheap iteration, no sidecar composition to debug), then take on cross-language composition once the skill content from rounds 1–2 has already been hardened.

## Per-app process

**1. Builder agent dispatch.** One fresh subagent per app, zero context from any prior app in this project or from this spec. The brief is short and deliberately does not name entities or architecture:

> Build a [CMS / Twitter clone / asset management app / CRM] on meshql. Work in the `[meshql-rs / meshobj / meshql]` repo. Use merkql for the event log — [for the Rust apps: "you're already in Rust, so this is direct" / for TS and Java apps: "your app's main language is [TS/Java]; merkql itself is Rust-only, so the event meshlette's connector and worker will need to be a small Rust piece — check `merkql-architecture` if you're unsure how that composes"]. Build both backend and frontend, fully working end to end, with tests. Report what you built and why.

**2. "Fully formed" bar.** Not feature-complete software — a working demonstrator: backend entities appropriate to the domain (however many the agent judges necessary), full REST+GraphQL surfaces, at least one real event→worker→domain slice via merkql, a working frontend (vanilla per this project's standing frontend conventions, built using `meshql-iron`) demonstrating the core workflow end to end, and passing tests.

**3. My review.** After each build, I read the actual artifacts — not just the agent's self-report — looking specifically for: places it had to guess because a skill was silent or ambiguous; places it got the event/domain split wrong or right for the wrong reason; places it hit a real bug (the way the `examples/farm` payload-prefix bug surfaced during `meshql-iron`'s own validation); places the cross-language sidecar composition (apps 3–4) went differently than `merkql-architecture` implies.

**4. Skill patch.** Findings become direct edits to the relevant skill doc(s) — `meshql-iron`, `meshql-patterns`, or `merkql-architecture`, in whichever repo(s) are affected — plus a regression test where the finding was an actual code bug rather than a documentation gap. This follows the same lightweight pattern already used in this session (the `examples/farm` template-bug fix): no full brainstorming cycle per finding, just a direct fix with a test proving it, committed on its own.

**5. Move to the next app.** Sequential, not parallel, specifically so patches from app *N* are already in place before app *N+1*'s builder agent starts — the whole value of this project is that the fourth app should need far fewer skill corrections than the first.

## Validation

Success isn't a fixed pass/fail gate — it's a trend. Each app's review (step 3 above) should surface fewer *new* categories of skill gap than the one before it, especially between apps 1→2 (same language, should be nearly clean by app 2) and 3→4 (both cross-language, app 4 should reuse app 3's sidecar-composition lessons rather than rediscovering them). If app 4 still surfaces first-time-seen gaps as large as app 1's, that's a signal the skill content isn't generalizing and is worth a harder look before calling this project done.

## Summary of what this design intentionally leaves open

- **Cloud-native worker ports (AWS/Azure)** — explicitly future work, not touched here.
- **Whether any app gets a polish pass** into true starter-kit quality — decided per app, after seeing its raw build, not committed to now.
- **Exactly how much of `meshql/CLAUDE.md` and `meshobj/CLAUDE.md` transfers directly into their new `meshql-patterns` installations** — a real question for implementation, not resolved here.
- **Whether the `meshql-patterns`/`merkql-architecture` port ends up large enough to deserve its own spec/plan cycle** rather than being one phase of this project's implementation plan — a call to make once the porting work is actually scoped in the implementation plan.
