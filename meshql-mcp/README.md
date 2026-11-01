# meshql-mcp

The standard MeshQL tools surface uses MCP **2025-06-18 over stdio**. This is
not an HTTP MCP transport or a claim to implement every MCP capability.

Start the ordinary REST/GraphQL deployment, then point the stdio command at it:

```sh
meshql-mcp --config config.conf --backend http://127.0.0.1:3000 --identity alice
```

The configuration supplies the real GraphQL SDL and REST document schemas.
The command does not open databases. Each operation uses normal HTTP to the
running deployment, where its configured authorization plugin and write mode
remain authoritative. Stdout contains protocol frames only.

`--identity` is trusted launcher configuration, not an MCP argument. The legacy
client identity headers remain `X-Manifold-User-Id` and
`X-Manifold-User-Groups`; the deployment's trusted edge must establish its auth
context from the chosen headers. Anonymous writes are allowed when `NoAuth`
allows them. There is no extra nonempty-environment-user gate.

## Library construction

```rust,ignore
let capabilities = CapabilitiesBuilder::new()
    .auto_from_schemas(&[("widget", "/widget/graph", WIDGET_SDL)])
    .with_rest_schema("widget", "/widget/api", actual_rest_schema)
    .build();
```

`auto_from_schemas` derives reads through the complete GraphQL SDL parser used
by graphlettes. Fields are selected from each operation's actual return type;
federated objects have one level of scalar selections. Scalar and list query
arguments are typed. Unsupported input types fail catalogue construction rather
than silently disappearing. GraphQL values are sent as variables, including
optional numeric `at` when declared. Tool names retain these conventions:

| Operation | Tool |
|---|---|
| `getAll` | `list_widgets` |
| `getById` | `get_widget_by_id` |
| `getByName` | `find_widgets_by_name` |
| `getByGroupId(group_id)` | `widgets_for_group` |

Other query names use `<entity>_<operation>`. The server sorts the catalogue
and validates each call against its advertised JSON Schema before dispatch.

Writes require `with_rest_schema`; their payload schemas are never inferred
from a read projection. Create takes `{ "payload": {...} }`, update takes
`{ "id": "...", "patch": {...} }`, and delete takes `{ "id": "..." }`.
Patch properties retain their declared types without requiring omitted fields;
the REST surface validates the merged document. Payload `id` is ordinary payload
when the document schema permits it; only the outer `id` selects the route.
IDs are encoded as one path segment. HTTP redirects are not followed.

## Protocol and bounds

Initialize with `protocolVersion`, `capabilities`, and `clientInfo`, then send
`notifications/initialized` before tools. ID-less methods and notifications never
execute tools. Batch arrays and invalid envelopes are rejected. Invalid tool
arguments and unknown tool names use JSON-RPC `-32602`; backend failures use
an `isError` tool result. Successful text JSON and object `structuredContent`
represent the same value, wrapping nonobjects as `{ "result": value }`.

The default input-frame bound is 1 MiB. Set `MESHQL_MCP_MAX_FRAME_BYTES` to a
positive integer; invalid configuration fails at startup. Oversized frames are
drained through their newline without dispatching and the stream remains usable.
Calls have a 30-second deadline and HTTP responses a 1-MiB bound. Closing stdin
drains complete requests and exits. Request handling is serial.

## Compatibility and certification

The earlier experimental API was not MCP-certified. Its builder and handler
variants remain available, with these intentional behavior changes: reads-only
SDL derivation, explicit REST schemas, payload/patch envelopes, typed argument
validation, initialization state, optional temporal reads, and no environment
identity gate. Legacy `GraphQuery` templates accept quoted scalar string values;
compound values or unquoted string placeholders are refused. New custom queries
should use `GraphQueryVariables`. REST JSON templates preserve typed values.

The shared required tier is `contract/features/mcplette/` (the Rust checkout uses
`meshql-cert/tests/features/contract`). It runs once against a real SQLite
backend and actual stdio processes, independently of the existing store matrix:

```sh
cargo build -p meshql-mcp --features cert-fixture --bins
python3 meshql-cert/tests/features/contract/cert/mcp/run.py \
  --adapter target/debug/mcp-cert-adapter --output target/mcp-cert/results.json
```

The fixture adapter's control endpoint exists only for certification. It observes
and seeds the normal backend; no certified operation is answered by a fake store.

Derived read arguments preserve SDL nullability (including list items), required
arguments, and argument defaults in GraphQL variable declarations. GraphQL `ID`
accepts strings or integers; `Int` is bounded to signed 32 bits. Custom scalar,
enum, and input-object arguments currently require an explicitly configured
capability and produce a startup diagnostic when encountered during derivation.
