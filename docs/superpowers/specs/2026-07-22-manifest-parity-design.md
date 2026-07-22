# Manifest parity for meshql (Java) and meshobj (TypeScript)

**Date:** 2026-07-22
**Status:** Approved design, pre-implementation
**Depends on:** nothing (no changes to meshql-rs; this ports an existing, shipped feature)
**Unblocks:** the TypeScript client store (`docs/superpowers/specs/2026-07-07-meshql-changes-design.md`) — that project needs a manifest on all three backends, since it is designed to not know or care which mesh implementation it is talking to

## Motivation

`meshql-rs` already ships a deployment manifest: a static JSON document, conforming to `schemas/manifest.schema.json`, describing a deployment's entities and their GraphQL/REST surfaces (see the 2026-07-07 design doc and `examples/egg-economy/src/manifest.rs`). The two sibling implementations — `meshql` (Java, at `/tank/repos/tailoredshapes/meshql`) and `meshobj` (TypeScript, at `/tank/repos/tailoredshapes/meshobj`) — do not.

The planned FE client library is explicitly meant to work against any of the three backends without knowing which one it's talking to. For that to be true rather than aspirational, the manifest must be universal: all three backends need to serve a schema-conformant document. (The client's other input, the `/changes` SSE feed, is optional by design — the client degrades gracefully to refetch-on-dispatch without it — so it is explicitly out of scope for this work.)

This project ports the manifest — schema, generator, and conformance tests — to both other implementations, each wired into their respective `examples/farm/`.

## What "port" means here

Not a shared library or a new repo. Each of the three implementations is a separate, independently-versioned codebase; this project makes two of them independently capable of producing the same *document shape*, using each language's own idioms. `schemas/manifest.schema.json` is the only artifact that must be byte-for-byte compatible in meaning (not necessarily byte-for-byte identical text) across all three — it is vendored into each repo, with `meshql-rs` remaining the canonical source for future schema evolution (e.g. a `manifest-v2.schema.json`).

## Reference algorithm (from `meshql-rs/examples/egg-economy/src/manifest.rs`)

```
generate(config_dir):
  entities = {}
  for each *.graphql file in config_dir/graph/, sorted by filename:
    entity = filename without extension
    graphql_text = read file
    surfaces = { "graph": { kind: "graphql", path: "/{entity}/graph", schema: graphql_text } }
    if config_dir/json/{entity}.schema.json exists:
      surfaces["api"] = { kind: "rest", path: "/{entity}/api", schema: <parsed JSON schema> }
    entities[entity] = { surfaces }
  return { meshql: 1, entities, surfaces: {} }
```

