# Streaming: consuming a streamlette over SSE

A **streamlette** is a third meshlette surface, alongside the restlette and the graphlette: one entity's change notifications, served as Server-Sent Events at `/{entity}/stream`. It exists so a client can stop polling — not so it can stop fetching. Everything a stream tells you is a *nudge to read*; the read still goes through the graphlette, so CQRS, temporal, and authorization invariants are untouched.

This is `honesty.md` extended to a live connection. There, the rule was "never silently render stale data" — compare timestamps, show pending state, refetch. Here it's the same rule under a longer-lived assumption: a socket that has been open for an hour is a claim about freshness, and the claim is only true if you fetched when it opened and refetch whenever it reopens. **Fetch on connect, and on every reconnect.** A stream is an optimisation over polling, never a substitute for reading.

## Step 1: discover the surface — don't assume it exists

Streamlettes are per-entity and opt-in. An entity has one only if the manifest says so (`manifest-discovery.md` covers the document as a whole):

```json
"lay_report": {
  "surfaces": {
    "graph":  { "kind": "graphql", "path": "/lay_report/graph",  "schema": "..." },
    "api":    { "kind": "rest",    "path": "/lay_report/api",    "schema": { } },
    "stream": { "kind": "sse",     "path": "/lay_report/stream", "resume": true }
  }
}
```

```js
const manifest = await fetch(`${BASE}/manifest`).then((r) => r.json());
const surface = manifest.entities.lay_report?.surfaces?.stream ?? null;
// null → this entity has no stream. Fetch on an interval, or on user action.
```

**`resume` is derived from the server's own source type and conformance-tested, so read it rather than assuming.** It answers one design-time question: can a dropped connection come back without a gap? `true` (a log-backed source) means reconnects are cheap and history is replayable. `false` (a poll-diff tail source) means every reconnect is a hole, and the UI must be built to refetch through it. Either way the per-connection truth arrives in the `ready` frame — the manifest flag tells you what to *design* for, `ready` tells you what actually happened.

## Step 2: the frames

Four things arrive on the wire.

**`event: ready`** — always first, on every connection:

```
event: ready
data: {"resume":true,"cursor":"0:1841"}
```

`resume` is whether your `Last-Event-ID` was honoured. `cursor` is **the position resume started from** — never the log tail — so comparing it against the id you sent tells you your cursor was rejected rather than leaving you to guess. It is `null` whenever `resume` is `false`. The frame deliberately carries no SSE `id:`, so it can't clobber the browser's `Last-Event-ID` tracking.

**`event: change`** — a thin fact about one record:

```
id: 0:1842
event: change
data: {"entity":"lay_report","id":"a1b2…","created_at":1751892345123,"deleted":false,"cursor":"0:1842"}
```

Note the wire naming: `created_at`, snake_case epoch millis — not GraphQL's RFC3339 `createdAt` from `honesty.md`. **The SSE `id:` IS the resume cursor**, and it is *absent* on sources that can't seek, so a browser never echoes back a `Last-Event-ID` the server couldn't honour. `cursor` and `payload` are omitted from `data` entirely when absent, not sent as `null`.

**`event: lagged`** — terminal:

```
event: lagged
data: {"skipped":8}
```

You overran the server's broadcast buffer; those 8 events are unrecoverable and the stream then closes. This is not an error to retry past — the only correct response is a full refetch and a fresh connection.

**`:heartbeat`** — an SSE comment every 15s. Ignore it; it exists to keep proxies from reaping an idle connection.

An unusable `Last-Event-ID` is never a `4xx` — an error response would put `EventSource` into a reconnect loop. It silently degrades to live-only and tells you so via `ready`.

## Step 3: subscribe

Vanilla `EventSource`, no framework, no build step.

> **Read "Three things the first browser consumer hit" below before you write this.** If your deployment resolves caller identity from a request header — the meshql trusted-edge model this skill assumes everywhere else — then **`EventSource` cannot authenticate at all**, because the API has no way to send one. You get a connection that looks perfectly healthy and delivers nothing, forever. The sample below is correct about the protocol and wrong about the platform; the fix (a `fetch` with a streamed `ReadableStream` body) is about twenty extra lines and is what a real browser client ended up shipping.

