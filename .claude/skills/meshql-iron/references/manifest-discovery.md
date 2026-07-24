# Discovering a meshql deployment via `/manifest`

Every meshql deployment self-describes at `GET /manifest`. Fetch it before writing any request against a guessed URL or payload shape — it's the source of truth for a running deployment, not documentation that can drift from it.

## Shape

```json
{
  "meshql": 1,
  "entities": {
    "<entity>": {
      "surfaces": {
        "graph": { "kind": "graphql", "path": "/<entity>/graph", "schema": "<SDL text>" },
        "api":   { "kind": "rest",    "path": "/<entity>/api",   "schema": { "...JSON Schema..." } }
      }
    }
  }
}
```

Real example, from `examples/farm`'s `coop` entity (trimmed):

```json
"coop": {
  "surfaces": {
    "graph": {
      "kind": "graphql",
      "path": "/coop/graph",
      "schema": "type Coop {\n    id: ID\n    farmId: String\n    name: String\n    capacity: Int\n    farm: Farm\n    hens: [Hen]\n}\n\ntype Query {\n    getCoop(id: ID, at: Float): Coop\n    getCoops(name: String, at: Float): [Coop]\n    getCoopsByFarm(id: ID, at: Float): [Coop]\n}\n"
    },
    "api": {
      "kind": "rest",
      "path": "/coop/api",
      "schema": {
        "type": "object",
        "properties": { "farmId": {"type": "string"}, "name": {"type": "string"}, "capacity": {"type": "integer"} },
        "required": ["farmId", "name"]
      }
    }
  }
}
```

## How to use each surface

- **`kind: "graphql"`** — `schema` is raw SDL text. Read it for the available `Query` fields, their arguments, and return types. Every query takes `at: Float` (epoch millis) for point-in-time reads — omit it (or pass the current time) for "now." `POST <path>` with a standard GraphQL `{ query, variables }` body.
- **`kind: "rest"`** — `schema` is the JSON Schema the write payload must satisfy. `POST <path>` to create, `PUT <path>/<id>` to update, `DELETE <path>/<id>` to remove. `required` is the minimum payload; validate a form against it the same way you'd validate against any JSON Schema.

## Query names are not a fixed vocabulary — read them

Two meshql deployments backing the identical entity shape can name their GraphQL queries differently. One service might expose `getLayReport(id)` / `getLayReportsByHen(id)` (entity-named); another might expose `getById(id)` / `getByHen(id)` (generic — used when a deployment wants to be auto-derivable by `meshql-mcp`). **Never hardcode a query name from a different deployment or from memory** — the manifest's SDL for *this* deployment is the only place that's guaranteed accurate. If a query you expect isn't in the schema, it doesn't exist here; look for the deployment's actual name for it instead of assuming it was omitted by mistake.

The same caution applies to payload field casing — most examples in this ecosystem use camelCase (`henId`, `farmId`), but don't assume it; read it from the JSON Schema's `properties`.

## URL construction

Paths in the manifest are already absolute (`/coop/graph`, `/coop/api`) — use them directly. If you ever need to construct one by convention instead of from a cached manifest, the fixed rule every implementation follows is `/{entity}/graph` and `/{entity}/api`, snake_case entity name.

## Stability

This doc is stable because the manifest schema itself (`schemas/manifest.schema.json`, `"meshql": 1` in every document) is stable and versioned. A deployment that changes its entity shapes changes what the manifest reports — it doesn't require a different fetching strategy on your end.

## When you're building both halves yourself

Everything above assumes you're writing a frontend against a deployment you didn't design — that's when hardcoding a query name or field is dangerous, because you're guessing at someone else's contract. If you're authoring the backend and frontend in the same task, you already know the schema you just wrote; hardcoding the query names and payload fields you yourself defined is fine and doesn't need a runtime `/manifest` fetch to justify it. Still generate and commit the manifest (other consumers, and your own future self, will need it), and don't let the two drift — but the frontend code itself doesn't have to rediscover what its own author already knows.
