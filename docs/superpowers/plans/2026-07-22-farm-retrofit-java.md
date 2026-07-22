# Farm Event-Sourcing Retrofit (Java) Implementation Plan

> **This is the Java leg of a three-language retrofit.** Sibling plans: `2026-07-22-farm-retrofit-rust.md` and `2026-07-22-farm-retrofit-ts.md` (same directory). All three implement the same approved spec, translated to each language's conventions. This plan is self-contained — you do not need the other two to execute it — but do not assume they've landed; do not depend on artifacts they might produce.
>
> **Repository:** this plan's changes land in `/tank/repos/tailoredshapes/meshql` (the Java monorepo — NOT `meshql-rs`, which is where this plan document itself lives). All file paths below are relative to `/tank/repos/tailoredshapes/meshql` unless stated otherwise.
>
> **Worktree required:** per `superpowers:subagent-driven-development`, execute this plan in a dedicated git worktree, not directly on `meshql`'s main branch. Create it with `superpowers:using-git-worktrees` before starting Task 1.
>
> **No push access:** this environment has no push credentials configured for the AI agent against `meshql`. Every task below commits locally only. The final task ends with an explicit reminder that the user must push the branch themselves — do not attempt `git push`, and do not ask for credentials.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retrofit `examples/farm` from plain CRUD to an event-sourced shape — `lay_report` becomes a create-only domain event with a standardized `{henId, eggs, timeOfDay}` payload, `hen` loses its legacy `eggs` field, a new `hen_productivity` restlette+graphlette pair becomes the read/worker-write projection, and Casbin RBAC is wired for real (worker-only writes to `hen_productivity`, create-only writes to `lay_report`) — closing the two verified gaps in Java's write-auth dispatch: `create()` calls no authorization method at all today, and `Auth` has no verb concept for `create`/`update`/`delete` to begin with.

**Architecture:** Two small, backward-compatible additions to core MeshQL (not just the farm example): a new `Auth.authorizeAction(credentials, action)` default method (implemented for real by `CasbinAuth`, using its already-present but previously-unused `obj`/`act` Casbin model dimensions) wired into `CrudHandler.create/update/remove`; and an optional per-restlette `Auth` override on `RestletteConfig` so `examples/farm`'s `Main.java` can attach a distinct `CasbinAuth` instance (its own model+policy) to the `lay_report` and `hen_productivity` restlettes without disturbing the single shared `Auth` every other restlette and graphlette still uses. `hen_productivity` is wired as an entirely ordinary restlette+graphlette pair — nothing about its plumbing differs from `farm`/`coop`/`hen`; only its attached `Auth` instance's policy differs.

