# Meshql Skill Dogfooding: Four Example Apps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Validate and harden the meshql skill suite (`meshql-patterns`, `meshql-iron`, `merkql-architecture`) by having minimally-prompted, context-free agents build four real applications (CMS, CRM, Twitter Clone, Asset Management) end to end, reviewing each build for skill gaps and patching the skills before moving to the next app.

**Architecture:** Phase 0 ports `meshql-patterns` and `merkql-architecture` into `meshql` (Java) and `meshobj` (TypeScript) — currently `meshql-rs`-only — since two of the four apps need them there. Phases 1–4 each dispatch one fresh builder agent per app with a short, entity-agnostic brief, then the plan's author (not a subagent — the session with full context on the skill suite) reviews the actual artifacts and patches whichever skill doc(s) the review finds gaps in.

**Tech Stack:** Markdown (skill content), Rust/Java/TypeScript (the four apps, per the language assignments below), merkql (event log, all four apps), vanilla frontends per `meshql-iron`.

**Grounding facts verified before writing this plan** (do not re-verify):
- `meshql-patterns` and `merkql-architecture` exist only in `meshql-rs/.claude/skills/`. `meshql` and `meshobj` currently have only `meshql-iron` installed.
- `meshql/CLAUDE.md` (491 lines) already documents most of what a Java `meshql-patterns` needs: Envelope shape, `RootConfig`/`QueryConfig`/resolver-config records, storage configs (Mongo/Postgres), the GraphQL schema pattern including the `createdAt` honesty opt-in, the REST endpoint table including honesty headers, "Building Applications: Patterns & Pitfalls" (REST identity model, CDC pipeline patterns, internal vs. external resolvers, frontend stack conventions, Docker Compose conventions), and a working reference (`examples/logistics`).
- `meshobj/CLAUDE.md` (299 lines) documents the TS equivalent: monorepo/plugin structure (`core/`, `repos/`), the **HOCON config-file format** (not a fluent code builder like Rust/Java — `restlettes`/`graphlettes` arrays with `storage`/`schema`/`rootConfig` keys), the `Plugin` interface new adapters implement, and its own honesty section (`core/restlette/src/crud.ts`, `repos/*/src/*Searcher.ts`).
- `meshql-patterns/SKILL.md` (meshql-rs) structure to mirror: frontmatter, "The six invariants," "Honesty: as-of freshness," "Naming and layout conventions," "Minimal entity wiring," "Deployment model," "Decision guide," "Anti-patterns to flag" — plus `references/{adding-an-entity,domain-design,federation,storage-adapters}.md`.
- All four apps use merkql for their event→worker→domain slice (user-confirmed). For Twitter Clone (TS) and Asset Management (Java), the event meshlette's storage connector and the worker are necessarily Rust (merkql is an embedded Rust library — see `merkql-architecture`); everything else stays in the app's assigned language.
- Build order: CMS (Rust) → CRM (Rust) → Twitter Clone (TS + Rust sidecar) → Asset Management (Java + Rust sidecar) — deliberate complexity ramp.

---

## Phase 0: Port `meshql-patterns` and `merkql-architecture` to `meshql` and `meshobj`

### Task 0.1: Port `merkql-architecture` to `meshql` (near-verbatim)

**Files:**
- Create: `meshql/.claude/skills/merkql-architecture/SKILL.md`

- [ ] **Step 1: Read the source**

Read `meshql-rs/.claude/skills/merkql-architecture/SKILL.md` in full.

- [ ] **Step 2: Copy with no content changes**

This skill's content is architecture-level prose about an embedding constraint (merkql is a Rust library, not a network service) — it doesn't reference Rust code specifics beyond the fact that the connector/worker "must be Rust," which is equally true from a Java reader's perspective. Copy the file verbatim to `meshql/.claude/skills/merkql-architecture/SKILL.md`. Do not add a Java-specific example — the existing "Concretely, for a farm-style event → projection pipeline backed by merkql" section already uses generic language (`lay_report`, `hen_productivity`) that reads fine regardless of which language the reader's own meshlettes are in.

- [ ] **Step 3: Verify frontmatter is intact**

```bash
head -5 meshql/.claude/skills/merkql-architecture/SKILL.md
```
Expected: starts with `---`, has `name: merkql-architecture` and a `description:` line, closing `---`.

