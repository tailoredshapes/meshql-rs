# Honesty: telling whether a read reflects your write

meshql never widens the JSON body to expose its internal Envelope (`id`, `createdAt`, `deleted`, `authorizedTokens` stay internal by design), but it does leak exactly two freshness signals — deliberately — so a frontend can tell how fresh a payload is instead of silently rendering stale data.

## The two mechanisms

- **REST**: `create`, `read`, and `update` responses carry an `X-Meshql-Created-At` header (RFC3339 timestamp) and an `X-Meshql-Deleted` header (`true`/`false`). Automatic, no configuration. Not present on `list` (one header can't represent many records) or `delete` (there's no Envelope left to read a timestamp from — deletion is a tombstone write, and the delete endpoint returns only a boolean). Implemented in `meshql-restlette/src/routes.rs`.
- **GraphQL**: every Searcher merges `createdAt` (RFC3339) into the returned record next to `id`. A type opts in just by declaring `createdAt: String` in its schema — no resolver code needed, and a type that doesn't declare it simply never exposes the field. `deleted` is never exposed in GraphQL; search results already exclude deleted/superseded versions, so there's nothing to leak.

## Case 1: same-entity read-your-writes

You wrote a `coop` and want to know whether a subsequent read reflects that write. This is a clean, mechanical comparison — *if the type opts into `createdAt`*:

```js
const writeRes = await fetch(`${BASE}/coop/api`, { method: 'POST', /* ... */ });
const writtenAt = writeRes.headers.get('X-Meshql-Created-At');

const readRes = await fetch(`${BASE}/coop/graph`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ query: `{ getCoop(id: "${id}") { id createdAt } }` }),
});
const { data } = await readRes.json();

const isFresh = data.getCoop && new Date(data.getCoop.createdAt) >= new Date(writtenAt);
```

If `isFresh` is false, the read predates your write — show a pending state and refetch rather than rendering what you got.

**Before relying on this, confirm the type actually declares `createdAt`** — it's opt-in per type, and none of the shipped `examples/farm` GraphQL schemas currently do (check the manifest's SDL for the type, per `manifest-discovery.md`, rather than assuming). If it isn't declared, you only have the REST write-side timestamp — enough to know *when* you wrote, not enough to mechanically confirm a specific read reflects it.

## Case 2: cross-entity / projection freshness

You wrote a `lay_report` (an event) and care about `hen_productivity` (a projection derived from it by a worker — see `event-vs-domain-mesh.md`). **No direct timestamp comparison is possible here** — the write and the projection are different records entirely, and the manifest has no way to encode that one feeds the other; that relationship lives inside the worker's code, not in anything published.

The guidance here is a pragmatic heuristic, not a mechanism:

1. After the write, show a pending/"recording…" affordance — don't render the old projection value as if it were current.
2. Refetch the projection (`hen_productivity`, in this example) — either once after a short delay, or a few times with a short interval.
3. Expect it to resolve within a read or two. The common case is a worker that processes an event faster than a page refresh takes to happen, not a system that needs sophisticated retry/backoff logic.
4. If the projection has its own domain timestamp field (e.g. `hen_productivity.lastLaidAt`, a payload field the worker sets — distinct from the honesty `createdAt` mechanism above), you can compare *that* against your write's `X-Meshql-Created-At` for a real, if domain-specific, freshness signal. This isn't the honesty mechanism reused generically — it's this particular projection happening to carry a timestamp that means something similar. Don't assume every projection has one.

This is the same pattern as this codebase's global frontend conventions: pending state, let the user refresh, never silently render stale data. Nothing here is meshql-specific beyond *what* to refetch and *why* a generic timestamp comparison doesn't apply.