Key properties carried over to both ports:
- **Reads the config directory directly** (`config/graph/*.graphql`, `config/json/*.schema.json`), not the runtime server-config object. This sidesteps a real asymmetry found during design: in Java, `GraphletteConfig.schema` is a file *path* at config-construction time (the SDL text isn't read until `Server.java` starts the graphlette), while in TypeScript the HOCON loader's `include file(...)` directive has already inlined both the GraphQL SDL text and the parsed JSON schema object into the `Config` structure by the time it exists. Reading the directory directly avoids depending on either behavior and keeps all three generators structurally identical.
- **Deterministic**: directory entries sorted before iteration (filesystem read order is not guaranteed to be stable, and object/map key order is otherwise insertion order in all three languages' JSON representations).
- **Path convention by construction, not configuration**: `/{entity}/graph` and `/{entity}/api`, matching how every `examples/farm` in all three repos already wires its graphlettes/restlettes.
- **`examples/farm` gets both surfaces for every entity, in practice.** Unlike `egg-economy`'s verb/noun (`ALL_VERBS`) split — which encodes that example's specific CQRS shape (only event-mesh "verbs" get a restlette) — `examples/farm` in all three repos is a plain CRUD example with no such split, and today every entity has a matching `.graphql` and `.schema.json` file. The generator still checks for the JSON schema file's existence rather than assuming it (same `if` in the pseudocode above) — don't hard-code "always emit `api`"; port the conditional check even though it's unconditionally true against `examples/farm`'s current contents.
- **No `changes` surface emitted.** Rust's egg-economy manifest advertises `{"surfaces": {"changes": {"kind": "sse", "path": "/changes"}}}` because that deployment has one. Neither Java's nor TS's `examples/farm` will have an SSE change feed (out of scope, see Motivation), so the top-level `surfaces` object in both ports is empty (`{}`), which is valid per the schema — absence of a `changes` surface is itself meaningful (it tells a client to degrade to refetch-on-dispatch).

## Java port (`meshql`)

- **Schema**: vendor `schemas/manifest.schema.json` at the repo root (new top-level `schemas/` directory), copied verbatim from `meshql-rs/schemas/manifest.schema.json`.
- **Generator**: new class `examples/farm/src/main/java/com/meshql/examples/farm/ManifestGenerator.java`, exposing a static method with the shape `JsonNode generate(Path configDir)`, implementing the reference algorithm above. Reads `.graphql` files as UTF-8 text; reads and parses `.schema.json` files with the Jackson `ObjectMapper` already used elsewhere in the repo (`com.fasterxml.jackson.databind.ObjectMapper`).
- **Regeneration CLI**: `examples/farm/src/main/java/com/meshql/examples/farm/GenManifest.java` (same package as `ManifestGenerator`), a small class with a `main` method that calls `ManifestGenerator.generate(...)` and writes the result to `examples/farm/config/manifest.json` (pretty-printed, stable key order), mirroring Rust's `gen_manifest` binary. Document the regeneration command (likely `mvn exec:java -pl examples/farm -Dexec.mainClass=...`, matching whatever invocation convention `examples/farm`'s `pom.xml` already supports — check for an existing `exec-maven-plugin` binding before adding one).
- **Committed artifact**: `examples/farm/config/manifest.json`.
- **Serving `GET /manifest` over HTTP** (required — a manifest that only exists as a repo file is useless to a client, which fetches it from a running deployment): `Server.init(Config)` in `server/src/main/java/com/meshql/server/Server.java` is `void` and its `ServletContextHandler` is a local variable, never exposed after construction — there is currently no way for a caller to mount an extra route. Add a small overload:
  ```java
  public void init(Config config) throws Exception { init(config, Map.of()); }
  public void init(Config config, Map<String, HttpServlet> extraServlets) throws Exception {
      // ...unchanged setup...
      for (var e : extraServlets.entrySet()) {
          context.addServlet(new ServletHolder(e.getValue()), e.getKey());
      }
      jettyServer.start();
      // ...
  }
  ```
  registering the extra servlets before `jettyServer.start()`. This is additive (existing `init(Config)` callers are unaffected) and mirrors the existing `HealthCheckServlet`/`ReadinessServlet` pattern at the bottom of `Server.java` for the handler shape (`doGet` sets `Content-Type: application/json`, writes the manifest string). `examples/farm/Main.java` then passes `Map.of("/manifest", <that servlet>)` when calling `init`. Both this servlet and the TS route below serve the **committed** `config/manifest.json` file (read at startup), not a live call to the generator — matching Rust's `include_str!` approach, and keeping with "the contract is the document" (2026-07-07 design doc): the generator is a dev-time authoring tool, not part of the request path.
- **Conformance tests**: new `examples/farm/src/test/java/com/meshql/examples/farm/ManifestConformanceTest.java` (JUnit 5, matching repo convention), three tests mirroring `manifest_conformance.rs`:
  1. `manifestValidatesAgainstPublishedSchema` — load the vendored schema, validate the committed manifest against it using `com.networknt.schema.JsonSchemaFactory` (already a dependency, used today for REST payload validation in `RestletteConfig`/`JSONSchemaValidator`). No new dependency.
  2. `manifestMatchesRegeneration` — assert the committed `manifest.json` deep-equals a fresh call to `ManifestGenerator.generate(...)`. Failure message should name the regeneration command.
  3. `everyGraphEntityAppearsInManifest` — walk `config/graph/*.graphql`, assert each entity is present in the manifest with a `graph` surface of kind `graphql`, and (since `examples/farm` has no verb/noun split) assert an `api` surface is present iff a matching `config/json/<entity>.schema.json` file exists. Also assert entity counts match (guards against a vacuous pass).
  4. An integration test (or extending an existing farm BDD/integration test) that starts the server and asserts `GET /manifest` returns 200 with the same content as the committed file — proves the servlet is actually wired, not just that the file exists.

## TypeScript port (`meshobj`)

- **Schema**: vendor `schemas/manifest.schema.json` at the repo root, copied verbatim.
- **Generator**: new module `examples/farm/src/manifest.ts`, exporting `generate(configDir: string): Record<string, any>`, implementing the reference algorithm using Node's `fs`/`path`. Deliberately reads the directory rather than consuming the already-parsed HOCON `Config` object (see "Reference algorithm" above) — this keeps the generator runnable standalone (no HOCON parsing, no server bootstrap) and structurally identical to the other two ports.
- **Regeneration CLI**: `examples/farm/scripts/gen-manifest.ts`, a small script (run via `ts-node` or the repo's existing script-running convention — check `examples/farm/package.json` for how other one-off scripts in the repo are invoked) that calls `generate(...)` and writes `examples/farm/config/manifest.json`.
- **Committed artifact**: `examples/farm/config/manifest.json`.
- **Serving `GET /manifest` over HTTP** (required — see the Java section for why): no core-package change needed here. `init(config, plugins): Promise<Application>` in `core/server/src/server.ts` already returns the live Express `Application`, and `init()` never calls `.listen()` itself — that's left to the caller. So `app.get('/manifest', (req, res) => res.json(manifest))` works today, called between `init()` returning and `app.listen()`. The only wrinkle: `examples/farm` currently has no entrypoint of its own — it runs the generic `@tailoredshapes/meshql-cli` binary (`node dist/index.js`) built from `core/cli`. Add a small custom entrypoint for this example (e.g. `examples/farm/src/index.ts`) that calls `init()` + mounts `/manifest` + calls `app.listen()`, replacing the generic CLI for this example specifically; update `examples/farm/package.json`'s `start` script to point at it.
- **Conformance tests**: new `examples/farm/test/manifest.spec.ts` (Vitest, matching repo convention), three tests mirroring the Rust/Java suites:
  1. Validates the committed manifest against the vendored schema using `ajv`/`ajv-formats` (already a dependency in the workspace). No new dependency.
  2. Committed manifest deep-equals fresh `generate(...)` output.
  3. Every `.graphql` file in `config/graph/` has a corresponding manifest entity with a `graph` surface, and an `api` surface iff a matching `.schema.json` file exists in `config/json/`; entity counts match.
  4. An integration test starting the custom entrypoint's app (via `supertest` or an equivalent already used elsewhere in the repo's test suites) and asserting `GET /manifest` returns 200 with the committed content — proves the route is actually wired, not just that the file exists.

## Testing strategy (both ports)

Same three-test shape as `meshql-rs/examples/egg-economy/tests/manifest_conformance.rs`, plus one HTTP-serving integration test per port (see above) — four tests total per language. No new CI wiring: Java's `.github/workflows/ci.yml` already runs `mvn test -B -T 1C` at the repo root, which includes the `examples/farm` Maven module (no `skipTests`/surefire exclusion configured for it); TS's `.github/workflows/ci.yml` runs `yarn test` (`lerna run test --concurrency 1`), and `examples/farm/package.json` already has a `test` script (`vitest --passWithNoTests --run`) that lerna picks up automatically. No network calls in tests; schema validation is always against the vendored local copy.

## Out of scope

- The `/changes` SSE change feed for Java or TS — optional per the client's design, tracked as separate future work if ever pursued.
- Wiring the manifest into any Java/TS example other than `examples/farm` (e.g. Java's `examples/egg-economy` or TS's `examples/events`) — `examples/farm` is sufficient to prove the port and give the future client something to test against in all three languages. Extending to other examples is a trivial follow-up, not required here.
- The TS client library itself — separate project, separate spec, blocked on this one completing.
- A shared/published schema package or schema registry — the vendor-a-copy approach (meshql-rs canonical, others vendored) was chosen explicitly over this; see "What 'port' means here."
- Schema v2 or any change to `manifest.schema.json`'s content — this project ports the existing v1 schema unchanged.