**Tech Stack:** Java 21, Maven multi-module reactor, JUnit 5 + Mockito, jcasbin 1.55.0 (already a transitive convention via `auth/casbin`), Jackson, `com.networknt:json-schema-validator`, MongoDB (farm's existing storage), TypeScript/Cucumber + Vitest for farm's existing BDD suite (Testcontainers-driven, Dockerized).

---

## Facts verified directly against source (do not re-derive, do not second-guess)

- **`CrudHandler.create()` calls no authorization method at all.** It calls `authorizer.getAuthToken(payload)` (note: passes the *request body* as context, not headers — see Task 5) purely to stamp `Envelope.authorizedTokens`, then writes unconditionally. `update()`/`remove()` call `getAuthTokens(request)` (header-based) but only use the result for `Envelope.authorizedTokens` / `repository.read`/`repository.remove` — never for a write-permission check. `Auth.isAuthorized(credentials, Envelope)` exists but is called **only** by `Searcher` implementations (`MongoSearcher`, `InMemorySearcher`, `RDBMSSearcher`, etc.) for read-time filtering of search/list results — never anywhere in the write path. Confirmed via `grep -rn isAuthorized` across the whole repo: every call site is inside a `*Searcher`/`*Repository` read path.
- **`Auth` interface (`core/src/main/java/com/meshql/core/Auth.java`) has exactly two methods**, `getAuthToken` and `isAuthorized` — no verb/action parameter anywhere.
- **`RestletteConfig` (`core/src/main/java/com/meshql/core/config/RestletteConfig.java`) has no `Auth` field.** `Server.init()` computes exactly one `Auth auth = processAuth(config)` (which, incidentally, is itself a stub — `config.casbinParams()` is parsed but never turned into a real `CasbinAuth`; it always falls back to `NoAuth` and logs a warning) and passes that same instance to **every** graphlette and restlette. There is currently no route-builder mechanism in Java for a distinct `Auth` per restlette — this plan adds one (Task 3/4), it does not already exist.
- **`CasbinAuth.isAuthorized()` never calls `enforcer.enforce()`.** It does manual list-overlap between `credentials` and `Envelope.authorizedTokens()`. The Casbin model already used in tests (`auth/casbin/src/test/resources/model.conf`) already has 3-field policies (`sub, obj, act`) — the `act` dimension is defined but currently unconsumed by any Java code. This plan's `authorizeAction` is what finally exercises it via `enforcer.enforce(role, obj, act)`.
- **`MongoPlugin.createRepository(sc, auth)` discards its `auth` parameter** — `MongoRepository` doesn't even store an `Auth`. Only `MongoPlugin.createSearcher` uses the plugin's constructor-injected `Auth` (for read-time `isAuthorized` filtering). This confirms the write path's `Auth` really does flow through `CrudHandler`'s own `authorizer` field alone — that's the correct, and only, place to add the new check.
- **`hen.schema.json` has a legacy `eggs` field**; `lay_report.schema.json` has `hen_id`/`time_of_day`/`eggs` (snake_case). Both confirmed present in `examples/farm/config/json/`.
- **`ManifestGenerator.generate()` already emits both `graph` and `api` surfaces for every entity that has both files** — it applies zero verb/noun filtering. Nothing to remove there (per spec §"Manifest generator changes", item 1). `hen_productivity` will appear automatically once its `.graphql`/`.schema.json` files exist — no generator code change needed, just new config files and a conformance-test assertion (Task 9/12).
- **`examples/farm/Dockerfile`** does `COPY --from=builder /build/examples/farm/config /app/config` — anything placed under `examples/farm/config/` (including a new `casbin/` subdirectory) ships in the image automatically. `Main.java` loads JSON schemas via hardcoded `/app/config/json/...` absolute paths (container-only convention) — new Casbin file paths in `Main.java` follow the same `/app/config/...` convention.

---

## Task 1: `Auth` interface — add the verb-aware `authorizeAction` method

**Files:**
- Modify: `core/src/main/java/com/meshql/core/Auth.java`
- Modify: `auth/noop/src/main/java/com/meshql/auth/noop/NoAuth.java`
- Modify: `auth/noop/pom.xml` (found during plan review: this module has no test dependency at all today — see Step 2 below)
- Test: `auth/noop/src/test/java/com/meshql/auth/noop/NoAuthTest.java` (create if it doesn't already exist)

- [ ] **Step 1: Check whether `NoAuthTest` already exists**

```bash
find /tank/repos/tailoredshapes/meshql/auth/noop/src/test -iname "*.java"
```

If it exists, read it first so your new test methods match its existing style. If it doesn't exist, you're creating it fresh in Step 3.

- [ ] **Step 2: Add the missing JUnit test dependency**

`auth/noop` has never had a test class before this task, and `auth/noop/pom.xml` currently declares no test dependencies at all (confirmed via `mvn -pl auth/noop dependency:tree` — no `org.junit.jupiter:*` artifact anywhere, and the root `meshql-java` parent pom only pins versions via `dependencyManagement`, it doesn't add the dependency itself). Without this, Step 4's test won't even compile (`import org.junit.jupiter.api.Test;` fails with "package does not exist"), which would be confusing since it looks like a different failure than the one Step 4 describes.

Edit `auth/noop/pom.xml`, adding inside the existing `<dependencies>` block (after the `core` dependency), matching the exact pattern already used in `auth/casbin/pom.xml`:

```xml
        <dependency>
            <groupId>org.junit.jupiter</groupId>
            <artifactId>junit-jupiter-api</artifactId>
            <scope>test</scope>
        </dependency>
        <dependency>
            <groupId>org.junit.jupiter</groupId>
            <artifactId>junit-jupiter-engine</artifactId>
            <scope>test</scope>
        </dependency>
```

No `<version>` needed — it's managed centrally via the parent POM's `dependencyManagement`, same as every other module that already has tests.

- [ ] **Step 3: Write the failing test**

Add to (or create) `auth/noop/src/test/java/com/meshql/auth/noop/NoAuthTest.java`:

```java
package com.meshql.auth.noop;

import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

class NoAuthTest {

    @Test
    void authorizeAction_returnsTrue_whenConstructedAuthed() {
        NoAuth auth = new NoAuth();

        assertTrue(auth.authorizeAction(List.of(), "create"));
        assertTrue(auth.authorizeAction(List.of("anyone"), "update"));
        assertTrue(auth.authorizeAction(List.of("anyone"), "delete"));
    }

    @Test
    void authorizeAction_returnsFalse_whenConstructedUnauthed() {
        NoAuth auth = new NoAuth(List.of("token"), false);

        assertFalse(auth.authorizeAction(List.of("token"), "create"));
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

```bash
mvn test -pl auth/noop -am -Dtest=NoAuthTest
```

Expected: compile error — `authorizeAction` does not exist on `Auth`/`NoAuth`. (With Step 2's dependency in place, this is a normal "method doesn't exist yet" compile failure — not a "package org.junit.jupiter.api does not exist" failure. If you see the latter, Step 2 wasn't applied correctly.)

- [ ] **Step 5: Add the interface method**

Edit `core/src/main/java/com/meshql/core/Auth.java`:

```java
package com.meshql.core;

import com.tailoredshapes.stash.Stash;
import java.util.List;

public interface Auth {
    List<String> getAuthToken(Stash context);
    boolean isAuthorized(List<String> credentials, Envelope data);

    /**
     * Verb-aware write authorization: may these credentials perform this
     * action ("create"/"update"/"delete") at all? This is distinct from
     * {@link #isAuthorized}, which is envelope-scoped ABAC consulted at
     * read time (list/search) against one document's authorizedTokens.
     * authorizeAction has no envelope to inspect — CrudHandler calls it
     * before attempting a write, so implementations needing per-entity
     * discrimination (e.g. CasbinAuth) get it by being constructed once
     * per restlette with a distinct policy, not by a resource parameter
     * here (see RestletteConfig.auth()).
     *
     * Default is permissive so every existing Auth implementation keeps
     * compiling and behaving exactly as before; override to enforce real
     * verb-based policy.
     */
    default boolean authorizeAction(List<String> credentials, String action) {
        return true;
    }
}
```

- [ ] **Step 6: Override in `NoAuth` for explicit, testable semantics**

Edit `auth/noop/src/main/java/com/meshql/auth/noop/NoAuth.java`, adding after `isAuthorized`:

```java
    @Override
    public boolean authorizeAction(List<String> credentials, String action) {
        return authed;
    }
```

- [ ] **Step 7: Run the test to verify it passes**

```bash
mvn test -pl auth/noop -am -Dtest=NoAuthTest
```

Expected: `BUILD SUCCESS`, `Tests run: 2, Failures: 0, Errors: 0`.

- [ ] **Step 8: Run the full `core` and `auth` test suites to confirm no regression**

```bash
mvn test -pl core,auth/noop,auth/jwt,auth/casbin -am
```

Expected: `BUILD SUCCESS` — `JWTSubAuthorizer` and `CasbinAuth` compile unchanged (they inherit the default `authorizeAction`; `CasbinAuth` gets its real override in Task 2).

- [ ] **Step 9: Commit**

```bash
git add core/src/main/java/com/meshql/core/Auth.java \
        auth/noop/src/main/java/com/meshql/auth/noop/NoAuth.java \
        auth/noop/pom.xml \
        auth/noop/src/test/java/com/meshql/auth/noop/NoAuthTest.java
git commit -m "feat(auth): add verb-aware Auth.authorizeAction, wire into NoAuth"
```

---

## Task 2: `CasbinAuth` — implement `authorizeAction` for real

**Files:**
- Modify: `auth/casbin/src/main/java/com/meshql/auth/casbin/CasbinAuth.java`
- Modify: `auth/casbin/src/test/resources/policy.csv`
- Test: `auth/casbin/src/test/java/com/meshql/auth/casbin/CasbinAuthTest.java`

**Design note:** `authorizeAction` hardcodes the Casbin object to the literal `"/api"` — this is the direct Java analog of Rust's `authorize_action`, which hardcodes the same string for the same reason (per spec §"Auth", gap 1's resolution): per-entity discrimination happens by which `CasbinAuth` *instance* (its own model+policy files) handles a given restlette's requests, decided in `Main.java`'s wiring (Task 11), not by a resource parameter threaded through the engine call.

- [ ] **Step 1: Write the failing tests**

The existing `auth/casbin/src/test/resources/model.conf` already supports `sub, obj, act` policies (no change needed there). Add two policy lines to `auth/casbin/src/test/resources/policy.csv` (existing file — append, don't replace):

```
p, admin, data1, read
p, admin, data1, write
p, editor, data1, read
p, viewer, data1, read

g, user1, admin
g, user2, editor
g, user3, viewer

p, admin, /api, create
p, admin, /api, update
p, editor, /api, create
```

Add to `auth/casbin/src/test/java/com/meshql/auth/casbin/CasbinAuthTest.java`, after the existing `isAuthorized` test block (before "Factory method tests"):

```java
    // ==================== authorizeAction tests ====================

    @Test
    void authorizeAction_shouldReturnTrue_whenRoleHasPolicyForAction() {
        boolean result = casbinAuth.authorizeAction(List.of("admin"), "create");

        assertTrue(result);
    }

    @Test
    void authorizeAction_shouldReturnFalse_whenRoleHasNoPolicyForAction() {
        // 'editor' has 'create' but not 'update' on /api
        boolean result = casbinAuth.authorizeAction(List.of("editor"), "update");

        assertFalse(result);
    }

    @Test
    void authorizeAction_shouldReturnFalse_whenNoRoleHasAnyPolicyForResource() {
        // 'viewer' has no /api policy at all
        boolean result = casbinAuth.authorizeAction(List.of("viewer"), "create");

        assertFalse(result);
    }

    @Test
    void authorizeAction_shouldReturnTrue_whenAnyCredentialMatches() {
        boolean result = casbinAuth.authorizeAction(List.of("viewer", "admin"), "update");

        assertTrue(result);
    }

    @Test
    void authorizeAction_shouldReturnFalse_whenCredentialsIsEmpty() {
        boolean result = casbinAuth.authorizeAction(List.of(), "create");

        assertFalse(result);
    }

    @Test
    void authorizeAction_shouldReturnFalse_whenCredentialsIsNull() {
        boolean result = casbinAuth.authorizeAction(null, "create");

        assertFalse(result);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
mvn test -pl auth/casbin -am -Dtest=CasbinAuthTest
```

Expected: compile error (`authorizeAction` not defined on `CasbinAuth` — it currently only inherits the permissive interface default, so `authorizeAction_shouldReturnFalse_*` tests would actually compile-but-fail rather than error, while nothing is overridden yet). To be precise: since Task 1 added a *default* method, this will compile; run it now and confirm the "should return false" tests fail (they'll get `true` from the permissive default) while "should return true" tests pass. That's the expected red state — record the actual failure count before proceeding.

- [ ] **Step 3: Implement `authorizeAction`**

Edit `auth/casbin/src/main/java/com/meshql/auth/casbin/CasbinAuth.java`, adding after `isAuthorized`:

```java
    /**
     * Verb-aware write authorization. Checks each credential (role) from
     * getAuthToken against the enforcer for the fixed object "/api" — see
     * the class-level design note on why the object is hardcoded rather
     * than parameterized.
     *
     * @param credentials List of roles from getAuthToken
     * @param action The verb being attempted: "create", "update", or "delete"
     * @return true if any role has a matching Casbin policy for this action
     */
    @Override
    public boolean authorizeAction(List<String> credentials, String action) {
        if (credentials == null || credentials.isEmpty()) {
            return false;
        }

        return credentials.stream().anyMatch(role -> enforcer.enforce(role, "/api", action));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
mvn test -pl auth/casbin -am -Dtest=CasbinAuthTest
```

Expected: `BUILD SUCCESS`, all `CasbinAuthTest` tests pass (existing + 6 new).

- [ ] **Step 5: Commit**

```bash
git add auth/casbin/src/main/java/com/meshql/auth/casbin/CasbinAuth.java \
        auth/casbin/src/test/resources/policy.csv \
        auth/casbin/src/test/java/com/meshql/auth/casbin/CasbinAuthTest.java
git commit -m "feat(auth): implement CasbinAuth.authorizeAction via enforcer.enforce"
```

---

## Task 3: `RestletteConfig` — optional per-restlette `Auth` override

**Files:**
- Modify: `core/src/main/java/com/meshql/core/config/RestletteConfig.java`
- Modify: `core/pom.xml` (found during plan review: `core` has no test dependency at all today — see Step 2 below)
- Test: `core/src/test/java/com/meshql/core/config/RestletteConfigTest.java` (create if it doesn't already exist — check first)

- [ ] **Step 1: Check for an existing test file**

```bash
find /tank/repos/tailoredshapes/meshql/core/src/test -iname "RestletteConfigTest.java"
```

- [ ] **Step 2: Add the missing JUnit test dependency**

`core/src/test` exists (it has `logback-test.xml`) but has never had an actual test class before this task, and `core/pom.xml` currently declares no `org.junit.jupiter:*` dependency at all (confirmed via `mvn -pl core dependency:tree`; the root `meshql-java` parent pom only pins JUnit's version via `dependencyManagement`, it doesn't add the dependency itself). Without this, Step 4's test won't compile at all (`import org.junit.jupiter.api.Test;` fails with "package does not exist"), not the ".auth(...) method missing" failure Step 4 describes.

Edit `core/pom.xml`, adding inside the existing `<dependencies>` block, matching the exact pattern already used in `auth/casbin/pom.xml`:

```xml
        <dependency>
            <groupId>org.junit.jupiter</groupId>
            <artifactId>junit-jupiter-api</artifactId>
            <scope>test</scope>
        </dependency>
        <dependency>
            <groupId>org.junit.jupiter</groupId>
            <artifactId>junit-jupiter-engine</artifactId>
            <scope>test</scope>
        </dependency>
```

No `<version>` needed — managed centrally via the parent POM's `dependencyManagement`.

- [ ] **Step 3: Write the failing test**

Create (or extend) `core/src/test/java/com/meshql/core/config/RestletteConfigTest.java`:

```java
package com.meshql.core.config;

import com.meshql.core.Auth;
import com.meshql.core.Envelope;
import com.tailoredshapes.stash.Stash;
import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;

class RestletteConfigTest {

    // `core` has no dependency on any Auth implementation module (auth/noop
    // depends on core, not the reverse — a core -> auth/noop test dependency
    // would create a Maven reactor cycle). Use an inline anonymous Auth
    // instead of pulling in NoAuth; this matches the test-double pattern
    // already used elsewhere in this plan (see Task 4's ServerTest).
    private static final Auth STUB_AUTH = new Auth() {
        @Override public List<String> getAuthToken(Stash context) { return List.of(); }
        @Override public boolean isAuthorized(List<String> credentials, Envelope data) { return true; }
    };

    @Test
    void auth_defaultsToNull_whenNotSet() {
        RestletteConfig config = RestletteConfig.builder()
                .path("/thing/api")
                .storage(new StorageConfig("memory"))
                .build();

        assertNull(config.auth());
    }

    @Test
    void auth_isSettable_forPerRestletteOverride() {
        RestletteConfig config = RestletteConfig.builder()
                .path("/thing/api")
                .storage(new StorageConfig("memory"))
                .auth(STUB_AUTH)
                .build();

        assertNotNull(config.auth());
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

```bash
mvn test -pl core -am -Dtest=RestletteConfigTest
```

Expected: compile error — `RestletteConfig.Builder` has no `.auth(...)` method and `RestletteConfig` has no `auth()` accessor. (With Step 2's dependency in place, this is a normal "method doesn't exist yet" compile failure, not a "package org.junit.jupiter.api does not exist" one.)

- [ ] **Step 5: Add the `auth` field**

Edit `core/src/main/java/com/meshql/core/config/RestletteConfig.java`:

```java
package com.meshql.core.config;

import com.meshql.core.Auth;
import com.networknt.schema.JsonSchema;

import java.util.ArrayList;
import java.util.List;

public record RestletteConfig(
        List<String> tokens,
        String path,
        int port,
        StorageConfig storage,
        JsonSchema schema,
        Auth auth
) {
    public static Builder builder() {
        return new Builder();
    }

    public static class Builder {
        private List<String> tokens = new ArrayList<>();
        private String path;
        private int port = 3033;
        private StorageConfig storage;
        private JsonSchema schema;
        private Auth auth;

        public Builder tokens(List<String> tokens) {
            this.tokens = new ArrayList<>(tokens);
            return this;
        }

        public Builder token(String token) {
            this.tokens.add(token);
            return this;
        }

        public Builder path(String path) {
            this.path = path;
            return this;
        }

        public Builder port(int port) {
            this.port = port;
            return this;
        }

        public Builder storage(StorageConfig storage) {
            this.storage = storage;
            return this;
        }

        public Builder schema(JsonSchema schema) {
            this.schema = schema;
            return this;
        }

        /**
         * Optional per-restlette Auth override. When null (the default),
         * the restlette uses whatever shared Auth the server was
         * constructed with (Server.processRestlette falls back to it).
         * Set this when a restlette needs a distinct policy from every
         * other restlette in the same Config — e.g. a worker-only
         * projection, or a create-only event entity.
         */
        public Builder auth(Auth auth) {
            this.auth = auth;
            return this;
        }

        public RestletteConfig build() {
            if (path == null || path.isBlank()) {
                throw new IllegalArgumentException("restlette path is required");
            }
            if (storage == null) {
                throw new IllegalArgumentException("restlette storage is required");
            }
            return new RestletteConfig(List.copyOf(tokens), path, port, storage, schema, auth);
        }
    }
}
```

- [ ] **Step 6: Run the test to verify it passes**

```bash
mvn test -pl core -am -Dtest=RestletteConfigTest
```

Expected: `BUILD SUCCESS`, `Tests run: 2, Failures: 0, Errors: 0`.

- [ ] **Step 7: Confirm the record change doesn't break other callers**

```bash
mvn compile -pl core,server,api/restlette,examples/farm -am
```

Expected: `BUILD SUCCESS`. (The 6-arg canonical record constructor changed shape; any code constructing `new RestletteConfig(...)` positionally — as opposed to via `.builder()` — would break here. `grep -rn "new RestletteConfig(" --include="*.java"` first if this fails, to find and fix positional call sites.)

- [ ] **Step 8: Commit**

```bash
git add core/src/main/java/com/meshql/core/config/RestletteConfig.java \
        core/src/test/java/com/meshql/core/config/RestletteConfigTest.java \
        core/pom.xml
git commit -m "feat(core): optional per-restlette Auth override on RestletteConfig"
```

---

## Task 4: `Server` — honor the per-restlette `Auth` override

**Files:**
- Modify: `server/src/main/java/com/meshql/server/Server.java:169-182` (`processRestlette`)
- Test: `server/src/test/java/com/meshql/server/ServerTest.java`

- [ ] **Step 1: Read the existing `ServerTest.java`** to match its setup style before adding to it:

```bash
cat /tank/repos/tailoredshapes/meshql/server/src/test/java/com/meshql/server/ServerTest.java
```

- [ ] **Step 2: Write the failing test**

Add a test to `ServerTest.java` that boots a `Server` with two restlettes on the same in-memory storage type, one with a per-restlette `Auth` override that denies everything, one without an override (falling back to a permissive shared `Auth`), then asserts the override actually takes effect over HTTP. Match the existing file's port-allocation and setup/teardown pattern (read it in Step 1 before writing this — do not guess a port that collides with another test in the same class). Sketch (adapt names/imports to what Step 1 revealed):

```java
@Test
void restletteConfig_authOverride_takesPrecedenceOverSharedAuth() throws Exception {
    // Denies every write regardless of action.
    Auth denyAll = new Auth() {
        @Override public List<String> getAuthToken(Stash context) { return List.of(); }
        @Override public boolean isAuthorized(List<String> credentials, Envelope data) { return true; }
        @Override public boolean authorizeAction(List<String> credentials, String action) { return false; }
    };

    Stash schema = stash(
            "type", "object",
            "properties", stash("name", stash("type", "string")),
            "required", list("name"));
    JsonSchemaFactory factory = JsonSchemaFactory.getInstance(SpecVersion.VersionFlag.V7);
    ObjectMapper mapper = new ObjectMapper();
    var jsonSchema = factory.getSchema(mapper.valueToTree(schema));

    int port = /* pick an unused port per this file's convention */;
    Config config = Config.builder()
            .port(port)
            .restlette(RestletteConfig.builder()
                    .path("/locked/api")
                    .port(port)
                    .storage(new StorageConfig("memory"))
                    .schema(jsonSchema)
                    .auth(denyAll))
            .build();

    Server server = new Server(Map.of("memory", new InMemoryPlugin()));
    try {
        server.init(config);

        HttpClient client = HttpClient.newHttpClient();
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + port + "/locked/api"))
                .header("Content-Type", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString("{\"name\":\"x\"}"))
                .build();
        HttpResponse<String> response = client.send(request, HttpResponse.BodyHandlers.ofString());

        assertEquals(403, response.statusCode());
    } finally {
        server.stop();
    }
}
```

This test depends on Task 5 (`CrudHandler` actually calling `authorizeAction`) to produce a real `403` — that's fine; Tasks 3-5 land together before this is expected to go green. If you're executing tasks strictly in order and Task 5 isn't done yet, this test will fail with `201` instead of `403` (the override is wired but not yet consulted) — that's expected red state for *this* task; re-run it after Task 5.

- [ ] **Step 3: Run the test to verify it fails (or note deferred-red state)**

```bash
mvn test -pl server -am -Dtest=ServerTest#restletteConfig_authOverride_takesPrecedenceOverSharedAuth
```

Expected before Task 5 lands: `201` instead of `403` — the assertion fails, confirming the override isn't consulted yet (because `CrudHandler` doesn't call `authorizeAction` until Task 5). Expected after Task 5 lands (re-run at end of Task 5): `403`, test passes.

- [ ] **Step 4: Wire the override in `Server.processRestlette`**

Edit `server/src/main/java/com/meshql/server/Server.java`, in `processRestlette`:

```java
    private void processRestlette(ServletContextHandler context, RestletteConfig config, Auth auth) {
        // Create validator - convert JsonSchema to Map
        Map<String, Object> schemaMap = objectMapper.convertValue(
            config.schema().getSchemaNode(),
            Map.class
        );
        Validator validator = new JSONSchemaValidator(schemaMap);

        // A restlette may carry its own Auth (RestletteConfig.auth()) when
        // it needs a policy distinct from every other restlette in this
        // Config — e.g. a worker-only projection. Falls back to the
        // server-wide shared Auth when unset.
        Auth effectiveAuth = config.auth() != null ? config.auth() : auth;

        // Create and register Restlette
        Restlette restlette = new Restlette(config, plugins, effectiveAuth, validator);

        // Mount the servlet at the configured path with wildcard
        context.addServlet(new ServletHolder(restlette), config.path() + "/*");
    }
```

- [ ] **Step 5: Run the full `server` test suite**

```bash
mvn test -pl server -am
```

Expected: `BUILD SUCCESS`. The new test is expected to still show `201` (red) until Task 5 lands — confirm every *other* `ServerTest` case still passes (no regression from the `processRestlette` change itself).

- [ ] **Step 6: Commit**

```bash
git add server/src/main/java/com/meshql/server/Server.java \
        server/src/test/java/com/meshql/server/ServerTest.java
git commit -m "feat(server): honor RestletteConfig's per-restlette Auth override"
```

---

## Task 5: `CrudHandler` — wire `authorizeAction` into create/update/remove

**Files:**
- Modify: `api/restlette/src/main/java/com/meshql/api/restlette/CrudHandler.java:47-75,103-161,249-262` (the third range is the `getAuthTokens` helper itself, see the third design note below)
- Test: `api/restlette/src/test/java/com/meshql/api/restlette/CrudHandlerTest.java`

**Design note — a second, necessary fix bundled here:** `create()` currently calls `authorizer.getAuthToken(payload)`, passing the *parsed request body* as the auth context. Every other write/read handler builds a header-based context via the private `getAuthTokens(request)` helper (`Authorization` header → `Stash` with a `headers` key). This inconsistency is silently harmless today only because nothing calls `isAuthorized`/`authorizeAction` from `create()` and `NoAuth` ignores its context entirely. Once `authorizeAction` is wired into `create()`, this bug becomes load-bearing: any header-based `Auth` (JWT, CasbinAuth-wrapping-JWT) would authenticate every `create()` call as anonymous, since the JWT `Authorization` header was never in the body-as-context. Fix it in this task — `create()` must use `getAuthTokens(request)` like the other handlers.

**Design note — a third, necessary fix bundled here, found during plan review:** the private `getAuthTokens(request)` helper itself (current source, unmodified so far by this plan) *short-circuits to `List.of()` and never calls `authorizer.getAuthToken(...)` at all* when there's no `Authorization` header:

```java
private List<String> getAuthTokens(HttpServletRequest request) {
    try {
        String authHeader = request.getHeader("Authorization");
        if (authHeader != null && !authHeader.isEmpty()) {
            Stash context = new Stash();
            context.put("headers", Map.of("authorization", authHeader));
            return authorizer.getAuthToken(context);
        }
        return List.of();
    } catch (Exception e) {
        logger.error("Failed to get auth tokens", e);
        return List.of();
    }
}
```

This is silently harmless today because nothing checks the result for a write-permission decision. It becomes load-bearing — and actively wrong — once `authorizeAction` is wired in and Task 10/11's Casbin policy design wraps `NoAuth(List.of("public"), true)` for `lay_report` specifically so that *every* caller, header or not, is treated as identity `"public"` (`NoAuth.getAuthToken` ignores its context entirely and always returns its fixed token list). With the short-circuit in place, a truly anonymous request (no `Authorization` header at all) never reaches `authorizer.getAuthToken(...)`, so it gets `List.of()` — empty credentials — and `CasbinAuth.authorizeAction` denies on empty credentials by design (Task 2). That directly contradicts the spec's requirement that no-token callers can create `lay_report` events, and Task 14's README curl example would 403 instead of the `201` it documents.

**Fix**: `getAuthTokens` must always call `authorizer.getAuthToken(context)` — passing a context with a populated `"headers"` key when the header is present, and an *empty* `Stash` (no `"headers"` key) when it isn't — rather than short-circuiting before the call. This is safe for the existing `JWTSubAuthorizer`, which already checks `context.get("headers") == null` and returns an empty list in that case (confirmed in `auth/jwt/src/main/java/com/meshql/auth/jwt/JWTSubAuthorizer.java`) — its behavior is unchanged. It's the fix for `NoAuth`-wrapping compositions, which ignore context and need the call to happen at all.

Add this to `api/restlette/src/main/java/com/meshql/api/restlette/CrudHandler.java`, replacing the existing `getAuthTokens` method:

```java
    /**
     * Get authentication tokens from request. Always delegates to the
     * configured Auth, even with no Authorization header present — a
     * context-ignoring Auth (e.g. NoAuth wrapped for a specific restlette,
     * see the Casbin policy design in Task 10/11) must still get a chance
     * to inject its identity for callers who send no header at all.
     */
    private List<String> getAuthTokens(HttpServletRequest request) {
        try {
            Stash context = new Stash();
            String authHeader = request.getHeader("Authorization");
            if (authHeader != null && !authHeader.isEmpty()) {
                context.put("headers", Map.of("authorization", authHeader));
            }
            return authorizer.getAuthToken(context);
        } catch (Exception e) {
            logger.error("Failed to get auth tokens", e);
            return List.of();
        }
    }
```

Add this test, proving the exact composition the spec requires — a request with **no** `Authorization` header still gets a non-empty, context-independent identity from a `NoAuth`-style `Auth`, and a verb-denying `Auth` still denies correctly when credentials are genuinely empty (JWT-style):

```java
    @Test
    void getAuthTokens_callsAuthorizerEvenWithNoAuthorizationHeader() throws Exception {
        // Simulates the lay_report composition: CasbinAuth wraps a fixed
        // NoAuth("public") identity, so even a header-less request must
        // resolve to a non-empty credential list.
        Auth contextIgnoringAuth = new Auth() {
            @Override
            public List<String> getAuthToken(Stash context) {
                return list("public"); // ignores context entirely, like NoAuth
            }

            @Override
            public boolean isAuthorized(List<String> credentials, Envelope data) {
                return true;
            }

            @Override
            public boolean authorizeAction(List<String> credentials, String action) {
                return !credentials.isEmpty() && credentials.contains("public");
            }
        };
        CrudHandler handler = new CrudHandler(contextIgnoringAuth, repository, validator, List.of());
        when(mockRequest.getHeader("Authorization")).thenReturn(null); // no header at all
        when(mockRequest.getReader()).thenReturn(new BufferedReader(new StringReader("{\"name\":\"Henny\",\"eggs\":5}")));

        handler.create(mockRequest, mockResponse);

        verify(mockResponse).setStatus(HttpServletResponse.SC_CREATED);
    }

    @Test
    void getAuthTokens_stillDeniesWhenNoHeaderAndAuthorizerNeedsRealCredentials() throws Exception {
        // Simulates the hen_productivity composition: a JWT-style Auth that
        // legitimately returns empty credentials for a header-less request,
        // which authorizeAction must still deny.
        CrudHandler handler = new CrudHandler(new VerbDenyingAuth("create") {
            @Override
            public List<String> getAuthToken(Stash context) {
                return list(); // no header -> no identity, unlike NoAuth
            }
        }, repository, validator, List.of());
        when(mockRequest.getHeader("Authorization")).thenReturn(null);
        when(mockRequest.getReader()).thenReturn(new BufferedReader(new StringReader("{\"name\":\"Henny\",\"eggs\":5}")));

        handler.create(mockRequest, mockResponse);

        verify(mockResponse).setStatus(HttpServletResponse.SC_FORBIDDEN);
    }
```

(The second test subclasses `VerbDenyingAuth` from Step 1 above purely to reuse its `authorizeAction` deny-list logic; `VerbDenyingAuth`'s own `getAuthToken` already returns a non-empty token unconditionally, so this override makes it behave like a real JWT authorizer with no bearer token instead.)

- [ ] **Step 1: Write the failing tests**

Add to `api/restlette/src/test/java/com/meshql/api/restlette/CrudHandlerTest.java`, a small deny-by-action test double and new test methods. First add the double as a private static nested class near the top of the test class:

```java
    private static class VerbDenyingAuth implements Auth {
        private final java.util.Set<String> deniedActions;

        VerbDenyingAuth(String... deniedActions) {
            this.deniedActions = java.util.Set.of(deniedActions);
        }

        @Override
        public List<String> getAuthToken(Stash context) {
            return list("test-caller");
        }

        @Override
        public boolean isAuthorized(List<String> credentials, Envelope data) {
            return true;
        }

        @Override
        public boolean authorizeAction(List<String> credentials, String action) {
            return !deniedActions.contains(action);
        }
    }
```

Then add these test methods:

```java
    @Test
    void create_shouldReturn403_whenAuthorizerDeniesCreate() throws Exception {
        CrudHandler denyingHandler = new CrudHandler(new VerbDenyingAuth("create"), repository, validator, List.of());
        var payload = stash("name", "Henny", "eggs", 5);
        when(mockRequest.getReader()).thenReturn(new BufferedReader(new StringReader(payload.toJSONString())));

        denyingHandler.create(mockRequest, mockResponse);

        verify(mockResponse).setStatus(HttpServletResponse.SC_FORBIDDEN);
        assertEquals(0, repository.list(list()).size());
    }

    @Test
    void update_shouldReturn403_whenAuthorizerDeniesUpdate() throws Exception {
        String henId = "hen-1";
        repository.create(new Envelope(henId, stash("name", "chuck", "eggs", 6), Instant.now(), false, List.of()), list());

        CrudHandler denyingHandler = new CrudHandler(new VerbDenyingAuth("update"), repository, validator, List.of());
        when(mockRequest.getReader()).thenReturn(new BufferedReader(new StringReader("{\"name\":\"chuck\",\"eggs\":9}")));

        denyingHandler.update(mockRequest, mockResponse, henId);

        verify(mockResponse).setStatus(HttpServletResponse.SC_FORBIDDEN);
        Envelope unchanged = repository.read(henId, list(), Instant.now()).orElseThrow();
        assertEquals(6.0, unchanged.payload().get("eggs"));
    }

    @Test
    void remove_shouldReturn403_whenAuthorizerDeniesDelete() throws Exception {
        String henId = "hen-2";
        repository.create(new Envelope(henId, stash("name", "duck", "eggs", 1), Instant.now(), false, List.of()), list());

        CrudHandler denyingHandler = new CrudHandler(new VerbDenyingAuth("delete"), repository, validator, List.of());

        denyingHandler.remove(mockRequest, mockResponse, henId);

        verify(mockResponse).setStatus(HttpServletResponse.SC_FORBIDDEN);
        assertTrue(repository.read(henId, list(), Instant.now()).isPresent());
    }

    @Test
    void create_shouldUseHeaderBasedAuthContext_notRequestBody() throws Exception {
        // Regression guard for the create()-used-body-as-context bug: a
        // custom Auth that only recognizes an Authorization header must
        // see it on create(), the same way it does on update()/remove().
        java.util.concurrent.atomic.AtomicReference<Stash> seenContext = new java.util.concurrent.atomic.AtomicReference<>();
        Auth headerInspectingAuth = new Auth() {
            @Override
            public List<String> getAuthToken(Stash context) {
                seenContext.set(context);
                return list("caller");
            }

            @Override
            public boolean isAuthorized(List<String> credentials, Envelope data) {
                return true;
            }
        };
        CrudHandler handler = new CrudHandler(headerInspectingAuth, repository, validator, List.of());
        when(mockRequest.getHeader("Authorization")).thenReturn("Bearer test-token");
        when(mockRequest.getReader()).thenReturn(new BufferedReader(new StringReader("{\"name\":\"Henny\",\"eggs\":5}")));

        handler.create(mockRequest, mockResponse);

        Stash context = seenContext.get();
        assertNotNull(context, "getAuthToken should have been called during create()");
        assertTrue(context.containsKey("headers"), "create() must build a header-based context, not pass the request body");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
mvn test -pl api/restlette -am -Dtest=CrudHandlerTest
```

Expected: the three `403` tests fail with actual status `201`/`200` (no authorization check exists yet); `create_shouldUseHeaderBasedAuthContext_notRequestBody` fails because `context` is the request body (no `"headers"` key); `getAuthTokens_callsAuthorizerEvenWithNoAuthorizationHeader` fails with actual status `403` (the pre-fix `getAuthTokens` short-circuits to `List.of()` before `authorizeAction` ever sees `"public"`); `getAuthTokens_stillDeniesWhenNoHeaderAndAuthorizerNeedsRealCredentials` passes even before the fix (denial is already the short-circuit's behavior) — that's expected, it's a regression guard, not a red/green pair on its own.

- [ ] **Step 3: Wire `authorizeAction` into `create`, `update`, `remove`; fix `create`'s context bug**

Edit `api/restlette/src/main/java/com/meshql/api/restlette/CrudHandler.java`. Replace the `create` method body:

```java
    /**
     * Handle create request
     */
    public void create(HttpServletRequest request, HttpServletResponse response) throws IOException {
        try {
            List<String> authTokens = getAuthTokens(request);
            if (!authorizer.authorizeAction(authTokens, "create")) {
                sendJsonError(response, HttpServletResponse.SC_FORBIDDEN, "Not authorized to create this resource");
                return;
            }

            String body = request.getReader().lines().collect(Collectors.joining());
            Stash payload = Stash.parseJSON(body);

            boolean isValid = validator.validate(payload).get();
            if (!isValid) {
                sendJsonError(response, HttpServletResponse.SC_BAD_REQUEST, "Invalid payload");
                return;
            }

            String id = UUID.randomUUID().toString();
            Envelope envelope = new Envelope(
                    id,
                    payload,
                    Instant.now(),
                    false,
                    authTokens);

            Envelope result = repository.create(envelope, tokens);
            setHonestyHeaders(response, result);
            sendJsonResponse(response, HttpServletResponse.SC_CREATED, result.payload());
        } catch (Exception e) {
            logger.error("Failed to create resource", e);
            sendJsonError(response, HttpServletResponse.SC_INTERNAL_SERVER_ERROR,
                "Failed to create resource");
        }
    }
```

Replace the `update` method body (add the check right after computing `authTokens`, before reading/validating the body):

```java
    /**
     * Handle update request
     */
    public void update(HttpServletRequest request, HttpServletResponse response, String id) throws IOException {
        try {
            List<String> authTokens = getAuthTokens(request);
            if (!authorizer.authorizeAction(authTokens, "update")) {
                sendJsonError(response, HttpServletResponse.SC_FORBIDDEN, "Not authorized to update this resource");
                return;
            }

            String body = request.getReader().lines().collect(Collectors.joining());
            Stash payload = Stash.parseJSON(body);

            boolean isValid = validator.validate(payload).get();
            if (!isValid) {
                sendJsonError(response, HttpServletResponse.SC_BAD_REQUEST, "Invalid payload");
                return;
            }

            Optional<Envelope> existing = repository.read(id, authTokens, Instant.now());

            if (existing.isEmpty()) {
                sendJsonError(response, HttpServletResponse.SC_NOT_FOUND, "Resource not found");
                return;
            }

            Envelope envelope = new Envelope(
                    id,
                    payload,
                    Instant.now(),
                    false,
                    authTokens);

            Envelope result = repository.create(envelope, tokens);
            setHonestyHeaders(response, result);
            sendJsonResponse(response, HttpServletResponse.SC_OK, result.payload());
        } catch (Exception e) {
            logger.error("Failed to update resource", e);
            sendJsonError(response, HttpServletResponse.SC_INTERNAL_SERVER_ERROR,
                "Failed to update resource");
        }
    }
```

Replace the `remove` method body:

```java
    /**
     * Handle delete request
     */
    public void remove(HttpServletRequest request, HttpServletResponse response, String id) throws IOException {
        try {
            List<String> authTokens = getAuthTokens(request);
            if (!authorizer.authorizeAction(authTokens, "delete")) {
                sendJsonError(response, HttpServletResponse.SC_FORBIDDEN, "Not authorized to delete this resource");
                return;
            }

            Boolean result = repository.remove(id, authTokens);

            if (!result) {
                sendJsonError(response, HttpServletResponse.SC_NOT_FOUND,
                    "Resource not found or could not be deleted");
                return;
            }

            Stash responseData = stash("id", id, "status", "deleted");
            sendJsonResponse(response, HttpServletResponse.SC_OK, responseData);
        } catch (Exception e) {
            logger.error("Failed to delete resource", e);
            sendJsonError(response, HttpServletResponse.SC_INTERNAL_SERVER_ERROR,
                "Failed to delete resource");
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
mvn test -pl api/restlette -am -Dtest=CrudHandlerTest
```

Expected: `BUILD SUCCESS`, all tests pass — including the two new `getAuthTokens_*` tests — and the pre-existing `testBasicCRUDOperations`, `testAuthorizationScenarios`, `testHonestyHeadersOnCreateReadUpdate` (unaffected — they all use `NoAuth`, whose `authorizeAction` returns `authed` = `true` by default, and `NoAuth.getAuthToken` ignores its context entirely, so neither the header-vs-body fix nor the short-circuit fix changes their behavior).

- [ ] **Step 5: Re-run Task 4's deferred-red `ServerTest` case to confirm it's now green**

```bash
mvn test -pl server -am -Dtest=ServerTest#restletteConfig_authOverride_takesPrecedenceOverSharedAuth
```

Expected: `BUILD SUCCESS` — now `403` as asserted.

- [ ] **Step 6: Run the full `api/restlette` and `server` suites**

```bash
mvn test -pl api/restlette,server -am
```

Expected: `BUILD SUCCESS`, zero failures.

- [ ] **Step 7: Commit**

```bash
git add api/restlette/src/main/java/com/meshql/api/restlette/CrudHandler.java \
        api/restlette/src/test/java/com/meshql/api/restlette/CrudHandlerTest.java
git commit -m "feat(restlette): call authorizeAction on create/update/delete; fix create()'s auth-context bug"
```

---

## Task 6: End-to-end framework proof — `RestletteAuthIntegrationTest`

**Files:**
- Create: `api/restlette/src/test/java/com/meshql/api/restlette/RestletteAuthIntegrationTest.java`

This is a real-HTTP integration test (modeled directly on the existing `RestletteIntegrationTest.java` in the same package) proving the full mechanism end to end: a `RestletteConfig` with its own `.auth(...)` override, wired through `Restlette`, actually blocks disallowed verbs and allows permitted ones — independent of the farm example, independent of Mongo/Docker, independent of Casbin (uses a tiny local `Auth` double, keeping this test's failure surface narrow: if this fails, the bug is in the wiring, not in Casbin policy content, which Task 2 already covers separately).

- [ ] **Step 1: Write the failing test**

Create `api/restlette/src/test/java/com/meshql/api/restlette/RestletteAuthIntegrationTest.java`:

```java
package com.meshql.api.restlette;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.meshql.core.Auth;
import com.meshql.core.Envelope;
import com.meshql.core.Plugin;
import com.meshql.core.Validator;
import com.meshql.core.config.RestletteConfig;
import com.meshql.core.config.StorageConfig;
import com.meshql.repositories.memory.InMemoryPlugin;
import com.networknt.schema.JsonSchemaFactory;
import com.networknt.schema.SpecVersion;
import com.tailoredshapes.stash.Stash;
import org.eclipse.jetty.ee10.servlet.ServletContextHandler;
import org.eclipse.jetty.ee10.servlet.ServletHolder;
import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.List;
import java.util.Map;
import java.util.Set;

import static com.tailoredshapes.stash.Stash.stash;
import static com.tailoredshapes.underbar.ocho.UnderBar.hash;
import static com.tailoredshapes.underbar.ocho.UnderBar.list;
import static org.junit.jupiter.api.Assertions.assertEquals;

/**
 * Proves the per-restlette Auth override mechanism (RestletteConfig.auth(),
 * Server.processRestlette, CrudHandler.authorizeAction) end to end over real
 * HTTP, independent of any specific Auth implementation (Casbin has its own
 * policy-content tests in auth/casbin). Modeled on RestletteIntegrationTest
 * in this same package.
 */
class RestletteAuthIntegrationTest {
    private static Server server;
    private static final int PORT = 4569;
    private static final String CREATE_ONLY_PATH = "/create-only/api";
    private static HttpClient httpClient;
    private static Map<String, Plugin> storageFactory;

    /** Allows exactly the given actions; denies everything else, including anonymous callers. */
    private static class VerbAllowlistAuth implements Auth {
        private final Set<String> allowedActions;

        VerbAllowlistAuth(String... allowedActions) {
            this.allowedActions = Set.of(allowedActions);
        }

        @Override
        public List<String> getAuthToken(Stash context) {
            return list("caller");
        }

        @Override
        public boolean isAuthorized(List<String> credentials, Envelope data) {
            return true;
        }

        @Override
        public boolean authorizeAction(List<String> credentials, String action) {
            return allowedActions.contains(action);
        }
    }

    @BeforeAll
    static void setUp() throws Exception {
        Stash schema = stash(
                "type", "object",
                "properties", stash("name", stash("type", "string")),
                "required", list("name"));
        ObjectMapper objectMapper = new ObjectMapper();
        JsonNode schemaNode = objectMapper.valueToTree(schema);
        JsonSchemaFactory factory = JsonSchemaFactory.getInstance(SpecVersion.VersionFlag.V7);
        var jsonSchema = factory.getSchema(schemaNode);
        Validator validator = new JSONSchemaValidator(schema);

        storageFactory = hash("memory", new InMemoryPlugin());

        var createOnlyConfig = RestletteConfig.builder()
                .path(CREATE_ONLY_PATH)
                .port(PORT)
                .storage(new StorageConfig("memory"))
                .schema(jsonSchema)
                .auth(new VerbAllowlistAuth("create"))
                .build();

        Restlette createOnlyRestlette = new Restlette(createOnlyConfig, storageFactory, new VerbAllowlistAuth("create"), validator);

        server = new Server();
        ServerConnector connector = new ServerConnector(server);
        connector.setPort(PORT);
        server.addConnector(connector);

        ServletContextHandler context = new ServletContextHandler(ServletContextHandler.SESSIONS);
        context.setContextPath("/");
        server.setHandler(context);
        context.addServlet(new ServletHolder(createOnlyRestlette), CREATE_ONLY_PATH + "/*");

        server.start();
        httpClient = HttpClient.newHttpClient();
    }

    @AfterAll
    static void tearDown() throws Exception {
        if (server != null) {
            server.stop();
        }
        storageFactory.values().forEach(Plugin::cleanUp);
    }

    @Test
    void createIsAllowed() throws Exception {
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + PORT + CREATE_ONLY_PATH))
                .header("Content-Type", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString("{\"name\":\"x\"}"))
                .build();

        HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());

        assertEquals(201, response.statusCode());
    }

    @Test
    void updateIsForbidden() throws Exception {
        // Create one first so there's an id to target.
        HttpRequest createRequest = HttpRequest.newBuilder(URI.create("http://localhost:" + PORT + CREATE_ONLY_PATH))
                .header("Content-Type", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString("{\"name\":\"y\"}"))
                .build();
        httpClient.send(createRequest, HttpResponse.BodyHandlers.ofString());

        HttpRequest updateRequest = HttpRequest.newBuilder(URI.create("http://localhost:" + PORT + CREATE_ONLY_PATH + "/whatever-id"))
                .header("Content-Type", "application/json")
                .PUT(HttpRequest.BodyPublishers.ofString("{\"name\":\"z\"}"))
                .build();

        HttpResponse<String> response = httpClient.send(updateRequest, HttpResponse.BodyHandlers.ofString());

        assertEquals(403, response.statusCode());
    }

    @Test
    void deleteIsForbidden() throws Exception {
        HttpRequest deleteRequest = HttpRequest.newBuilder(URI.create("http://localhost:" + PORT + CREATE_ONLY_PATH + "/whatever-id"))
                .DELETE()
                .build();

        HttpResponse<String> response = httpClient.send(deleteRequest, HttpResponse.BodyHandlers.ofString());

        assertEquals(403, response.statusCode());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
mvn test -pl api/restlette -am -Dtest=RestletteAuthIntegrationTest
```

Since Tasks 1-5 already landed, this should actually already pass on first run — that's fine and expected (it's a proof/regression test for behavior Tasks 1-5 just built, not new production code). If it fails, the bug is in the Task 1-5 wiring, not in this test; stop and debug via `superpowers:systematic-debugging` before continuing.

- [ ] **Step 3: Confirm it passes**

```bash
mvn test -pl api/restlette -am -Dtest=RestletteAuthIntegrationTest
```

Expected: `BUILD SUCCESS`, `Tests run: 3, Failures: 0, Errors: 0`.

- [ ] **Step 4: Commit**

```bash
git add api/restlette/src/test/java/com/meshql/api/restlette/RestletteAuthIntegrationTest.java
git commit -m "test(restlette): end-to-end proof of per-restlette verb-based Auth override"
```

---

## Task 7: `lay_report` — migrate to `{henId, eggs, timeOfDay}`, create-only contract

**Files:**
- Modify: `examples/farm/config/json/lay_report.schema.json`
- Modify: `examples/farm/config/graph/lay_report.graphql`
- Modify: `examples/farm/config/graph/hen.graphql`
- Modify: `examples/farm/config/graph/coop.graphql`
- Modify: `examples/farm/src/main/java/com/meshql/examples/farm/Main.java` (lay_report graphlette's query templates and resolver)
- Test: `examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java`

- [ ] **Step 1: Write the failing test**

Add to `examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java`:

```java
    @Test
    void layReportSchemaUsesCamelCaseFieldNames() throws Exception {
        JsonNode manifest = ManifestGenerator.generate(CONFIG_DIR);
        JsonNode layReportSchema = manifest.at("/entities/lay_report/surfaces/api/schema");
        JsonNode properties = layReportSchema.get("properties");

        assertTrue(properties.has("henId"), "lay_report must expose henId (camelCase)");
        assertTrue(properties.has("timeOfDay"), "lay_report must expose timeOfDay (camelCase)");
        assertFalse(properties.has("hen_id"), "lay_report must not retain the old snake_case hen_id");
        assertFalse(properties.has("time_of_day"), "lay_report must not retain the old snake_case time_of_day");

        List<String> required = new java.util.ArrayList<>();
        layReportSchema.get("required").forEach(n -> required.add(n.asText()));
        assertEquals(java.util.Set.of("henId", "eggs", "timeOfDay"), java.util.Set.copyOf(required));
    }
```

Add the missing `List` import if not already present: `import java.util.List;`.

- [ ] **Step 2: Run the test to verify it fails**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest#layReportSchemaUsesCamelCaseFieldNames
```

Expected: assertion failures — `properties.has("henId")` is `false`, the schema still has `hen_id`/`time_of_day`.

- [ ] **Step 3: Migrate the JSON schema**

Replace `examples/farm/config/json/lay_report.schema.json`:

```json
{
    "type": "object",
    "additionalProperties": false,
    "required": ["henId", "eggs", "timeOfDay"],
    "properties": {
        "id": {
            "type": "string",
            "format": "uuid"
        },
        "henId": {
            "type": "string",
            "format": "uuid"
        },
        "eggs": {
            "type": "integer",
            "minimum": 0,
            "maximum": 3
        },
        "timeOfDay": {
            "type": "string",
            "enum": ["morning", "afternoon", "evening"]
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest#layReportSchemaUsesCamelCaseFieldNames
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 5: Migrate the GraphQL schemas (no JUnit coverage for GraphQL SDL text content — hand-verify against the diff)**

Replace `examples/farm/config/graph/lay_report.graphql`:

```graphql
scalar Date

type Hen {
  name: String!
  id: ID
  dob: Date
}

type Query {
  getById(id: ID, at: Float): LayReport
  getByHen(id: ID, at: Float): [LayReport]
}

type LayReport {
  timeOfDay: String!
  eggs: Int!
  hen: Hen
  id: ID
}
```

(Removed the stale `eggs: Int` from the embedded `Hen` type here too — this file's `Hen` type is Task 8's concern, but leaving it inconsistent with the rest of this task's edit would fail Task 8's own conformance check. If Task 8 hasn't landed yet in your execution order, it's fine to leave this `Hen` type as-is now and let Task 8 remove `eggs` from it — just don't rename `time_of_day` there since Task 8 only touches the `eggs` field. The snippet above assumes Task 8 has not yet run; verify against the current file state before pasting.)

Edit `examples/farm/config/graph/hen.graphql`, in the embedded `LayReport` type at the bottom:

```graphql
type LayReport {
  timeOfDay: String!
  eggs: Int!
  id: ID
}
```

(Only the `time_of_day` → `timeOfDay` rename; leave the rest of this file for Task 8.)

Edit `examples/farm/config/graph/coop.graphql`, in the embedded `LayReport` type at the bottom:

```graphql
type LayReport {
  timeOfDay: String!
  eggs: Int!
  id: ID
}
```

- [ ] **Step 6: Update `Main.java`'s lay_report query templates and resolver**

Edit `examples/farm/src/main/java/com/meshql/examples/farm/Main.java`, in the `/lay_report/graph` graphlette block:

```java
                // Lay Report graphlette
                .graphlette(GraphletteConfig.builder()
                        .path("/lay_report/graph")
                        .storage(layReportDB)
                        .schema("/app/config/graph/lay_report.graphql")
                        .rootConfig(RootConfig.builder()
                                .singleton("getById", "{\"id\": \"{{id}}\"}")
                                .vector("getByHen", "{\"payload.henId\": \"{{id}}\"}")
                                .singletonResolver("hen", "henId", "getById", platformUrl + "/hen/graph")))
```

(Changed `"payload.hen_id"` → `"payload.henId"` and the FK field name `"hen_id"` → `"henId"`.)

- [ ] **Step 7: Verify the full example module compiles**

```bash
mvn compile -pl examples/farm -am
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 8: Re-run the manifest generator test suite to confirm no other regression**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest
```

Expected: `BUILD SUCCESS`. (`ManifestConformanceTest` will now fail because `config/manifest.json` is stale — that's expected and handled in Task 12, which regenerates it after all schema tasks land. Do not regenerate yet; regenerating now and again after Task 8/9 wastes a step. If you want to double check your work in the meantime, run `ManifestGeneratorTest` only, not `ManifestConformanceTest`.)

- [ ] **Step 9: Commit**

```bash
git add examples/farm/config/json/lay_report.schema.json \
        examples/farm/config/graph/lay_report.graphql \
        examples/farm/config/graph/hen.graphql \
        examples/farm/config/graph/coop.graphql \
        examples/farm/src/main/java/com/meshql/examples/farm/Main.java \
        examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java
git commit -m "feat(farm): migrate lay_report to {henId, eggs, timeOfDay}"
```

---

## Task 8: `hen` — remove the legacy `eggs` field

**Files:**
- Modify: `examples/farm/config/json/hen.schema.json`
- Modify: `examples/farm/config/graph/hen.graphql`
- Modify: `examples/farm/config/graph/coop.graphql`
- Modify: `examples/farm/config/graph/farm.graphql`
- Modify: `examples/farm/config/graph/lay_report.graphql`
- Test: `examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java`

- [ ] **Step 1: Write the failing test**

Add to `ManifestGeneratorTest.java`:

```java
    @Test
    void henSchemaNoLongerHasLegacyEggsField() throws Exception {
        JsonNode manifest = ManifestGenerator.generate(CONFIG_DIR);
        JsonNode henProperties = manifest.at("/entities/hen/surfaces/api/schema/properties");

        assertFalse(henProperties.has("eggs"),
            "hen must not carry a legacy eggs field — hen_productivity is now the sole source of truth for egg counts");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest#henSchemaNoLongerHasLegacyEggsField
```

Expected: assertion failure — `henProperties.has("eggs")` is `true`.

- [ ] **Step 3: Remove `eggs` from the JSON schema**

Replace `examples/farm/config/json/hen.schema.json`:

```json
{
    "type": "object",
    "additionalProperties": false,
    "required": ["name"],
    "properties": {
        "id": {
            "type": "string",
            "format": "uuid"
        },
        "name": {
            "type": "string",
            "faker": "person.firstName"
        },
        "coop_id": {
            "type": "string",
            "format": "uuid"
        },
        "dob": {
            "type": "string",
            "format": "date"
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest#henSchemaNoLongerHasLegacyEggsField
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 5: Remove `eggs: Int` from every embedded `Hen` type across the GraphQL schemas**

Edit `examples/farm/config/graph/hen.graphql` — this file's own primary `Hen` type:

```graphql
scalar Date

type Farm {
  name: String!
  id: ID
  coops: [Coop]
}

type Coop {
  name: String!
  farm: Farm!
  id: ID
}

type Query {
  getByName(name: String, at: Float): [Hen]
  getById(id: ID, at: Float): Hen
  getByCoop(id: ID, at: Float): [Hen]
}

type Hen {
  name: String!
  coop: Coop
  dob: Date
  id: ID
  layReports: [LayReport]
}

type LayReport {
  timeOfDay: String!
  eggs: Int!
  id: ID
}
```

Edit `examples/farm/config/graph/coop.graphql` — the embedded `Hen` type:

```graphql
scalar Date

type Farm {
  name: String!
  id: ID
}

type Query {
  getByName(name: String, at: Float): Coop
  getById(id: ID, at: Float): Coop
  getByFarm(id: ID, at: Float): [Coop]
}

type Coop {
  name: String!
  farm: Farm!
  id: ID
  hens: [Hen]
}

type Hen {
  name: String!
  dob: Date
  id: ID
  layReports: [LayReport]
}

type LayReport {
  timeOfDay: String!
  eggs: Int!
  id: ID
}
```

Edit `examples/farm/config/graph/farm.graphql` — the embedded `Hen` type:

```graphql
scalar Date

type Query {
  getById(id: ID, at: Float): Farm
}

type Farm {
  name: String!
  id: ID
  coops: [Coop]
}

type Coop {
  name: String!
  id: ID
  hens: [Hen]
}

type Hen {
  name: String!
  coop: Coop
  dob: Date
  id: ID
}
```

Edit `examples/farm/config/graph/lay_report.graphql` — the embedded `Hen` type (verify this wasn't already fixed if Task 7 ran first; if so this is a no-op):

```graphql
scalar Date

type Hen {
  name: String!
  id: ID
  dob: Date
}

type Query {
  getById(id: ID, at: Float): LayReport
  getByHen(id: ID, at: Float): [LayReport]
}

type LayReport {
  timeOfDay: String!
  eggs: Int!
  hen: Hen
  id: ID
}
```

- [ ] **Step 6: Verify compile**

```bash
mvn compile -pl examples/farm -am
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 7: Run the manifest generator tests**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest
```

Expected: `BUILD SUCCESS` (again, `ManifestConformanceTest` stays red until Task 12 — don't run it yet).

- [ ] **Step 8: Commit**

```bash
git add examples/farm/config/json/hen.schema.json \
        examples/farm/config/graph/hen.graphql \
        examples/farm/config/graph/coop.graphql \
        examples/farm/config/graph/farm.graphql \
        examples/farm/config/graph/lay_report.graphql \
        examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java
git commit -m "feat(farm): remove legacy eggs field from hen; hen_productivity is now sole source of truth"
```

---

## Task 9: `hen_productivity` — new entity, JSON schema + GraphQL schema

**Files:**
- Create: `examples/farm/config/json/hen_productivity.schema.json`
- Create: `examples/farm/config/graph/hen_productivity.graphql`
- Test: `examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java`

**Aggregate field decision:** `{henId, totalEggs, lastLaidAt}` — a running total of eggs laid, keyed by the hen it's about, plus the timestamp of the most recent contributing `lay_report`. This is a deliberately minimal projection shape (the spec leaves exact fields unsettled, calling it "not settled here"): `totalEggs` is the obvious fold over `lay_report.eggs`, `lastLaidAt` gives the FE a freshness signal without needing per-day breakdowns. `henId` is the FK back to the aggregate root, camelCase per the same `<parent>Id` convention `lay_report` just adopted. One `hen_productivity` record exists per hen (not per lay_report) — the worker looks it up by `henId` (via `getByHen`, a singleton — not a vector — query) and either creates it (first lay_report for that hen) or updates it in place (subsequent ones).

- [ ] **Step 1: Write the failing test**

Add to `ManifestGeneratorTest.java`:

```java
    @Test
    void henProductivityEntityExistsWithBothSurfaces() throws Exception {
        JsonNode manifest = ManifestGenerator.generate(CONFIG_DIR);
        JsonNode entities = manifest.get("entities");

        assertTrue(entities.has("hen_productivity"), "hen_productivity must be a generated entity");
        assertEquals("graphql", entities.at("/hen_productivity/surfaces/graph/kind").asText());
        assertEquals("/hen_productivity/graph", entities.at("/hen_productivity/surfaces/graph/path").asText());
        assertEquals("rest", entities.at("/hen_productivity/surfaces/api/kind").asText());
        assertEquals("/hen_productivity/api", entities.at("/hen_productivity/surfaces/api/path").asText());

        JsonNode properties = entities.at("/hen_productivity/surfaces/api/schema/properties");
        assertTrue(properties.has("henId"));
        assertTrue(properties.has("totalEggs"));
        assertTrue(properties.has("lastLaidAt"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest#henProductivityEntityExistsWithBothSurfaces
```

Expected: `entities.has("hen_productivity")` is `false` — no `hen_productivity.graphql` file exists yet.

- [ ] **Step 3: Create the JSON schema**

Create `examples/farm/config/json/hen_productivity.schema.json`:

```json
{
    "type": "object",
    "additionalProperties": false,
    "required": ["henId", "totalEggs", "lastLaidAt"],
    "properties": {
        "id": {
            "type": "string",
            "format": "uuid"
        },
        "henId": {
            "type": "string",
            "format": "uuid"
        },
        "totalEggs": {
            "type": "integer",
            "minimum": 0
        },
        "lastLaidAt": {
            "type": "string",
            "format": "date-time"
        }
    }
}
```

- [ ] **Step 4: Create the GraphQL schema**

Create `examples/farm/config/graph/hen_productivity.graphql`:

```graphql
scalar Date

type Hen {
  name: String!
  id: ID
  dob: Date
}

type Query {
  getById(id: ID, at: Float): HenProductivity
  getByHen(id: ID, at: Float): HenProductivity
}

type HenProductivity {
  henId: String!
  totalEggs: Int!
  lastLaidAt: Date
  hen: Hen
  id: ID
}
```

Note `getByHen` returns a single `HenProductivity`, not a list — this is a 1:1 aggregate-per-hen projection, unlike `lay_report`'s `getByHen` which returns `[LayReport]`.

- [ ] **Step 5: Run the test to verify it passes**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest#henProductivityEntityExistsWithBothSurfaces
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 6: Run the full `ManifestGeneratorTest` suite**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest
```

Expected: `BUILD SUCCESS`, all tests pass (`generatesAnEntryForEveryGraphqlFile`'s assertions for farm/coop/hen/lay_report still hold; it doesn't yet assert an exhaustive count, so the new entity doesn't break it — verify this is still true by reading the test before moving on, since Task 12 will add a stricter count-based assertion).

- [ ] **Step 7: Commit**

```bash
git add examples/farm/config/json/hen_productivity.schema.json \
        examples/farm/config/graph/hen_productivity.graphql \
        examples/farm/src/test/java/com/meshql/examples/farm/ManifestGeneratorTest.java
git commit -m "feat(farm): add hen_productivity entity schema (JSON + GraphQL)"
```

---

## Task 10: Casbin policy files for `lay_report` and `hen_productivity`

**Files:**
- Create: `examples/farm/config/casbin/model.conf`
- Create: `examples/farm/config/casbin/lay_report-policy.csv`
- Create: `examples/farm/config/casbin/hen_productivity-policy.csv`

**Design:**
- `lay_report`: everyone (no token required) may `create`; nobody may `update`/`delete` (immutable event — a correction is a new event, not an edit, per the spec). Modeled by wrapping a fixed pseudo-identity `NoAuth(List.of("public"), true)` as `CasbinAuth`'s identity source — every caller, regardless of headers, authenticates as the Casbin subject `"public"`. A `g, public, public` self-mapping makes `enforcer.getRolesForUser("public")` return `["public"]`, so the `p, public, /api, create` rule matches.
- `hen_productivity`: only the worker service account may `create`/`update`; nobody (including anonymous callers) may do anything else — the FE has no write access here at all, per the "single writer" invariant (only the worker, via CDC from `lay_report`, ever writes here). Modeled by wrapping a real `JWTSubAuthorizer` (decode-only, per existing convention — see `core/CLAUDE.md`'s JWT section) as `CasbinAuth`'s identity source: a caller must present `Authorization: Bearer <jwt with sub=worker-service>`; anonymous callers get an empty credential list from `CasbinAuth.getAuthToken`, and `authorizeAction` on an empty credential list is `false` (Task 2), so they're denied by construction, without needing an explicit deny rule.

There's no JUnit-testable behavior in this task by itself (these are plain data files, exercised by the wiring in Task 11 and the BDD suite in Task 13) — no red/green cycle here, just careful authoring. Double-check every file against the model below before moving on; a typo here fails silently (Casbin denies unknown syntax rather than erroring).

- [ ] **Step 1: Create the shared Casbin model**

Create `examples/farm/config/casbin/model.conf` (identical to the model already proven in `auth/casbin/src/test/resources/model.conf` — reuse it verbatim, don't invent a new shape):

```
[request_definition]
r = sub, obj, act

[policy_definition]
p = sub, obj, act

[role_definition]
g = _, _

[policy_effect]
e = some(where (p.eft == allow))

[matchers]
m = g(r.sub, p.sub) && r.obj == p.obj && r.act == p.act
```

- [ ] **Step 2: Create the `lay_report` policy**

Create `examples/farm/config/casbin/lay_report-policy.csv`:

```
p, public, /api, create

g, public, public
```

- [ ] **Step 3: Create the `hen_productivity` policy**

Create `examples/farm/config/casbin/hen_productivity-policy.csv`:

```
p, worker, /api, create
p, worker, /api, update

g, worker-service, worker
```

`worker-service` is the JWT `sub` claim the worker process must present. (The worker itself is built by the companion spec `2026-07-22-merkql-worker-pipeline-design.md`, out of scope here — this policy just needs to be ready for it.)

- [ ] **Step 4: Confirm the files parse as valid Casbin config with a quick scratch check**

This is not a permanent test — just a sanity check before committing, since a typo here fails silently in production:

```bash
cd /tank/repos/tailoredshapes/meshql
cat > /tmp/CasbinPolicyCheck.java << 'EOF'
import org.casbin.jcasbin.main.Enforcer;

public class CasbinPolicyCheck {
    public static void main(String[] args) {
        Enforcer layReport = new Enforcer(
            "examples/farm/config/casbin/model.conf",
            "examples/farm/config/casbin/lay_report-policy.csv");
        System.out.println("lay_report public/create: " + layReport.enforce("public", "/api", "create"));
        System.out.println("lay_report public/update: " + layReport.enforce("public", "/api", "update"));

        Enforcer henProductivity = new Enforcer(
            "examples/farm/config/casbin/model.conf",
            "examples/farm/config/casbin/hen_productivity-policy.csv");
        System.out.println("hen_productivity worker-service/create (via role): " +
            henProductivity.enforce("worker", "/api", "create"));
        System.out.println("hen_productivity worker-service getRolesForUser: " +
            henProductivity.getRolesForUser("worker-service"));
    }
}
EOF
CASBIN_JAR=$(find ~/.m2/repository/org/casbin -name "jcasbin-*.jar" | head -1)
javac -cp "$CASBIN_JAR" -d /tmp /tmp/CasbinPolicyCheck.java
java -cp "/tmp:$CASBIN_JAR:$(find ~/.m2/repository -name 'slf4j-api-*.jar' | head -1)" CasbinPolicyCheck
rm /tmp/CasbinPolicyCheck.java /tmp/CasbinPolicyCheck.class
```

Expected output:
```
lay_report public/create: true
lay_report public/update: false
hen_productivity worker-service/create (via role): true
hen_productivity worker-service getRolesForUser: [worker]
```

If `getRolesForUser` returns `[]` or `enforce` returns unexpected values, re-check the `g`/`p` lines for typos (trailing whitespace, wrong column count) before proceeding — do not paste code into Task 11 that depends on a policy file you haven't confirmed parses correctly.

- [ ] **Step 5: Commit**

```bash
git add examples/farm/config/casbin/model.conf \
        examples/farm/config/casbin/lay_report-policy.csv \
        examples/farm/config/casbin/hen_productivity-policy.csv
git commit -m "feat(farm): add Casbin policy files for lay_report (create-only) and hen_productivity (worker-only)"
```

---

## Task 11: `Main.java` — wire `hen_productivity` + attach the two `CasbinAuth` instances

**Files:**
- Modify: `examples/farm/pom.xml`
- Modify: `examples/farm/src/main/java/com/meshql/examples/farm/Main.java`

- [ ] **Step 1: Add the `casbin` and `jwt` module dependencies**

Edit `examples/farm/pom.xml`, adding after the existing `restlette` dependency:

```xml
        <dependency>
            <groupId>com.tailoredshapes</groupId>
            <artifactId>casbin</artifactId>
            <version>${project.version}</version>
        </dependency>
        <dependency>
            <groupId>com.tailoredshapes</groupId>
            <artifactId>jwt</artifactId>
            <version>${project.version}</version>
        </dependency>
```

- [ ] **Step 2: Verify the new dependencies resolve**

```bash
mvn compile -pl examples/farm -am
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 3: Add imports to `Main.java`**

Edit `examples/farm/src/main/java/com/meshql/examples/farm/Main.java`, adding to the import block:

```java
import com.meshql.auth.casbin.CasbinAuth;
import com.meshql.auth.jwt.JWTSubAuthorizer;
```

- [ ] **Step 4: Add the `hen_productivity` MongoConfig**

Edit `Main.java`, in the storage config block:

```java
        // Create storage configs for each collection
        MongoConfig farmDB = createMongoConfig(mongoUri, prefix, env, "farm");
        MongoConfig coopDB = createMongoConfig(mongoUri, prefix, env, "coop");
        MongoConfig henDB = createMongoConfig(mongoUri, prefix, env, "hen");
        MongoConfig layReportDB = createMongoConfig(mongoUri, prefix, env, "lay_report");
        MongoConfig henProductivityDB = createMongoConfig(mongoUri, prefix, env, "hen_productivity");
```

- [ ] **Step 5: Construct the two per-restlette `CasbinAuth` instances**

Edit `Main.java`, right before the `Config config = Config.builder()...` block, insert:

```java
        // Verb-aware auth for lay_report: anyone may create (it's the FE's
        // one write path for this domain event); nobody may update or
        // delete it — a correction is a new event, not an edit. Every
        // caller authenticates as the fixed Casbin subject "public"
        // (NoAuth ignores headers and always returns that token), which
        // maps via a self-referencing `g` rule to the "public" role.
        Auth layReportAuth = CasbinAuth.create(
                "/app/config/casbin/model.conf",
                "/app/config/casbin/lay_report-policy.csv",
                new NoAuth(List.of("public"), true));

        // Verb-aware auth for hen_productivity: only the worker service
        // account (identified by a JWT `sub` claim mapped to the "worker"
        // role) may create/update. The FE has no write access at all —
        // this is a worker-only projection per the "single writer"
        // invariant. Anonymous callers get an empty credential list from
        // CasbinAuth.getAuthToken and are denied by construction.
        Auth henProductivityAuth = CasbinAuth.create(
                "/app/config/casbin/model.conf",
                "/app/config/casbin/hen_productivity-policy.csv",
                new JWTSubAuthorizer());
```

- [ ] **Step 6: Add the `hen_productivity` graphlette**

Edit `Main.java`, adding after the `/lay_report/graph` graphlette block and before `// Restlettes`:

```java
                // Hen Productivity graphlette (read-only projection; writes
                // come only from the worker via its own restlette below)
                .graphlette(GraphletteConfig.builder()
                        .path("/hen_productivity/graph")
                        .storage(henProductivityDB)
                        .schema("/app/config/graph/hen_productivity.graphql")
                        .rootConfig(RootConfig.builder()
                                .singleton("getById", "{\"id\": \"{{id}}\"}")
                                .singleton("getByHen", "{\"payload.henId\": \"{{id}}\"}")
                                .singletonResolver("hen", "henId", "getById", platformUrl + "/hen/graph")))
```

- [ ] **Step 7: Add the `hen_productivity` restlette and attach the two `CasbinAuth` overrides**

Edit `Main.java`, in the restlette block — add `.auth(layReportAuth)` to the existing `/lay_report/api` restlette, and add a new `/hen_productivity/api` restlette:

```java
                .restlette(RestletteConfig.builder()
                        .path("/lay_report/api")
                        .port(port)
                        .storage(layReportDB)
                        .schema(loadJsonSchema("/app/config/json/lay_report.schema.json"))
                        .auth(layReportAuth))
                .restlette(RestletteConfig.builder()
                        .path("/hen_productivity/api")
                        .port(port)
                        .storage(henProductivityDB)
                        .schema(loadJsonSchema("/app/config/json/hen_productivity.schema.json"))
                        .auth(henProductivityAuth))
```

(`farm`/`coop`/`hen` restlettes are unchanged — they keep using the server's shared `Auth`, which stays `NoAuth`, matching the spec's "general/FE callers... authorized for create on farm/coop/hen, plus update/delete" — that's already true today and doesn't need a `CasbinAuth` instance of its own.)

- [ ] **Step 8: Update the startup log lines**

Edit `Main.java`'s logging block to mention the new endpoints:

```java
        logger.info("GraphQL endpoints:");
        logger.info("  - http://localhost:{}/farm/graph", port);
        logger.info("  - http://localhost:{}/coop/graph", port);
        logger.info("  - http://localhost:{}/hen/graph", port);
        logger.info("  - http://localhost:{}/lay_report/graph", port);
        logger.info("  - http://localhost:{}/hen_productivity/graph", port);
        logger.info("Manifest: http://localhost:{}/manifest", port);
```

- [ ] **Step 9: Verify the example compiles**

```bash
mvn compile -pl examples/farm -am
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 10: Verify the Docker image would include the new config directory (dry check, no build)**

```bash
grep -n "COPY --from=builder.*config" /tank/repos/tailoredshapes/meshql/examples/farm/Dockerfile
```

Expected: the existing `COPY --from=builder /build/examples/farm/config /app/config` line — confirm it copies the whole `config/` tree (it does, verified in the Facts section above), so `config/casbin/` ships automatically with no `Dockerfile` change needed.

- [ ] **Step 11: Commit**

```bash
git add examples/farm/pom.xml \
        examples/farm/src/main/java/com/meshql/examples/farm/Main.java
git commit -m "feat(farm): wire hen_productivity graphlette+restlette; attach CasbinAuth to lay_report and hen_productivity"
```

---

## Task 12: Regenerate the manifest; tighten conformance tests

**Files:**
- Modify: `examples/farm/config/manifest.json` (generated)
- Modify: `examples/farm/src/test/java/com/meshql/examples/farm/ManifestConformanceTest.java`

- [ ] **Step 1: Confirm `ManifestConformanceTest` is currently red**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestConformanceTest
```

Expected: `manifestMatchesRegeneration` fails — `config/manifest.json` is stale relative to the schema/config changes from Tasks 7-9.

- [ ] **Step 2: Regenerate the manifest**

```bash
mvn install -pl core,api,auth,repos/mongo,server -am -DskipTests
cd examples/farm
mvn org.codehaus.mojo:exec-maven-plugin:3.1.0:java \
    -Dexec.mainClass=com.meshql.examples.farm.GenManifest \
    -Dexec.classpathScope=compile
cd ../..
```

Expected output ends with `Wrote <path>/examples/farm/config/manifest.json`. (The `mvn install ... -DskipTests` step ensures every module `GenManifest` transitively depends on is present in the local repo with today's changes — mirrors the exact recipe used by the original manifest-parity plan, `meshql/docs/superpowers/plans/2026-07-22-manifest-parity.md` Task 3.)

- [ ] **Step 3: Inspect the regenerated file**

```bash
grep -c '"henId"\|"totalEggs"\|"lastLaidAt"' examples/farm/config/manifest.json
grep -c '"eggs"' examples/farm/config/manifest.json
git diff --stat examples/farm/config/manifest.json
```

Confirm `hen_productivity` appears, `hen`'s `eggs` property is gone, and `lay_report` shows the camelCase fields. Read the full diff, don't just trust the grep counts.

- [ ] **Step 4: Add a stricter conformance assertion — exact entity count**

Edit `examples/farm/src/test/java/com/meshql/examples/farm/ManifestConformanceTest.java`, tightening `everyGraphEntityAppearsInManifestWithCorrectSurfaces`'s final assertion (it already asserts `seen == entities.size()`, which is already an exact-count check driven by however many `.graphql` files exist — no code change is needed there, since it's already schema-count-driven, not hardcoded to 4). Instead, add a new, explicit test making the expected entity set visible in the test file itself (defensive — catches an accidentally-deleted `.graphql` file that the existing count-based test wouldn't catch if two things went missing at once):

```java
    @Test
    void allFiveEntitiesArePresent() throws Exception {
        JsonNode manifest = MAPPER.readTree(MANIFEST_PATH.toFile());
        JsonNode entities = manifest.get("entities");

        assertEquals(
            Set.of("farm", "coop", "hen", "lay_report", "hen_productivity"),
            com.tailoredshapes.underbar.ocho.UnderBar.set(entities.fieldNames()),
            "farm's entity set changed — update this assertion deliberately if that's intended"
        );
    }
```

If `com.tailoredshapes.underbar.ocho.UnderBar.set(Iterator<String>)` isn't the right helper signature, use plain JDK instead:

```java
    @Test
    void allFiveEntitiesArePresent() throws Exception {
        JsonNode manifest = MAPPER.readTree(MANIFEST_PATH.toFile());
        JsonNode entities = manifest.get("entities");

        java.util.Set<String> names = new java.util.HashSet<>();
        entities.fieldNames().forEachRemaining(names::add);

        assertEquals(
            Set.of("farm", "coop", "hen", "lay_report", "hen_productivity"),
            names,
            "farm's entity set changed — update this assertion deliberately if that's intended"
        );
    }
```

Add `import java.util.Set;` if not already present.

- [ ] **Step 5: Run the full manifest test suite**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestGeneratorTest,ManifestConformanceTest,ManifestServingIntegrationTest
```

Expected: `BUILD SUCCESS`, zero failures across all three classes.

- [ ] **Step 6: Commit**

```bash
git add examples/farm/config/manifest.json \
        examples/farm/src/test/java/com/meshql/examples/farm/ManifestConformanceTest.java
git commit -m "chore(farm): regenerate manifest.json for lay_report/hen/hen_productivity changes"
```

---

## Task 13: Update the TS/Cucumber + Vitest BDD suite

**Files:**
- Modify: `examples/farm/test/steps/farm_steps.ts`
- Modify: `examples/farm/test/features/farm.feature`
- Modify: `examples/farm/test/support/world.ts`
- Modify: `examples/farm/test/support/hooks.ts`
- Modify: `examples/farm/test/farm.spec.ts`

These tests run against the real Dockerized Java server (`docker-compose.yml`, Testcontainers) — they're the only coverage in this plan that exercises `Main.java`'s actual wiring (MongoDB storage, real HTTP, real Casbin enforcement) end to end, since Task 11's changes have no JUnit test of their own (no test in this plan boots `Main.java` directly — by design, matching how the rest of `examples/farm` is tested per `meshql/CLAUDE.md`'s "Examples with Docker Compose dependencies" convention).

- [ ] **Step 1: Remove `eggs` from hen creation and GraphQL queries — `farm_steps.ts`**

Edit `examples/farm/test/steps/farm_steps.ts`, in `'I have populated the farm data'`:

```typescript
    // Create hens
    const hens = [
        { name: 'chuck', coop_id: this.coop1_id },
        { name: 'duck', coop_id: this.coop1_id },
        { name: 'euck', coop_id: this.coop2_id },
        { name: 'fuck', coop_id: this.coop2_id },
    ];

    await Promise.all(hens.map((hen) => this.hen_api!.create(undefined, hen)));
```

- [ ] **Step 2: Remove `eggs` from the GraphQL query in `farm.feature`**

Edit `examples/farm/test/features/farm.feature`:

```gherkin
Feature: Farm Integration Test
  As a MeshQL user
  I want to verify the farm example works end-to-end
  So that I can see GraphQL resolvers working across multiple databases

  Background:
    Given the farm service is running in Docker
    And I have created the REST API clients
    And I have populated the farm data

  Scenario: Query farm with nested coops and hens
    When I query the farm graph with:
      """
      {
        getById(id: "${farm_id}") {
          name
          coops {
            name
            hens {
              name
            }
          }
        }
      }
      """
    Then the farm name should be "Emerdale"
    And there should be 3 coops

  Scenario: lay_report can be created but not updated or deleted
    Given I have created a hen
    When I create a lay report for that hen with 2 eggs in the "morning"
    Then the lay report create should succeed
    When I attempt to update that lay report
    Then the lay report update should be forbidden
    When I attempt to delete that lay report
    Then the lay report delete should be forbidden

  Scenario: hen_productivity rejects FE writes
    When I attempt to create a hen_productivity record directly
    Then the hen_productivity create should be forbidden
```

- [ ] **Step 3: Add `lay_report_api` and `hen_productivity_api` to the world**

Edit `examples/farm/test/support/world.ts`:

```typescript
import { World, IWorldOptions } from '@cucumber/cucumber';
import { StartedDockerComposeEnvironment } from 'testcontainers';
import { OpenAPIClient } from 'openapi-client-axios';

export interface FarmWorld extends World {
    environment?: StartedDockerComposeEnvironment;
    hen_api?: OpenAPIClient;
    coop_api?: OpenAPIClient;
    farm_api?: OpenAPIClient;
    lay_report_api?: OpenAPIClient;
    hen_productivity_api?: OpenAPIClient;
    farm_id?: string;
    coop1_id?: string;
    coop2_id?: string;
    hen_id?: string;
    lay_report_id?: string;
    graphqlResult?: any;
    lastResponseStatus?: number;
    tearDown?: () => Promise<void>;
}

export class FarmTestWorld extends World implements FarmWorld {
    environment?: StartedDockerComposeEnvironment;
    hen_api?: OpenAPIClient;
    coop_api?: OpenAPIClient;
    farm_api?: OpenAPIClient;
    lay_report_api?: OpenAPIClient;
    hen_productivity_api?: OpenAPIClient;
    farm_id?: string;
    coop1_id?: string;
    coop2_id?: string;
    hen_id?: string;
    lay_report_id?: string;
    graphqlResult?: any;
    lastResponseStatus?: number;
    tearDown?: () => Promise<void>;

    constructor(options: IWorldOptions) {
        super(options);
    }
}
```

- [ ] **Step 4: Wire up the two new API clients in `hooks.ts`**

Edit `examples/farm/test/support/hooks.ts` — extend `getSwaggerDocs`'s restlette list, `buildApis`'s return type/path-matching, and `Before`'s assignment:

```typescript
let globalApis: {
    hen_api: OpenAPIClient;
    coop_api: OpenAPIClient;
    farm_api: OpenAPIClient;
    lay_report_api: OpenAPIClient;
    hen_productivity_api: OpenAPIClient;
} | null = null;
```

```typescript
async function getSwaggerDocs(): Promise<Document[]> {
    const maxRetries = 10;
    const retryDelay = 2000;

    for (let attempt = 1; attempt <= maxRetries; attempt++) {
        try {
            return await Promise.all(
                ['/hen', '/coop', '/farm', '/lay_report', '/hen_productivity'].map(async (restlette) => {
                    let url = `http://localhost:3033${restlette}/api/api-docs/swagger.json`;
                    const response = await fetch(url);

                    if (!response.ok) {
                        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
                    }

                    let doc = await response.json();
                    return doc;
                }),
            );
        } catch (error) {
            if (attempt === maxRetries) {
                throw error;
            }
            await new Promise(resolve => setTimeout(resolve, retryDelay));
        }
    }

    throw new Error('Failed to fetch swagger docs after retries');
}

async function buildApis(swagger_docs: Document[]): Promise<{
    hen_api: OpenAPIClient;
    coop_api: OpenAPIClient;
    farm_api: OpenAPIClient;
    lay_report_api: OpenAPIClient;
    hen_productivity_api: OpenAPIClient;
}> {
    const authHeaders = { Authorization: `Bearer ` };

    const apis: OpenAPIClient[] = await Promise.all(
        swagger_docs.map(async (doc: Document): Promise<OpenAPIClient> => {
            if (!doc.paths || Object.keys(doc.paths).length === 0) {
                throw new Error(`Swagger document for ${doc.info.title} has no paths defined`);
            }

            const api = new OpenAPIClientAxios({
                definition: doc,
                axiosConfigDefaults: { headers: authHeaders, validateStatus: () => true },
            });

            return api.init();
        }),
    );

    let result: any = {};
    for (const api of apis) {
        const firstPath = Object.keys(api.paths)[0];
        if (firstPath.includes('hen_productivity')) {
            result.hen_productivity_api = api;
        } else if (firstPath.includes('hen')) {
            result.hen_api = api;
        } else if (firstPath.includes('coop')) {
            result.coop_api = api;
        } else if (firstPath.includes('farm')) {
            result.farm_api = api;
        } else if (firstPath.includes('lay_report')) {
            result.lay_report_api = api;
        }
    }

    return result;
}
```

Note the added `validateStatus: () => true` in `axiosConfigDefaults` — the new scenarios assert on `403` responses, which axios treats as thrown errors by default; without this, the forbidden-write assertions would need try/catch instead of a plain status check. (Check whether this changes behavior for any *existing* assertion in this file before committing — it shouldn't, since existing scenarios only ever expect 2xx, but read `buildApis`'s other callers first.)

Edit the `Before` hook:

```typescript
Before(async function(this: FarmWorld) {
    this.environment = globalEnvironment!;
    this.hen_api = globalApis!.hen_api;
    this.coop_api = globalApis!.coop_api;
    this.farm_api = globalApis!.farm_api;
    this.lay_report_api = globalApis!.lay_report_api;
    this.hen_productivity_api = globalApis!.hen_productivity_api;
});
```

- [ ] **Step 5: Add the new step definitions**

Add to `examples/farm/test/steps/farm_steps.ts`:

```typescript
Given('I have created a hen', async function(this: FarmWorld) {
    const farm = await this.farm_api!.create(undefined, { name: 'EventFarm' });
    const farmId = (farm as any).request.path.slice(-36);

    const coop = await this.coop_api!.create(undefined, { name: 'EventCoop', farm_id: farmId });
    const coopId = (coop as any).request.path.slice(-36);

    const hen = await this.hen_api!.create(undefined, { name: 'EventHen', coop_id: coopId });
    this.hen_id = (hen as any).request.path.slice(-36);
});

When('I create a lay report for that hen with {int} eggs in the {string}', async function(this: FarmWorld, eggs: number, timeOfDay: string) {
    const response: any = await this.lay_report_api!.create(undefined, {
        henId: this.hen_id,
        eggs,
        timeOfDay,
    });
    this.lastResponseStatus = response.status;
    this.lay_report_id = response.request?.path?.slice(-36);
});

Then('the lay report create should succeed', function(this: FarmWorld) {
    expect(this.lastResponseStatus).to.equal(201);
});

When('I attempt to update that lay report', async function(this: FarmWorld) {
    const response: any = await this.lay_report_api!.update(
        { id: this.lay_report_id } as any,
        { henId: this.hen_id, eggs: 3, timeOfDay: 'evening' },
    );
    this.lastResponseStatus = response.status;
});

Then('the lay report update should be forbidden', function(this: FarmWorld) {
    expect(this.lastResponseStatus).to.equal(403);
});

When('I attempt to delete that lay report', async function(this: FarmWorld) {
    const response: any = await this.lay_report_api!.remove({ id: this.lay_report_id } as any);
    this.lastResponseStatus = response.status;
});

Then('the lay report delete should be forbidden', function(this: FarmWorld) {
    expect(this.lastResponseStatus).to.equal(403);
});

When('I attempt to create a hen_productivity record directly', async function(this: FarmWorld) {
    const response: any = await this.hen_productivity_api!.create(undefined, {
        henId: this.hen_id ?? '00000000-0000-0000-0000-000000000000',
        totalEggs: 99,
        lastLaidAt: new Date().toISOString(),
    });
    this.lastResponseStatus = response.status;
});

Then('the hen_productivity create should be forbidden', function(this: FarmWorld) {
    expect(this.lastResponseStatus).to.equal(403);
});
```

Check the generated OpenAPI client's exact method names for `update`/`remove` (`openapi-client-axios` derives them from the swagger doc's `operationId`s) against what `hen_api`/`coop_api` already call elsewhere in this file (`this.coop_api!.update(...)`) — reuse the same calling convention rather than guessing a different one.

- [ ] **Step 6: Fix the Vitest smoke test — `farm.spec.ts`**

Edit `examples/farm/test/farm.spec.ts`, removing `eggs` from `buildModels()`'s hen list and from the GraphQL query:

```typescript
    it('should build a server with multiple nodes', async () => {
        const query = `{
            getById(id: "${farm_id}") {
                name 
                coops {
                    name
                    hens {
                        name
                    }
                }
            }
        }`;

        const json = await callSubgraph(new URL(`http://localhost:3033/farm/graph`), query, 'getById', null);

        expect(json.name).toBe('Emerdale');
        expect(json.coops.length).toBe(3);
    });
```

```typescript
async function buildModels() {
    const farm = await farm_api.create(null, { name: 'Emerdale' });

    farm_id = farm.request.path.slice(-36);

    const coop1 = await coop_api.create(null, { name: 'red', farm_id });
    coop1_id = coop1.request.path.slice(-36);

    const coop2 = await coop_api.create(null, { name: 'yellow', farm_id });
    coop2_id = coop2.request.path.slice(-36);

    await coop_api.create(null, { name: 'pink', farm_id });

    await coop_api.update({ id: coop1_id }, { name: 'purple', farm_id });

    const hens = [
        { name: 'chuck', coop_id: coop1_id },
        { name: 'duck', coop_id: coop1_id },
        { name: 'euck', coop_id: coop2_id },
        { name: 'fuck', coop_id: coop2_id },
    ];

    await Promise.all(hens.map((hen) => hen_api.create(null, hen)));
}
```

- [ ] **Step 7: Run the BDD suite (requires Docker; expect ~100s+)**

```bash
cd /tank/repos/tailoredshapes/meshql/examples/farm
npm run test:bdd
```

Expected: all scenarios in `farm.feature` pass, including the two new ones. If Docker isn't available in this environment, skip execution but leave the step here for the next engineer/CI to run — do not claim this step passed without having actually run it (per `superpowers:verification-before-completion`).

- [ ] **Step 8: Run the Vitest smoke test**

```bash
npm test
```

Expected: pass (or skip, per the existing `describe.skipIf(process.env.CI === 'true')` guard, if `CI=true` in this environment).

- [ ] **Step 9: Commit**

```bash
cd /tank/repos/tailoredshapes/meshql
git add examples/farm/test/steps/farm_steps.ts \
        examples/farm/test/features/farm.feature \
        examples/farm/test/support/world.ts \
        examples/farm/test/support/hooks.ts \
        examples/farm/test/farm.spec.ts
git commit -m "test(farm): update BDD suite for lay_report/hen_productivity retrofit; remove legacy eggs field"
```

---

## Task 14: README

**Files:**
- Modify: `examples/farm/README.md`

- [ ] **Step 1: Update the domain model, endpoints table, and quick-start example**

Edit `examples/farm/README.md`:

1. In the intro (line 5), change `4 entities. 8 resolvers. 1 JVM. No boilerplate.` to `5 entities. 1 JVM. No boilerplate.` (drop the stale resolver count — it's not maintained precisely elsewhere in this doc either; don't invent a new precise number, just remove the claim rather than guess).

2. Replace the hierarchy diagram and the "single GraphQL query" example (remove `eggs` from the hen fields, remove `time_of_day`/`eggs` — actually keep `layReports` selection but rename the field):

```
Farm
 └── Coop (farm_id)
      └── Hen (coop_id)
           └── LayReport (henId) — create-only domain event
           └── HenProductivity (henId) — worker-maintained projection, read-only from the FE
```

```graphql
{
  getById(id: "farm-123") {
    name
    coops {
      name
      hens {
        name
        layReports {
          timeOfDay
          eggs
        }
      }
    }
  }
}
```

3. Update the endpoints table to add `hen_productivity`'s two surfaces:

```markdown
| Endpoint | Type | URL |
|:---------|:-----|:----|
| Farms | GraphQL | http://localhost:3033/farm/graph |
| Coops | GraphQL | http://localhost:3033/coop/graph |
| Hens | GraphQL | http://localhost:3033/hen/graph |
| Lay Reports | GraphQL | http://localhost:3033/lay_report/graph |
| Hen Productivity | GraphQL | http://localhost:3033/hen_productivity/graph |
| Farms | REST + Swagger | http://localhost:3033/farm/api |
| Coops | REST + Swagger | http://localhost:3033/coop/api |
| Hens | REST + Swagger | http://localhost:3033/hen/api |
| Lay Reports | REST + Swagger (create-only) | http://localhost:3033/lay_report/api |
| Hen Productivity | REST + Swagger (worker-only writes) | http://localhost:3033/hen_productivity/api |
| Health | HTTP | http://localhost:3033/ready |
| Manifest | HTTP | http://localhost:3033/manifest |
```

4. In "Try It", remove `eggs` from the hen creation payload and add a lay_report example:

```bash
# Create a hen (no eggs field — hen_productivity is the source of truth for egg counts)
curl -s -X POST http://localhost:3033/hen/api \
  -H "Content-Type: application/json" \
  -d "{\"name\": \"Henrietta\", \"coop_id\": \"$COOP_ID\"}"

# Submit a lay report (create-only — this is a domain event, not an editable record)
curl -s -X POST http://localhost:3033/lay_report/api \
  -H "Content-Type: application/json" \
  -d "{\"henId\": \"$HEN_ID\", \"eggs\": 2, \"timeOfDay\": \"morning\"}"

# Attempting to update or delete a lay report is rejected (403) — corrections
# are new lay_report events, not edits to old ones.
```

5. Update the "Domain Model" table:

```markdown
| Entity | Fields | Relationships |
|:-------|:-------|:--------------|
| **Farm** | `name` | has many Coops |
| **Coop** | `name`, `farm_id` | belongs to Farm, has many Hens |
| **Hen** | `name`, `dob`, `coop_id` | belongs to Coop, has many LayReports |
| **LayReport** | `henId`, `eggs` (0-3), `timeOfDay` (morning/afternoon/evening) | belongs to Hen — **create-only**, immutable domain event |
| **HenProductivity** | `henId`, `totalEggs`, `lastLaidAt` | 1:1 projection over Hen — **worker-only writes**, read-only from the FE |
```

6. Add a new section after "Federation Map" explaining the event-sourced shape:

```markdown
## Event Sourcing: `lay_report` and `hen_productivity`

`lay_report` and `hen_productivity` aren't plain CRUD entities, unlike the other three:

- **`lay_report` is create-only.** POSTing `{henId, eggs, timeOfDay}` records a domain event ("this hen laid N eggs at this time"). `PUT`/`DELETE` are rejected with `403` — once recorded, a lay report is never edited or removed. A correction is a new `lay_report`, not a change to an old one.
- **`hen_productivity` is worker-only.** Nothing the FE does writes here directly. It's a running-total projection (`{henId, totalEggs, lastLaidAt}`) maintained by folding `lay_report` events, written via the same ordinary restlette every other entity uses — the only thing unusual about it is *who* calls it (a backend worker authenticated as the `worker` Casbin role, not a browser). See the companion spec (`2026-07-22-merkql-worker-pipeline-design.md` in `meshql-rs`) for the CDC bridge that keeps it in sync; that worker isn't part of this example.

Both are enforced by attaching a distinct `CasbinAuth` instance (its own model + policy file under `config/casbin/`) to each restlette, rather than sharing one `Auth` across the whole server the way `farm`/`coop`/`hen` still do. See `Main.java`'s `layReportAuth`/`henProductivityAuth` construction.
```

7. Update the performance-section index snippet (`payload.hen_id` → `payload.henId`):

```javascript
db['farm-development-lay_report'].createIndex({'payload.henId': 1});
db['farm-development-hen'].createIndex({'payload.coop_id': 1});
db['farm-development-coop'].createIndex({'payload.farm_id': 1});
```

- [ ] **Step 2: Proofread the whole file for any remaining `eggs`/`hen_id`/`time_of_day` references you might have missed**

```bash
grep -n "eggs\|hen_id\|time_of_day" examples/farm/README.md
```

Every remaining `eggs` hit should be inside a `lay_report`/`hen_productivity` context (`totalEggs`, `eggs` on LayReport itself — which is correct, `lay_report.eggs` is not being removed, only `hen.eggs`). Anything else is a miss — go back and fix it.

- [ ] **Step 3: Commit**

```bash
git add examples/farm/README.md
git commit -m "docs(farm): describe the event-sourced lay_report/hen_productivity shape"
```

---

## Task 15: Full workspace verification and final state

**Files:** none (verification only)

- [ ] **Step 1: Run the full Java test suite for every module this plan touched**

```bash
cd /tank/repos/tailoredshapes/meshql
mvn test -pl core,auth/noop,auth/casbin,auth/jwt,api/restlette,server,examples/farm -am
```

Expected: `BUILD SUCCESS`, zero failures, zero errors, across every module.

- [ ] **Step 2: Run the complete workspace build once, to catch any module this plan's changes touched indirectly (e.g. another example depending on `core`'s `RestletteConfig` shape) that the module-scoped runs above wouldn't catch**

```bash
mvn clean install -DskipTests
```

Expected: `BUILD SUCCESS`. This confirms the `RestletteConfig` record's new 6th field (Task 3) didn't silently break another example's positional-constructor usage anywhere else in the monorepo — if it did, `-DskipTests` still fails at compile, which is what you're checking for here (a full `mvn test` across every module in the reactor is out of scope for this plan and would run unrelated example suites needing infrastructure this plan doesn't set up — compile-only is the right bar).

- [ ] **Step 3: Confirm the manifest is genuinely in sync one more time (belt-and-suspenders, since Task 12 already did this)**

```bash
mvn test -pl examples/farm -am -Dtest=ManifestConformanceTest
```

Expected: `BUILD SUCCESS`.

- [ ] **Step 4: Review the full diff against `main` before considering this done**

```bash
git status
git diff main --stat
```

Confirm the file list matches what this plan touched — no stray edits, no leftover scratch files (e.g. `/tmp/CasbinPolicyCheck.*` from Task 10, Step 4, which was already cleaned up inline but double-check).

- [ ] **Step 5: Final state — hand back to the user for push**

This environment has no push credentials configured for the AI agent. All commits from Tasks 1-14 are local, on the worktree's branch. **Do not attempt `git push`.** Report the branch name and commit count to the user and ask them to push it themselves:

```bash
git log --oneline main..HEAD
git branch --show-current
```

Include this exact output in your final report to the user.