- [ ] **Step 4: Commit**

```bash
cd /tank/repos/tailoredshapes/meshql
git add .claude/skills/merkql-architecture
git commit -m "Port merkql-architecture skill from meshql-rs (verbatim, language-agnostic content)"
```

### Task 0.2: Port `merkql-architecture` to `meshobj` (near-verbatim)

Same as Task 0.1, targeting `meshobj/.claude/skills/merkql-architecture/SKILL.md` and committed in the `meshobj` repo.

- [ ] Copy `meshql-rs/.claude/skills/merkql-architecture/SKILL.md` to `meshobj/.claude/skills/merkql-architecture/SKILL.md` verbatim.
- [ ] Verify frontmatter intact (same check as Task 0.1 Step 3).
- [ ] Commit in `meshobj`: `git add .claude/skills/merkql-architecture && git commit -m "Port merkql-architecture skill from meshql-rs (verbatim, language-agnostic content)"`.

### Task 0.3: Write `meshql-patterns` for `meshql` (Java)

This is real authoring, not a copy — see the plan header's grounding facts for what already exists in `meshql/CLAUDE.md` to draw from.

**Files:**
- Create: `meshql/.claude/skills/meshql-patterns/SKILL.md`
- Create: `meshql/.claude/skills/meshql-patterns/references/adding-an-entity.md`
- Create: `meshql/.claude/skills/meshql-patterns/references/domain-design.md`
- Create: `meshql/.claude/skills/meshql-patterns/references/federation.md`
- Create: `meshql/.claude/skills/meshql-patterns/references/storage-adapters.md`

- [ ] **Step 1: Read the source structure and the target material**

