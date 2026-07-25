---
name: meshql-iron
description: Build a frontend or API client against a meshql deployment. Use when writing UI code, a client library, or any consumer that reads from, writes to, or subscribes to a meshql-backed service (REST restlettes, GraphQL graphlettes, SSE streamlettes) — including real-time or live-updating UIs, and anything that would otherwise poll. Covers manifest discovery, the event-mesh/domain-mesh convention, "honesty" freshness timestamps, and SSE streaming with reconnect and resume.
---

# meshql-iron: building a frontend against meshql

`meshql-iron` is the frontend-consumption counterpart to `meshql-patterns` (which teaches how to *build* a meshql deployment). Use this skill whenever the task is building a UI, a client library, or any other consumer of an already-running meshql deployment — not when you're adding entities or endpoints to the deployment itself.

## The core principle, in one paragraph

Every meshql entity is a **meshlette**: a Restlette (`POST`/`PUT`/`DELETE /<entity>/api`, JSON Schema-validated writes) paired with a Graphlette (`/<entity>/graph`, GraphQL reads) over the same store. **Write via the restlette. Read via the graphlette.** Some deployments additionally split entities into an **event mesh** (create-only, projected by a worker into a separate entity) and a **domain mesh** (directly queryable) — when that's true, write to the event restlette and read from the domain graphlette, never the other way around. See `references/event-vs-domain-mesh.md` for how to recognize this and why it matters. Every response also carries "honesty" freshness metadata — an `X-Meshql-Created-At` REST header and an opt-in `createdAt` GraphQL field — so you can tell whether a read reflects a write you just made, instead of silently rendering stale data. See `references/honesty.md`.

## Getting started: discover the deployment

Don't hand-write requests against guessed shapes. Fetch `GET /manifest` first — every meshql deployment self-describes there: every entity, its GraphQL SDL, and its JSON Schema for writes. Read it the way you'd read any API document, and derive the correct calls from what it actually says, not from what a similar-looking deployment did last time. See `references/manifest-discovery.md`.

## Decision guide

- **Discovering what a deployment exposes** (entities, queries, write schemas) → read `references/manifest-discovery.md`
- **Deciding whether to write to entity X directly, or to some other event entity that feeds it** → read `references/event-vs-domain-mesh.md`
- **Showing a "pending"/"saved" state, or deciding whether a read reflects a write you just made** → read `references/honesty.md`
- **Keeping a long-lived view current without polling** (a streamlette's SSE surface at `/{entity}/stream`, its `ready`/`change`/`lagged` frames, reconnects and resume) → read `references/streaming.md`
- **Seeing it all put together** → read `references/worked-example.md`, a real vanilla-JS walkthrough against `examples/farm`

## Non-goals — don't reach for these

- Don't consume the deployment-level `/changes` feed from a UI. Streaming itself is in scope — a per-entity **streamlette** (`/{entity}/stream`) is the surface a frontend subscribes to, and `references/streaming.md` covers it. `/changes` is a different, deployment-wide pump with its own contract; reach for the entity's own stream instead.
- No generated or compiled client package. Read the manifest and write plain `fetch` calls — there's no codegen step and nothing to `npm install`.
- No reactive store. Subscribing is fine — a streamlette is a real subscription, and `references/streaming.md` shows how to consume one — but the response to a notification is a `fetch`, not a framework. Don't build an observable/derived-state layer that reconstructs entity state from change frames; a cache here is still just a `fetch` you haven't repeated yet.
- Don't force an event/domain split onto an entity that's plain CRUD. Only apply `event-vs-domain-mesh.md` when the deployment actually uses that pattern — see its detection heuristic before assuming it does.