```js
// stream.js — one entity's live view, kept honest.
export function subscribe(base, surface, { refetch, onChange }) {
  let source = null;
  let resyncing = false;
  let buffered = [];

  // Fetch-on-connect. Subscribing happens BEFORE the fetch, so anything
  // committed mid-fetch is buffered rather than lost down the gap; the
  // buffer is applied after the fresh read lands.
  async function resync() {
    resyncing = true;
    buffered = [];
    try {
      await refetch();
    } finally {
      resyncing = false;
      const queued = buffered;
      buffered = [];
      for (const change of queued) deliver(change);
    }
  }

  function deliver(change) {
    if (resyncing) { buffered.push(change); return; }
    onChange(change);
  }

  function connect() {
    source = new EventSource(base + surface.path);

    source.addEventListener('ready', (e) => {
      const { resume } = JSON.parse(e.data);
      // resume === true: the server replayed everything after our cursor,
      // so there is no gap to close. Otherwise we are missing history.
      if (!resume) resync();
    });

    source.addEventListener('change', (e) => deliver(JSON.parse(e.data)));

    source.addEventListener('lagged', () => {
      // Terminal — the server has closed this stream. Reconnect from
      // scratch and refetch; do not try to resume past the gap.
      restart();
    });

    // Transport-level drops are NOT handled here on purpose: EventSource
    // reconnects on its own and resends Last-Event-ID, and the `ready`
    // frame on the new connection tells us whether that worked.
  }

  function restart() {
    source?.close();
    connect(); // a NEW EventSource — see "abandon the cursor" below
  }

  connect();
  return { close: () => source?.close(), restart };
}
```

A brand-new `EventSource` sends no `Last-Event-ID`, so its `ready` frame reports `resume: false`, which triggers `resync()`. Fetch-on-connect and resync-after-lag are therefore the same code path, driven entirely by the `ready` frame — you never have to track "is this my first connection" yourself.

Per the standing frontend conventions, announce a reconnect in a `role="status"` region rather than mutating the DOM silently; a user watching a list should be told it was reloaded.

## Two rules that are easy to get wrong

Both produce the same symptom — duplicated items — and both bite hardest on payload-carrying streams, where the client appends instead of refetching.

**1. After a full refetch, abandon the cursor.** Browser `EventSource` auto-reconnects *and* auto-resends `Last-Event-ID`, and there is **no API to clear it**. Letting it auto-reconnect after you have already refetched replays events the refetch just returned. The only way out is to `close()` the `EventSource` and construct a **new** one — which is exactly what `restart()` above does, and why it exists as a separate function rather than a call to `connect()` on the same object.

**2. Payload-consuming clients dedupe locally by `cursor`.** Delivery is at-least-once. For a notification-only client this is free — the response is an idempotent refetch. For a client that renders what arrives, it is not:

```js
const seen = new Set();
function onChange(change) {
  if (change.cursor) {
    if (seen.has(change.cursor)) return;
    seen.add(change.cursor);
  }
  render(change);
}
```

## Three things the first browser consumer hit that the above does not cover

Written after converting teamchat's frontend, which was this guidance's first
real use. Step 3's `EventSource` sample is correct about the *protocol* and
wrong about the *platform* in two ways that only appear in a browser.

**1. `EventSource` cannot send a request header, so it cannot authenticate
against a trusted edge.** The API has no header parameter; `withCredentials`
sends cookies and nothing else. If your deployment resolves identity from a
header — which is the meshql trusted-edge model this skill assumes everywhere
else — then `new EventSource('/thing/stream')` arrives *anonymous*. It resolves
no tokens, every envelope carrying at least one is filtered out, and you get a
200, a well-formed `ready` frame, heartbeats forever, and zero events.

That is **the same silent healthy connection** described below, reached from
the client instead of the router, and it is exactly as invisible. Check it in
the same breath: if a stream delivers nothing, confirm both that the edge runs
on `/stream` *and* that the client can actually get its identity onto the
request.

The fix is a `fetch` with a streamed `ReadableStream` body, which carries
headers. Still vanilla, still no build step. It costs the two things
`EventSource` does for free — reconnect with backoff, and `Last-Event-ID`
resend — and both are perhaps twenty lines. It also **dissolves rule 1**: the
cursor becomes an ordinary variable, so "abandon the cursor after a refetch" is
`cursor = null` rather than a fight with an API that has no way to clear it.
The rule does not change; obeying it stops being awkward.