Read, in full:
- `meshql-rs/.claude/skills/meshql-patterns/SKILL.md` and all four of its `references/*.md` files (the structural and architectural template — invariants, decision guide shape, anti-patterns list).
- `meshql/CLAUDE.md` in full (the Java-specific source material — already read once during this plan's research; re-read to have it fresh when drafting).
- `meshql/examples/logistics/README.md` and `meshql/examples/farm/src/main/java/**/Main.java` (or equivalent) for a real, current example of Java entity wiring to quote from, the way `meshql-patterns` quotes `examples/farm/src/main.rs` for Rust.

- [ ] **Step 2: Draft `SKILL.md`**

Same section shape as the Rust version (frontmatter with `name: meshql-patterns`, `description:` describing when to use it; "The six invariants" — these are backend-agnostic, port near-verbatim from the Rust version's invariant list, since CQRS-by-convention/immutable-envelopes/temporal-everywhere/authorization/pluggable-storage/pick-your-scale apply identically to the Java implementation; "Honesty: as-of freshness" — draw from `meshql/CLAUDE.md`'s own "GraphQL Schema Pattern" and "REST API Endpoints" sections, which already document this for Java; "Naming and layout conventions" — adapt the table to Java's actual conventions (check `examples/farm/config/` for real path examples); "Minimal entity wiring" — replace the Rust `RootConfig::builder()` snippet with Java's actual builder/record pattern, quoting real code from `examples/logistics` or `examples/farm`; "Deployment model" — Java's equivalent of "compose your own binary" (Maven dependency composition, per `meshql/CLAUDE.md`'s "Maven Dependencies" section); "Decision guide" — same five bullets, pointing at this skill's own `references/*.md`; "Anti-patterns to flag" — port the Rust list's architectural anti-patterns (missing `at: Float`, hard deletes, unfiltered `deleted`, unfiltered `authorized_tokens`, business logic in adapters, cross-entity joins in restlettes) since these are invariant violations regardless of language; drop or adapt the Rust-specific ones (runtime plugin loading — check whether this applies to Java's Maven-based composition model at all, or whether Java has its own equivalent gotcha from `meshql/CLAUDE.md`'s "Building Applications: Patterns & Pitfalls" section to use instead, e.g. the REST identity model gotcha, the CDC pipeline gotchas, or the internal-vs-external resolver gotcha).

- [ ] **Step 3: Draft the four reference docs**

Each mirrors its Rust counterpart's scope, rewritten with Java specifics:
- `adding-an-entity.md`: how to add a new entity in Java — GraphQL schema file, JSON schema file, `QueryConfig`/`GraphletteConfig`/`RestletteConfig` wiring, referencing `meshql/CLAUDE.md`'s "Adding a New Entity" numbered list and "Quick Start Pattern" code block directly.
- `domain-design.md`: the events/projections/workers methodology — this section is the most language-agnostic of the four (it's a modeling methodology, not code), so lean on the Rust version's structure and content, adapting only the code-shaped bits (worker wiring) to Java, and cite `meshql/CLAUDE.md`'s "CDC Pipeline Patterns" section (phased processing, stable consumer group IDs, Debezium envelope differences) as Java-specific worker-building guidance the Rust version doesn't have an equivalent for.
- `federation.md`: internal vs. external resolvers — `meshql/CLAUDE.md`'s "Internal vs External Resolvers" section already has this fully worked out with real code (`InternalSingletonResolverConfig`, `InternalVectorResolverConfig`); adapt directly.
- `storage-adapters.md`: the `Plugin`/`Repository`/`Searcher` interfaces and certification suite, adapted from `meshql/CLAUDE.md`'s "Key Interfaces" table and "Storage Configs" section, referencing `meshql/repos/certification` (confirmed to exist earlier this session — `SearcherCertification.java`) as the Java equivalent of `meshql-core/src/testing.rs`.

- [ ] **Step 4: Verify frontmatter and internal cross-references**

```bash
head -5 meshql/.claude/skills/meshql-patterns/SKILL.md
grep -o 'references/[a-z-]*\.md' meshql/.claude/skills/meshql-patterns/SKILL.md | sort -u
ls meshql/.claude/skills/meshql-patterns/references/
```
Confirm every file the `SKILL.md` decision guide references under `references/` actually exists, and vice versa (no orphaned reference file the `SKILL.md` never points to).

- [ ] **Step 5: Commit**

```bash
cd /tank/repos/tailoredshapes/meshql
git add .claude/skills/meshql-patterns
git commit -m "Add meshql-patterns skill for Java, adapted from meshql-rs's version and meshql/CLAUDE.md"
```

### Task 0.4: Write `meshql-patterns` for `meshobj` (TypeScript)

Same shape as Task 0.3, targeting `meshobj`.

**Files:**
- Create: `meshobj/.claude/skills/meshql-patterns/SKILL.md`
- Create: `meshobj/.claude/skills/meshql-patterns/references/adding-an-entity.md`
- Create: `meshobj/.claude/skills/meshql-patterns/references/domain-design.md`
- Create: `meshobj/.claude/skills/meshql-patterns/references/federation.md`
- Create: `meshobj/.claude/skills/meshql-patterns/references/storage-adapters.md`

- [ ] **Step 1: Read the source structure and the target material**

Read, in full:
- `meshql-rs/.claude/skills/meshql-patterns/SKILL.md` and its four `references/*.md` files (same template as Task 0.3).
- `meshobj/CLAUDE.md` in full (re-read fresh — TS-specific source material, notably the HOCON config format, which is structurally different from Rust/Java's code-based builder pattern).
- One of `meshobj/examples/farm/` or `meshobj/examples/twofarms/` (whichever is more current/complete — check both) for real HOCON config and wiring code to quote.

- [ ] **Step 2: Draft `SKILL.md`**

Same section shape, with one structural difference to call out explicitly in "Minimal entity wiring": TS deployments are **configured via HOCON files**, not composed via code the way Rust's `main.rs`/`lib.rs` or Java's `Main.java` are — quote a real `.conf` snippet from `meshobj/CLAUDE.md`'s "Configuration System" section (the `port`/`graphlettes`/`restlettes` HOCON shape) rather than trying to force a code-builder example that doesn't match how this implementation actually works. "The six invariants" port near-verbatim (language-agnostic). "Honesty: as-of freshness" draws directly from `meshobj/CLAUDE.md`'s own "Honesty: as-of freshness" section (already written, just needs to be moved into skill form). "Deployment model" is TS's Lerna/Yarn workspace + `Plugin` interface composition (per `meshobj/CLAUDE.md`'s monorepo structure section) rather than Cargo or Maven. "Anti-patterns to flag" ports the language-agnostic invariant violations from the Rust list, plus TS-specific gotchas if `meshobj/CLAUDE.md`'s "Security Architecture & Common Review Pitfalls" section has any that are really architecture anti-patterns rather than false-positive security flags (most of that section is explicitly about what NOT to flag as a vulnerability, so check carefully before importing anything from it).

- [ ] **Step 3: Draft the four reference docs**

- `adding-an-entity.md`: adding an entity via HOCON config + GraphQL/JSON schema files, referencing `meshobj/CLAUDE.md`'s "Configuration System" section directly.
- `domain-design.md`: events/projections/workers methodology, same as Task 0.3's approach — mostly language-agnostic content from the Rust version, adapted wiring examples.
- `federation.md`: "Cross-Service Resolution" per `meshobj/CLAUDE.md`, plus the HOCON `resolvers` config key shape.
- `storage-adapters.md`: the `Plugin` interface (`createRepository`/`createSearcher`/`cleanup`) from `meshobj/CLAUDE.md`'s "Plugin System" section, and `core/cert`'s BDD certification suite as the TS equivalent of the Rust certification tests.

- [ ] **Step 4: Verify frontmatter and internal cross-references** (same check as Task 0.3 Step 4, `meshobj` paths).

- [ ] **Step 5: Commit**

```bash
cd /tank/repos/tailoredshapes/meshobj
git add .claude/skills/meshql-patterns
git commit -m "Add meshql-patterns skill for TypeScript, adapted from meshql-rs's version and meshobj/CLAUDE.md"
```

### Task 0.5: Phase 0 sanity check

- [ ] Confirm all three repos now have all three skills installed:

```bash
for repo in meshql-rs meshql meshobj; do
  echo "=== $repo ==="
  ls /tank/repos/tailoredshapes/$repo/.claude/skills/
done
```
Expected: each lists `merkql-architecture`, `meshql-iron`, `meshql-patterns`.

---

## Phase 1: CMS (`meshql-rs`, Rust)

### Task 1.1: Dispatch the builder agent

- [ ] Dispatch a fresh Agent (general-purpose, zero prior context, `isolation: "worktree"` recommended so the build doesn't collide with other work on `meshql-rs` `main`) with exactly this brief:

> Build a CMS on meshql. Work in the `meshql-rs` repo. Use merkql for the event log — you're already in Rust, so this is direct. Build both backend and frontend, fully working end to end, with tests. Report what you built and why.

- [ ] Let it run to completion (this is a large task — expect a long-running dispatch). Do not provide additional guidance beyond this brief unless the agent asks a clarifying question; answering questions is fine, proactively steering its design decisions is not (that would undermine the point of the test).

### Task 1.2: Review the build

- [ ] Read the actual code and content the agent produced — not just its self-report. Check specifically:
  - Did it correctly identify which entities should be event-mesh vs. domain-mesh, and can you tell *why* it made that call (does it match `event-vs-domain-mesh.md`'s detection guidance, or did it guess)?
  - Does the event meshlette's data actually flow through merkql → a real worker → the domain meshlette's restlette, or did it fake/skip part of the pipeline?
  - Does the frontend follow `meshql-iron`'s guidance (manifest discovery, honesty timestamps, no hardcoded query names)?
  - Do the tests actually exercise the write-then-read-back path (the same category of gap the `examples/farm` payload-prefix bug was), or are they superficial?
  - Note every place it had to guess because a skill was silent, or got something wrong that a skill should have prevented.

### Task 1.3: Patch the skill(s)

- [ ] For each finding from Task 1.2, make a direct, minimal edit to the relevant skill doc(s) (`meshql-iron`, `meshql-patterns`, and/or `merkql-architecture`, in `meshql-rs`). If the finding was an actual code bug (not just a documentation gap), add a regression test proving it, following the same pattern as `examples/farm/tests/farm_e2e_cert.rs` earlier in this project.
- [ ] Commit each patch on its own, with a commit message explaining what the CMS build revealed (mirrors this project's own earlier `examples/farm` bug-fix commits).

---

## Phase 2: CRM (`meshql-rs`, Rust)

### Task 2.1: Dispatch the builder agent

- [ ] Dispatch a fresh Agent (general-purpose, zero prior context, `isolation: "worktree"`) with exactly this brief:

> Build a CRM on meshql. Work in the `meshql-rs` repo. Use merkql for the event log — you're already in Rust, so this is direct. Build both backend and frontend, fully working end to end, with tests. Report what you built and why.

- [ ] Let it run to completion, same ground rules as Task 1.1.

### Task 2.2: Review the build

- [ ] Same checklist as Task 1.2, plus specifically: CRM is meant to be a **mixed-mode test** (some entities plain CRUD, one naturally event-sourced, e.g. an activity/interaction log). Check whether the agent forced event/domain ceremony onto entities that didn't need it, or correctly left them as plain CRUD per `event-vs-domain-mesh.md`'s explicit anti-pattern guidance.

### Task 2.3: Patch the skill(s)

- [ ] Same process as Task 1.3.

---

## Phase 3: Twitter Clone (`meshobj`, TypeScript + Rust merkql sidecar)

### Task 3.1: Dispatch the builder agent

- [ ] Dispatch a fresh Agent (general-purpose, zero prior context, `isolation: "worktree"`) with exactly this brief:

> Build a Twitter clone on meshql. Work in the `meshobj` repo. Your app's main language is TypeScript; merkql itself is Rust-only, so the event meshlette's connector and worker will need to be a small Rust piece — check `merkql-architecture` if you're unsure how that composes across languages. Build both backend and frontend, fully working end to end, with tests. Report what you built and why.

- [ ] Let it run to completion, same ground rules as Task 1.1.

### Task 3.2: Review the build

- [ ] Same checklist as Task 1.2, plus specifically: this is the first cross-language build. Check whether the Rust sidecar (connector + worker) actually composes with the TS meshlettes the way `merkql-architecture` describes (worker writes to the projection's REST surface over HTTP, nothing tighter-coupled than that), and whether the agent found `merkql-architecture` on its own or got stuck without being told exactly where to look — that's itself a signal about whether `meshql-iron`'s or `meshql-patterns`' decision guides should point at `merkql-architecture` more explicitly for this scenario.

### Task 3.3: Patch the skill(s)

- [ ] Same process as Task 1.3, now potentially touching `meshql-rs`'s `merkql-architecture` (if the cross-language composition guidance itself needs sharpening) in addition to `meshobj`'s `meshql-iron`/`meshql-patterns`. If `merkql-architecture` gets patched, remember to also re-copy the fixed version to `meshql`'s and `meshobj`'s installations (Task 0.1/0.2 made them verbatim copies — keep them in sync).

---

## Phase 4: Asset Management (`meshql`, Java + Rust merkql sidecar)

### Task 4.1: Dispatch the builder agent

- [ ] Dispatch a fresh Agent (general-purpose, zero prior context, `isolation: "worktree"`) with exactly this brief:

> Build an asset management app on meshql. Work in the `meshql` repo. Your app's main language is Java; merkql itself is Rust-only, so the event meshlette's connector and worker will need to be a small Rust piece — check `merkql-architecture` if you're unsure how that composes across languages. Build both backend and frontend, fully working end to end, with tests. Report what you built and why.

- [ ] Let it run to completion, same ground rules as Task 1.1.

### Task 4.2: Review the build

- [ ] Same checklist as Task 1.2, plus: Asset Management is mostly plain CRUD (assets, categories, locations) with an assignment/checkout event feeding an availability projection — check both the "don't force ceremony" concern (as in Task 2.2) and the cross-language sidecar concern (as in Task 3.2) together. Compare against Phase 3's findings — did this build need fewer corrections for the sidecar pattern specifically, confirming the Phase 3 patches generalized?

### Task 4.3: Patch the skill(s)

- [ ] Same process as Task 1.3.

---

## Phase 5: Final validation review

- [ ] Re-read all four builds' review notes (Tasks 1.2, 2.2, 3.2, 4.2) side by side. Per the spec's Validation section, check the trend: did each app surface fewer *new* categories of skill gap than the one before it, especially 1→2 (same language, should be nearly clean by app 2) and 3→4 (both cross-language, app 4 should reuse app 3's lessons)?
- [ ] If app 4 still surfaced first-time-seen gaps comparable in size to app 1's, note this explicitly and flag it to the user — that's a signal the skill content isn't generalizing and deserves a harder look before considering this project done. Do not silently call it done if the trend doesn't hold.
- [ ] Summarize the whole project's findings and the resulting skill diffs for the user.
