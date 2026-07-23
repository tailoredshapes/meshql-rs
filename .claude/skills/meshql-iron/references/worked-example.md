# Worked example: a minimal farm frontend

A single, concrete walkthrough grounded in `examples/farm` — the deployment this project built out with a full event/domain split (`lay_report` event feeding a `hen_productivity` projection) across all three meshql backends. Real vanilla JS/HTML, no framework, no build step, per this project's standing frontend conventions.

**Note on which branch has this shape:** as of this writing, `lay_report`/`hen_productivity` exist on `merkql-worker-pipeline` (this repo), `farm-retrofit-java`, and `farm-retrofit-ts` — not yet on any repo's `main`. Check whether that's since changed; if the entities aren't in `main`'s `/manifest`, run against the feature branch instead.

## Step 1: fetch the manifest, identify the split

```bash
curl -s http://localhost:3033/manifest | jq '.entities | keys'
# ["coop", "farm", "hen", "hen_productivity", "lay_report"]
```

Per `event-vs-domain-mesh.md`'s detection process: `examples/farm`'s own docs describe `lay_report` as create-only (event-mesh) and `hen_productivity` as worker-maintained (domain-mesh). This deployment's manifest (entity-named dialect) exposes:

- `lay_report`: `getLayReport(id, at)`, `getLayReports(at)`, `getLayReportsByHen(id, at)` — reads, plus `POST /lay_report/api` to write.
- `hen_productivity`: `getHenProductivity(id, at)`, `getHenProductivities(at)`, `getHenProductivityByHen(id, at)` — reads only, from a frontend's perspective; writes come from the worker, not from us.

(A different deployment might expose these under the generic dialect — `getById`/`getByHen` — instead. Always confirm against the actual manifest response; see `manifest-discovery.md`.)

## Step 2: the page

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Farm — Lay Reports</title>
</head>
<body>
  <main>
    <h1>Record a lay report</h1>

    <section aria-labelledby="hens-heading">
      <h2 id="hens-heading">Hens</h2>
      <ul id="hen-list"></ul>
    </section>

    <form id="lay-report-form">
      <label for="hen-select">Hen</label>
      <select id="hen-select" name="henId" required></select>

      <label for="eggs">Eggs</label>
      <input id="eggs" name="eggs" type="number" min="0" required>

      <label for="time-of-day">Time of day</label>
      <input id="time-of-day" name="timeOfDay" type="datetime-local" required>

      <button type="submit">Record</button>
    </form>

    <p id="status" role="status" aria-live="polite"></p>
  </main>

  <script type="module" src="./app.js"></script>
</body>
</html>
```

## Step 3: the logic

```js
// app.js — vanilla ES module, no build step, no framework.
const BASE = 'http://localhost:3033';

const henSelect = document.getElementById('hen-select');
const henList = document.getElementById('hen-list');
const form = document.getElementById('lay-report-form');
const status = document.getElementById('status');

async function loadHens() {
  const res = await fetch(`${BASE}/hen/graph`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ query: '{ getHens { id name } }' }),
  });
  const { data } = await res.json();

  henList.innerHTML = '';
  henSelect.innerHTML = '';
  for (const hen of data.getHens) {
    const li = document.createElement('li');
    li.textContent = hen.name;
    henList.appendChild(li);

    const option = document.createElement('option');
    option.value = hen.id;
    option.textContent = hen.name;
    henSelect.appendChild(option);
  }
}

async function getHenProductivity(henId) {
  const res = await fetch(`${BASE}/hen_productivity/graph`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      query: 'query($id: ID!) { getHenProductivityByHen(id: $id) { totalEggs lastLaidAt } }',
      variables: { id: henId },
    }),
  });
  const { data } = await res.json();
  return data.getHenProductivityByHen[0] ?? null;
}

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  const henId = henSelect.value;
  const eggs = Number(document.getElementById('eggs').value);
  const timeOfDay = new Date(document.getElementById('time-of-day').value).toISOString();

  // lay_report is event-mesh: create-only, write via its restlette. Never PUT/DELETE it,
  // and never write hen_productivity directly — a worker derives it from this event.
  const writeRes = await fetch(`${BASE}/lay_report/api`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ henId, eggs, timeOfDay }),
  });
  const writtenAt = writeRes.headers.get('X-Meshql-Created-At');

  status.textContent = 'Recording…';

  // hen_productivity is domain-mesh: read it via its own graphlette, not lay_report's.
  // The manifest has no structural link between the two entities — that relationship
  // lives in the worker, so freshness here is "refetch and compare," not a lookup
  // (see honesty.md, Case 2).
  const deadline = Date.now() + 5000;
  let productivity = null;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    productivity = await getHenProductivity(henId);
    if (productivity?.lastLaidAt && new Date(productivity.lastLaidAt) >= new Date(writtenAt)) break;
  }

  status.textContent = (productivity?.lastLaidAt && new Date(productivity.lastLaidAt) >= new Date(writtenAt))
    ? `Recorded. ${productivity.totalEggs} eggs so far.`
    : 'Recorded — still catching up, refresh in a moment.';
});

loadHens();
```

## What this demonstrates

- **Manifest-driven discovery**: query names (`getHens`, `getHenProductivityByHen`) came from reading the manifest, not from memory of a similar deployment.
- **Event vs. domain routing**: the form writes to `/lay_report/api` (the event restlette) and never touches `/hen_productivity/api` — that entity is worker-only, per `event-vs-domain-mesh.md`.
- **Honesty in practice**: `X-Meshql-Created-At` from the write is compared against `hen_productivity.lastLaidAt` (a domain field the worker sets) to decide when to stop polling — the cross-entity heuristic from `honesty.md`, not a generic mechanism.
- **Standing frontend conventions**: semantic HTML (`<main>`, `<form>`, labeled inputs), an `aria-live="polite"` status region instead of silent DOM mutation, no framework, no build step.