If you need `EventSource` specifically, the edge needs a second identity source
that survives a header-less request — a cookie, or a short-lived token in the
query string. That is a server change and no amount of client code substitutes
for it.

**2. Streamlettes are per-entity, and a browser has about six connections.**
`fetch` and `EventSource` share one per-origin pool that every current browser
caps at six over HTTP/1.1, and browsers speak HTTP/2 only over TLS. A service
with nine streamed entities therefore cannot have a client that watches all
nine: the streams take the whole pool, extras queue forever, and — the part
that actually breaks the app — **the graphlette reads the notifications exist
to trigger are starved**. Notifications arrive with no connection left to act
on them.

So a browser client needs a *connection budget*, spent on the entities the
current view actually shows and reconciled on navigation. This is cheap when
the streams are notification-only, because the response to any of them is the
same idempotent refetch — a dropped stream costs staleness until something else
nudges, not a permanently wrong view. Plan for it at design time: "which meshes
get a stream" is a server question with a different answer from "which streams
can one page hold".

**3. Coalesce. A burst of ten notifications must be one read.** The sample above
buffers changes *during* a refetch, which is right, but says nothing about
changes arriving *between* refetches — and for a notification-only client,
`onChange` and `refetch` are the same action. Ten messages in a busy channel
then means ten full reads. Debounce the nudge, join an in-flight read rather
than starting a second, and re-run once if a notification landed while a read
was in flight (otherwise the write that arrived mid-read stays invisible until
something unrelated happens).

## Payloads, and why only some streams have them

A `change` frame may carry a `payload` — the record's contents, saving the follow-up read. **Only log-backed (seekable) sources may do this; tail sources never do.**

The reason is worth understanding rather than treating as a quirk. A poll-diff tail source detects change by comparing payload hashes, so it cannot see a *token-only* ACL change: it retains the record's old `authorized_tokens` until the next payload edit or delete. Filtering a notification through a stale token set leaks only the fact that *something* changed. Filtering a **payload** through it leaks the record itself, to someone who should no longer see it. The server's type system makes the unsafe pairing unrepresentable, so you will never receive an unsound payload — but knowing this explains why the same client code sees payloads on one entity and bare notifications on another, and why the answer is "refetch through the graphlette," not "ask for payloads to be turned on."

## Two caveats to document in your own product

- **Subscriber tokens are captured once, at connect.** A long-lived stream will not observe a mid-connection privilege change: someone removed from a channel keeps receiving its notifications until they reconnect. If your product needs hard revocation, it must force a disconnect — the stream will not do it for you.
- **Tail sources carry stale tokens** (above). On a notification-only stream the exposure is limited to "a record you can no longer see changed," which is far less dangerous than a payload leak, but it is not nothing.

## The silent healthy connection — check this first when a stream delivers nothing

If your deployment computes caller tokens at a trusted edge (middleware that resolves an identity header into credentials), **make sure that middleware runs on `/{entity}/stream` too, not only on `/api` and `/graph`.**

Miss it and the failure is invisible in every direction a developer normally looks. The stream gets an empty stash, so it resolves *no* tokens; every envelope carrying at least one token is therefore filtered out. The result is HTTP 200, a well-formed `ready` frame, heartbeats arriving on schedule forever, and **zero events** — no error, no warning, nothing in a log. It is indistinguishable from a quiet channel, and it will survive any amount of staring at the client.

This is not hypothetical: it is the first thing the first real consumer of streamlettes hit, and it cost a debugging cycle before anyone suspected routing. If a stream connects cleanly and delivers nothing, check the edge's path matching before you check anything else.

The tell: the same caller can read the entity over its graphlette but receives nothing over its stream. That asymmetry means identity is reaching one surface and not the other.

## When not to stream

A page that reads once does not need a subscription. Streaming earns its keep when a view is long-lived and other people write to it — a chat channel, a live dashboard, a queue someone else is working. It earns nothing on a form, a detail page, a report, or anything the user reloads by navigating.

The failure mode to avoid is treating a streamlette as *the* way to read. It isn't; the graphlette is. If you find yourself reconstructing state purely from `change` frames, you have rebuilt the reactive store this skill's non-goals warn against, and you have done it on an at-least-once feed with a terminal lag frame. Fetch-on-connect is always correct. Add the stream when polling is the thing you're trying to delete.
