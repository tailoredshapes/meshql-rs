# meshql deployment manifest

A meshql deployment is described by a static JSON document conforming to
`manifest.schema.json`. The document is declared by the deployment author
(it can describe surfaces no single process knows about — MCP servers,
sidecars, search indexes) and served however you like: a `run_ext` static
route, nginx, S3, committed next to `config/`.

Clients (e.g. the meshql TS client) are constructed with a manifest URL.
The `kind` field of each surface is an open string; consumers use the kinds
they understand and ignore the rest. Absence of a `changes` surface (kind
`sse`) tells a client to degrade to refetch-on-dispatch.

A surface's `path` is authoritative when present; non-HTTP kinds (e.g.
`mcp`) omit it. There is no path-derivation convention — clients use
exactly what the manifest declares.

The `schema` field's type follows its surface's `kind` — `graphql` carries
schema text (string), `rest` carries a JSON Schema (object). The manifest
schema does not enforce this pairing; consumers should.

Versioning: documents declare `"meshql": 1`. Breaking schema changes ship
as `manifest-v2.schema.json` (new `$id`); this file always points at the
latest via its filename `manifest.schema.json`.

See `examples/egg-economy/config/manifest.json` for a complete example
(generated — see `gen_manifest` in that example) and
`docs/superpowers/specs/2026-07-07-meshql-changes-design.md` for the design.
