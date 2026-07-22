# Farm Event-Sourcing Retrofit (TypeScript) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **This is the TS leg of a three-language retrofit.** Companion plans (same directory): `2026-07-22-farm-retrofit-rust.md` and `2026-07-22-farm-retrofit-java.md`. The three are independent — this plan only touches `/tank/repos/tailoredshapes/meshobj` (the TS `meshobj` repo), not `meshql-rs` or the Java `meshql` repo.
>
> **Worktree required.** Per `superpowers:using-git-worktrees` / `superpowers:subagent-driven-development`, do this work in a dedicated git worktree off `meshobj`'s default branch — do not implement directly on `main` in the primary checkout. Create it before Task 1, e.g. `git -C /tank/repos/tailoredshapes/meshobj worktree add ../meshobj-farm-retrofit-ts -b farm-retrofit-ts`, then run every command below from that worktree path instead of `/tank/repos/tailoredshapes/meshobj`.
>
> **No push credentials.** This environment has no push credentials configured for the AI agent. Every task below ends in a **local** commit only. Do not attempt `git push`. The plan's final step (end of Task 13) explicitly hands off to the user to review the worktree and push manually.

**Goal:** Retrofit `examples/farm` (TypeScript) from plain CRUD to an event-sourced shape — `lay_report` becomes a create-only domain event with a standardized `{henId, eggs, timeOfDay}` payload, a new `hen_productivity` read-model entity is added as an ordinary restlette+graphlette pair, `hen`'s stale `eggs` field is removed, and write authorization gets real per-entity, per-verb enforcement via Casbin — closing two gaps confirmed directly against source: `create()` in `core/restlette/src/crud.ts` calls no auth method at all today, and TS's `Auth` interface has no verb concept for any of `create`/`update`/`remove` to vary.

**Architecture:** A new `authorizeAction(credentials, action, envelope)` method is added to the `Auth` interface (`core/auth`) — implemented as a same-behavior passthrough in `NoOp` and `JWTSubAuthorizer`, and as a real Casbin `enforcer.enforce(subject, resource, action)` RBAC check in `CasbinAuth` (`core/casbin_auth`), which gains a `resource` field and a cheap `withResource()` clone so one loaded policy file can back five differently-scoped `Auth` instances (one per restlette). `Crud.create/update/remove` (`core/restlette/src/crud.ts`) call `authorizeAction` with `'create'|'update'|'delete'`. `meshql-server`'s `init()` gains an optional `restletteAuthOverrides: Record<string, Auth>` param, keyed by restlette path, so `examples/farm/index.ts` can hand each restlette its own resource-scoped `CasbinAuth` instance without a full `ServerConfig` redesign. `examples/farm` gets a new `hen_productivity` entity (JSON schema + GraphQL schema + Mongo storage + graphlette + restlette) and a Casbin `model.conf`/`policy.csv` pair enforcing: `worker` role → create+update `hen_productivity`; everyone else → create `farm`/`coop`/`hen`/`lay_report`, update/delete `farm`/`coop`/`hen` only (not `lay_report`, not `hen_productivity`).

**Tech Stack:** TypeScript, Express, Vitest, `casbin` (RBAC engine, already a `core/casbin_auth` dependency), `jsonwebtoken` (test-only JWT construction — decode-only in production, per this repo's documented gateway-auth model), Zod (config schemas), Ajv (JSON Schema validation / manifest conformance), `@tailoredshapes/meshql-sqlite_repo` (fast in-process test storage, no Docker).

---

### Task 1: `Auth` interface gains a verb-aware `authorizeAction` method

**Context:** Confirmed directly against `core/auth/src/index.ts`: `Auth` today is `{ getAuthToken, isAuthorized }` — `isAuthorized(credentials, envelope)` is envelope-token ABAC with no verb parameter. This task adds the new method and its `Action` type, and implements it on `NoOp` (the only `Auth` implementer that lives in this package). This must land first — every other `Auth` implementer and every literal object typed `Auth` in the codebase needs this method to typecheck once it's required.

**Files:**
- Modify: `core/auth/src/index.ts`
- Create: `core/auth/test/index.spec.ts`
- Create: `core/auth/vitest.config.ts` (missing today — every sibling core package has one; needed so this package's tests show up in the root `vitest.workspace.ts` glob `./core/*/vitest.config.ts`)

- [ ] **Step 1: Write the failing test**

  Create `core/auth/test/index.spec.ts`:
  ```typescript
  import { describe, it, expect } from 'vitest';
  import { NoOp } from '../src';
  import { Envelope } from '@tailoredshapes/meshql-common';

  describe('NoOp', () => {
      it('authorizes every action for every credential', async () => {
          const auth = new NoOp();
          const envelope: Envelope = { payload: {} };

          expect(await auth.authorizeAction([], 'create', envelope)).toBe(true);
          expect(await auth.authorizeAction(['anyone'], 'update', envelope)).toBe(true);
          expect(await auth.authorizeAction(['anyone'], 'delete', envelope)).toBe(true);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd core/auth && yarn vitest run test/index.spec.ts
  ```
  Expected: fails to compile / fails at runtime — `auth.authorizeAction is not a function` (method doesn't exist yet).

- [ ] **Step 3: Write minimal implementation**

  Replace `core/auth/src/index.ts` in full:
  ```typescript
  import { Envelope } from '@tailoredshapes/meshql-common';

  export type Action = 'create' | 'update' | 'delete';

  export interface Auth {
      getAuthToken(context: Record<string, any>): Promise<string[]>;
      isAuthorized(credentials: string[], data: Envelope): Promise<boolean>;
      /**
       * Verb-aware authorization check. Distinct from isAuthorized (envelope-
       * token ABAC, no verb concept): this lets an Auth implementation grant
       * or deny a specific create/update/delete independent of any per-record
       * authorized_tokens. NoOp and JWTSubAuthorizer have no policy engine of
       * their own; CasbinAuth is the implementation that actually varies its
       * answer by resource+action (see core/casbin_auth).
       */
      authorizeAction(credentials: string[], action: Action, data: Envelope): Promise<boolean>;
  }

  export class NoOp implements Auth {
      async getAuthToken(): Promise<string[]> {
          return ['TOKEN'];
      }

      async isAuthorized(): Promise<boolean> {
          return true;
      }

      async authorizeAction(): Promise<boolean> {
          return true;
      }
  }

  export type ReadSecurer<T> = (creds: any, query: any) => Promise<T>;
  ```

  Create `core/auth/vitest.config.ts` (mirrors `core/casbin_auth/vitest.config.ts`):
  ```typescript
  import { defineConfig } from 'vitest/config';

  export default defineConfig({
      test: {
          globals: true,
          environment: 'node',
          coverage: {
              provider: 'v8',
              include: ['src/**/*.ts'],
              exclude: ['**/*.d.ts', '**/*.js', '**/test/**', '**/coverage/**'],
              all: false,
          },
      },
  });
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd core/auth && yarn vitest run test/index.spec.ts
  ```
  Expected: `1 passed`.

- [ ] **Step 5: Commit**

  ```bash
  git add core/auth/src/index.ts core/auth/test/index.spec.ts core/auth/vitest.config.ts
  git commit -m "$(cat <<'EOF'
  feat(auth): add verb-aware authorizeAction to the Auth interface

  Auth.create() calls no auth method today and Auth has no verb concept
  for any of create/update/remove to vary. This is step 1 of wiring real
  per-entity, per-verb write authorization for the farm retrofit.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 2: `JWTSubAuthorizer.authorizeAction` — passthrough to existing ABAC

**Context:** `JWTSubAuthorizer` (`core/jwt_auth/src/index.ts`) is the default `Auth` used when `config.casbinParams` is absent (see `core/server/src/server.ts`'s `processAuth`). It must implement the new interface method to keep compiling. Delegating to its own `isAuthorized` (same envelope-token ABAC, verb ignored) is the correct default: it keeps every non-Casbin deployment's newly-wired `create()` call a no-op change in practice, since farm's envelopes never set `authorized_tokens` today.

**Files:**
- Modify: `core/jwt_auth/src/index.ts`
- Modify: `core/jwt_auth/test/index.spec.ts`

- [ ] **Step 1: Write the failing test**

  Add to `core/jwt_auth/test/index.spec.ts` (new `describe` block, after the existing `isAuthorized` block):
  ```typescript
  describe('authorizeAction', () => {
      it('delegates to isAuthorized, ignoring the verb', async () => {
          const envelope: Envelope = { payload: {}, authorized_tokens: ['user1'] };

          expect(await authorizer.authorizeAction(['user1'], 'create', envelope)).toBe(true);
          expect(await authorizer.authorizeAction(['user1'], 'update', envelope)).toBe(true);
          expect(await authorizer.authorizeAction(['user1'], 'delete', envelope)).toBe(true);
          expect(await authorizer.authorizeAction(['someone-else'], 'create', envelope)).toBe(false);
      });

      it('authorizes everyone when authorized_tokens is unset, for every verb', async () => {
          const envelope: Envelope = { payload: {} };
          expect(await authorizer.authorizeAction([], 'create', envelope)).toBe(true);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd core/jwt_auth && yarn vitest run test/index.spec.ts
  ```
  Expected: `authorizer.authorizeAction is not a function`.

- [ ] **Step 3: Write minimal implementation**

  In `core/jwt_auth/src/index.ts`, update the import and add the method:
  ```typescript
  import { Action, Auth } from '@tailoredshapes/meshql-auth';
  ```
  ```typescript
      async isAuthorized(credentials: string[], data: Envelope): Promise<boolean> {
          const authorizedTokens = data.authorized_tokens;

          // Allow access if authorized_tokens is empty or undefined
          if (!authorizedTokens || authorizedTokens.length === 0) {
              return true;
          }

          // Check if any of the credentials match authorized tokens
          return authorizedTokens.some((token) => credentials.includes(token));
      }

      async authorizeAction(credentials: string[], _action: Action, data: Envelope): Promise<boolean> {
          // JWTSubAuthorizer has no policy engine of its own — reuse the same
          // envelope-token ABAC check as isAuthorized, ignoring the verb.
          return this.isAuthorized(credentials, data);
      }
  }
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd core/jwt_auth && yarn vitest run test/index.spec.ts
  ```
  Expected: `6 passed` (4 existing + 2 new).

- [ ] **Step 5: Commit**

  ```bash
  git add core/jwt_auth/src/index.ts core/jwt_auth/test/index.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(jwt_auth): implement authorizeAction as a passthrough to isAuthorized

  Keeps default (non-Casbin) deployments' behavior unchanged now that
  create/update/remove all call authorizeAction.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 3: `CasbinAuth.authorizeAction` — real RBAC enforcement, plus `withResource`

**Context:** Confirmed directly: `CasbinAuth.isAuthorized` (`core/casbin_auth/src/index.ts`) is byte-for-byte the same envelope-token ABAC check as `JWTSubAuthorizer.isAuthorized` — `enforcer.enforce()` is never called anywhere in the current codebase. Casbin's engine (loaded model.conf/policy.csv, `getRolesForUser`) exists but nothing consults it for a decision. This task makes `authorizeAction` the first real caller of `enforcer.enforce(subject, resource, action)`. `resource` is now a per-instance field (default `''`, backward compatible) so one `CasbinAuth` built from one policy file can be cloned via `withResource()` into five differently-scoped instances — this is the "separate Auth instance per restlette, not shared" mechanism the spec calls for, without needing a redesign of `authorize_action`'s signature or five separate policy files.

Also fixes a latent bug while touching this code: `getAuthToken` called `enforcer.getRolesForUser(sub[0])` unconditionally, including when `sub` is `[]` (no bearer token) — `sub[0]` is `undefined` in that case. Guard it to return `[]` directly.

**Files:**
- Modify: `core/casbin_auth/src/index.ts`
- Modify: `core/casbin_auth/test/casbin.spec.ts`

- [ ] **Step 1: Write the failing test**

  Add to `core/casbin_auth/test/casbin.spec.ts`. First, add `authorizeAction: vi.fn()` to the existing `mockAuth` object (it's typed `vi.Mocked<Auth>` and will now be missing a required member):
  ```typescript
      mockAuth = {
          getAuthToken: vi.fn(),
          isAuthorized: vi.fn(),
          authorizeAction: vi.fn(),
      } as vi.Mocked<Auth>;
  ```
  Then add a new `describe` block at the end of the file, before the final closing `});`:
  ```typescript
      describe('authorizeAction', () => {
          it('grants when enforcer.enforce resolves true for a credential', async () => {
              mockEnforcer.enforce = vi.fn().mockResolvedValue(true);
              const casbinAuth = await CasbinAuth.create(['model.conf', 'policy.csv'], mockAuth, 'lay_report');

              const result = await casbinAuth.authorizeAction(['anonymous'], 'create', { payload: {} });

              expect(result).toBe(true);
              expect(mockEnforcer.enforce).toHaveBeenCalledWith('anonymous', 'lay_report', 'create');
          });

          it('denies when enforcer.enforce resolves false for every credential', async () => {
              mockEnforcer.enforce = vi.fn().mockResolvedValue(false);
              const casbinAuth = await CasbinAuth.create(['model.conf', 'policy.csv'], mockAuth, 'lay_report');

              const result = await casbinAuth.authorizeAction(['anonymous'], 'update', { payload: {} });

              expect(result).toBe(false);
          });

          it('falls back to the literal subject "anonymous" when credentials is empty', async () => {
              mockEnforcer.enforce = vi.fn().mockResolvedValue(true);
              const casbinAuth = await CasbinAuth.create(['model.conf', 'policy.csv'], mockAuth, 'farm');

              await casbinAuth.authorizeAction([], 'create', { payload: {} });

              expect(mockEnforcer.enforce).toHaveBeenCalledWith('anonymous', 'farm', 'create');
          });
      });

      describe('withResource', () => {
          it('clones with a different resource but the same enforcer', async () => {
              const casbinAuth = await CasbinAuth.create(['model.conf', 'policy.csv'], mockAuth, 'farm');
              const scoped = casbinAuth.withResource('hen_productivity');

              expect(scoped.enforcer).toBe(casbinAuth.enforcer);
              expect(scoped.resource).toBe('hen_productivity');
              expect(casbinAuth.resource).toBe('farm');
          });
      });

      describe('getAuthToken with no bearer token', () => {
          it('returns [] without calling getRolesForUser(undefined)', async () => {
              mockAuth.getAuthToken.mockResolvedValueOnce([]);
              const casbinAuth = await CasbinAuth.create(['model.conf', 'policy.csv'], mockAuth);

              const roles = await casbinAuth.getAuthToken({});

              expect(roles).toEqual([]);
              expect(mockEnforcer.getRolesForUser).not.toHaveBeenCalled();
          });
      });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd core/casbin_auth && yarn vitest run test/casbin.spec.ts
  ```
  Expected: fails — `casbinAuth.authorizeAction is not a function`, `casbinAuth.withResource is not a function`, and `CasbinAuth.create(...)`'s third argument doesn't exist yet.

- [ ] **Step 3: Write minimal implementation**

  Replace `core/casbin_auth/src/index.ts` in full:
  ```typescript
  import { Enforcer, newEnforcer } from 'casbin';
  import { Action, Auth } from '@tailoredshapes/meshql-auth';
  import { Envelope } from '@tailoredshapes/meshql-common';

  export class CasbinAuth implements Auth {
      enforcer: Enforcer;
      jwtAuth: Auth;
      resource: string;

      // Private constructor to prevent direct instantiation
      private constructor(enforcer: Enforcer, jwtAuth: Auth, resource: string = '') {
          this.enforcer = enforcer;
          this.jwtAuth = jwtAuth;
          this.resource = resource;
      }

      static async create(params: any[], auth: Auth, resource: string = ''): Promise<CasbinAuth> {
          const enforcer = await newEnforcer(...params);
          return new CasbinAuth(enforcer, auth, resource);
      }

      /**
       * Returns a new CasbinAuth sharing this instance's enforcer/policy but
       * scoped to a different Casbin object ("resource"). Lets one loaded
       * model.conf/policy.csv back several per-restlette Auth instances
       * without re-parsing the policy files for each one.
       */
      withResource(resource: string): CasbinAuth {
          return new CasbinAuth(this.enforcer, this.jwtAuth, resource);
      }

      async getAuthToken(context: any): Promise<any> {
          const sub = await this.jwtAuth.getAuthToken(context);
          if (sub.length === 0) {
              return [];
          }
          return await this.enforcer.getRolesForUser(sub[0]);
      }

      async isAuthorized(credentials: string[], data: Envelope): Promise<boolean> {
          const authorizedTokens = data.authorized_tokens;

          // Allow access if authorized_tokens is empty or undefined
          if (!authorizedTokens || authorizedTokens.length === 0) {
              return true;
          }

          // Check if any of the credentials match authorized tokens
          return authorizedTokens.some((token) => credentials.includes(token));
      }

      /**
       * Verb-aware RBAC check: does any of `credentials` (subjects/roles, as
       * returned by getAuthToken) have permission to perform `action` on this
       * instance's `resource`? Backed by a real casbin enforcer.enforce()
       * call, unlike isAuthorized's envelope-token ABAC. Credentials with no
       * resolved role fall back to the literal "anonymous" subject, so
       * policy.csv can grant/deny unauthenticated callers explicitly.
       */
      async authorizeAction(credentials: string[], action: Action, _data: Envelope): Promise<boolean> {
          const subjects = credentials.length > 0 ? credentials : ['anonymous'];

          for (const subject of subjects) {
              if (await this.enforcer.enforce(subject, this.resource, action)) {
                  return true;
              }
          }

          return false;
      }
  }
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd core/casbin_auth && yarn vitest run test/casbin.spec.ts
  ```
  Expected: all tests pass (5 original + 6 new = 11 passed).

- [ ] **Step 5: Commit**

  ```bash
  git add core/casbin_auth/src/index.ts core/casbin_auth/test/casbin.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(casbin_auth): implement authorizeAction with real enforcer.enforce

  CasbinAuth.isAuthorized never actually consulted the loaded policy —
  it duplicated JWTSubAuthorizer's envelope-token ABAC check verbatim.
  authorizeAction is the first real RBAC decision point, and withResource
  lets one loaded policy back several per-restlette scoped instances.
  Also fixes getAuthToken calling getRolesForUser(undefined) when no
  bearer token is present.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 4: Wire `authorizeAction` into `Crud.create`/`update`/`remove`

**Context:** Confirmed directly against `core/restlette/src/crud.ts`: `create()` calls no auth method at all — it builds the envelope and writes straight to the repository. `update()`/`remove()` call only `isAuthorized(tokens, envelope)`. This task adds `authorizeAction` calls to all three, composing with the existing `isAuthorized` check for `update`/`remove` (both must pass) — `create` has no prior envelope to ABAC-check against, so it's governed by `authorizeAction` alone. This is net-new authorization coverage for `create`, for every restlette in every TS deployment, not just `examples/farm`.

**Files:**
- Modify: `core/restlette/src/crud.ts`
- Modify: `core/restlette/test/crud.spec.ts`

- [ ] **Step 1: Write the failing test**

  In `core/restlette/test/crud.spec.ts`, first fix the existing `authorization tests` block's inline `Auth` literal (it's a plain object typed `Auth` and will fail to compile without the new method):
  ```typescript
          const auth: Auth = {
              async getAuthToken(context: Record<string, any>): Promise<string[]> {
                  return [context.headers?.authorization ?? 'fd'];
              },
              async isAuthorized(credentials: string[], data: Record<string, any>): Promise<boolean> {
                  return credentials[0] === 'token';
              },
              async authorizeAction(): Promise<boolean> {
                  return true;
              },
          };
  ```
  Then add a new top-level `describe` block, after `authorization tests`, before the closing `});` of the file:
  ```typescript
  describe('verb-aware authorization (authorizeAction)', function () {
      let app: Application;
      let server: any;

      const port = 40500;

      afterAll(() => {
          server.close();
      });

      beforeAll(async () => {
          // Denies 'update' specifically, allows every other verb — proves
          // create/update/remove each pass their own distinct action string.
          const auth: Auth = {
              async getAuthToken(): Promise<string[]> {
                  return ['someone'];
              },
              async isAuthorized(): Promise<boolean> {
                  return true;
              },
              async authorizeAction(_credentials: string[], action: string): Promise<boolean> {
                  return action !== 'update';
              },
          };

          const repo: Repository = new InMemory();
          await repo.create({ id: 'block-update', payload: { name: 'chuck', eggs: 6 } });

          app = express();
          app.use(express.json());

          const context = '/hens';
          const validator: Validator = async () => true;

          const crud: Crud = new Crud(auth, repo, validator, context);
          init(app, crud, context, port, henSchema);

          server = app.listen(port);
      });

      it('rejects create when authorizeAction denies the "create" verb', async () => {
          const response = await fetch(`http://localhost:${port}/hens`, {
              method: 'POST',
              body: JSON.stringify({ name: 'newHen' }),
              headers: { 'Content-Type': 'application/json' },
          });

          // this auth only denies 'update', so create should succeed here
          expect(response.status).toBe(200);
      });

      it('rejects update when authorizeAction denies the "update" verb', async () => {
          const response = await fetch(`http://localhost:${port}/hens/block-update`, {
              method: 'PUT',
              body: JSON.stringify({ name: 'chuck', eggs: 9 }),
              headers: { 'Content-Type': 'application/json' },
          });

          expect(response.status).toBe(403);
      });

      it('allows remove when authorizeAction allows the "delete" verb', async () => {
          const response = await fetch(`http://localhost:${port}/hens/block-update`, {
              method: 'DELETE',
          });

          expect(response.status).toBe(200);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd core/restlette && yarn vitest run test/crud.spec.ts
  ```
  Expected: compile failure on the `authorization tests` block (missing `authorizeAction`) and/or the new block's `update` test getting `200`/`303` instead of `403` because `create`/`update`/`remove` don't call `authorizeAction` yet.

- [ ] **Step 3: Write minimal implementation**

  In `core/restlette/src/crud.ts`, update the import:
  ```typescript
  import { Action, Auth } from '@tailoredshapes/meshql-auth';
  ```
  Replace `create`:
  ```typescript
      create = async (req: Request, res: Response) => {
          const authToken: string[] = await this._authorizer.getAuthToken(req);
          const authorized_tokens: string[] = await this.calculateTokens(req);
          const payload: Record<string, any> = req.body;

          if (!(await this._validator(payload))) {
              res.status(400).send('Invalid document');
              return;
          }

          const doc: Envelope = { payload, authorized_tokens };

          if (!(await this._authorizer.authorizeAction(authToken, 'create', doc))) {
              res.status(403).send({});
              return;
          }

          const result: Envelope = await this._repo.create(doc);

          if (result) {
              logger.debug(`Created: ${JSON.stringify(result)}`);
              this.setHonestyHeaders(res, result);
              res.status(303).location(`${this._context}/${result.id}`).send();
          } else {
              logger.error(`Failed to create: ${JSON.stringify(doc)}`);
              res.status(400).send();
          }
      };
  ```
  Replace the body of `update`'s authorization check:
  ```typescript
          const current = await this._repo.read(id);

          if (current) {
              const abacAllowed = await this._authorizer.isAuthorized(authToken, current);
              const verbAllowed = await this._authorizer.authorizeAction(authToken, 'update', current);

              if (abacAllowed && verbAllowed) {
                  const result = await this._repo.create(envelope);
                  logger.debug(`Updated: ${JSON.stringify(result)}`);
                  this.setHonestyHeaders(res, result);
                  res.status(303).location(`${this._context}/${result.id}`).send();
              } else {
                  res.status(403).send({});
              }
          } else {
              res.status(404).send({});
          }
      };
  ```
  Replace the body of `remove`'s authorization check:
  ```typescript
          if (result) {
              const abacAllowed = await this._authorizer.isAuthorized(tokens, result);
              const verbAllowed = await this._authorizer.authorizeAction(tokens, 'delete', result);

              if (abacAllowed && verbAllowed) {
                  const success = await this._repo.remove(id);
                  if (success) {
                      logger.debug(`Deleted: ${id}`);
                      res.send({ deleted: id });
                  } else {
                      res.status(404).send({});
                  }
              } else {
                  res.status(403).send({});
              }
          } else {
              res.status(404).send({});
          }
      };
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd core/restlette && yarn vitest run test/crud.spec.ts
  ```
  Expected: all tests pass. Then check for fallout in `core/graphlette`, which has two `Mock<Auth>` (moq.ts) instances that may or may not need an explicit `authorizeAction` setup:
  ```bash
  cd core/graphlette && yarn test
  ```
  If this fails with a missing-method error on the `Auth` mock, add to both `core/graphlette/test/root.spec.ts` and `core/graphlette/test/dataLoader.spec.ts`, chained onto the existing `Mock<Auth>` setup:
  ```typescript
      .setup(async (i) => i.authorizeAction(It.IsAny(), It.IsAny(), It.IsAny()))
      .returnsAsync(true)
  ```

- [ ] **Step 5: Commit**

  ```bash
  git add core/restlette/src/crud.ts core/restlette/test/crud.spec.ts
  git add core/graphlette/test/root.spec.ts core/graphlette/test/dataLoader.spec.ts 2>/dev/null
  git commit -m "$(cat <<'EOF'
  feat(restlette): wire authorizeAction into create/update/remove

  create() previously called no auth method at all; update()/remove()
  called only the envelope-token ABAC check (isAuthorized). All three
  now also consult authorizeAction with their own verb string
  (create/update/delete), composing with isAuthorized where an existing
  envelope exists to ABAC-check.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 5: `meshql-server` `init()` — optional per-restlette `Auth` override

**Context:** Confirmed directly against `core/server/src/server.ts`: `init()` builds exactly one `Auth` via `processAuth(config)` and passes that same instance to every graphlette and every restlette in the `for` loops. There is no way for one `ServerConfig` to give `lay_report`'s restlette a different `Auth` than `farm`'s. This task adds an optional 4th... actually 3rd parameter, `restletteAuthOverrides`, keyed by restlette `path`, defaulting to `{}` so every existing caller (`examples/events`, `examples/farm`'s current 2-arg call, `core/server/test/health.spec.ts`) is unaffected.

**Files:**
- Modify: `core/server/src/server.ts`
- Test: `core/server/test/restlette-auth-override.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `core/server/test/restlette-auth-override.spec.ts`:
  ```typescript
  import { describe, it, expect, beforeAll, afterAll } from 'vitest';
  import { init } from '../src/server';
  import { Config, Restlette } from '../src/configTypes';
  import { SQLitePlugin, SQLConfig } from '@tailoredshapes/meshql-sqlite_repo';
  import { Auth } from '@tailoredshapes/meshql-auth';

  const allowAll: Auth = {
      async getAuthToken() {
          return ['anyone'];
      },
      async isAuthorized() {
          return true;
      },
      async authorizeAction() {
          return true;
      },
  };

  const denyAll: Auth = {
      async getAuthToken() {
          return ['anyone'];
      },
      async isAuthorized() {
          return true;
      },
      async authorizeAction() {
          return false;
      },
  };

  describe('per-restlette Auth override', () => {
      let app: any;
      let server: any;
      const port = 40510;

      const schema = { type: 'object', additionalProperties: false, required: ['name'], properties: { id: { type: 'string' }, name: { type: 'string' } } };

      const storage = (collection: string): SQLConfig => ({ type: 'sqlite', uri: ':memory:', collection });

      beforeAll(async () => {
          const open: Restlette = { path: '/open/api', storage: storage('open'), schema };
          const locked: Restlette = { path: '/locked/api', storage: storage('locked'), schema };

          const config: Config = { port, graphlettes: [], restlettes: [open, locked] };

          app = await init(config, { sqlite: new SQLitePlugin() }, { '/locked/api': denyAll });
          server = app.listen(port);
      });

      afterAll(() => {
          server.close();
      });

      it('uses the default auth for restlettes with no override', async () => {
          const response = await fetch(`http://localhost:${port}/open/api`, {
              method: 'POST',
              body: JSON.stringify({ name: 'ok' }),
              headers: { 'Content-Type': 'application/json' },
          });

          expect(response.status).toBe(200);
      });

      it('uses the overridden auth for the restlette named in restletteAuthOverrides', async () => {
          const response = await fetch(`http://localhost:${port}/locked/api`, {
              method: 'POST',
              body: JSON.stringify({ name: 'blocked' }),
              headers: { 'Content-Type': 'application/json' },
          });

          expect(response.status).toBe(403);
      });
  });
  ```

  Add `@tailoredshapes/meshql-sqlite_repo` as a devDependency of `core/server/package.json` if not already present (check first — `core/server/test/health.spec.ts` already imports it, so it likely already is).

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd core/server && yarn vitest run test/restlette-auth-override.spec.ts
  ```
  Expected: fails — `init(config, plugins, overrides)` rejects the 3rd argument (too many arguments) or, if TS allows the extra arg to be silently ignored at the JS level, the `/locked/api` test gets `200` instead of `403`.

- [ ] **Step 3: Write minimal implementation**

  In `core/server/src/server.ts`, change the `init` signature and restlette loop:
  ```typescript
  export async function init(
      config: Config,
      plugins: Record<string, Plugin>,
      restletteAuthOverrides: Record<string, Auth> = {},
  ): Promise<Application> {
      const auth: Auth = await processAuth(config);

      const app: Application = express();
      app.use(express.json());

      // Use CORS middleware
      app.use(
          cors({
              origin: '*', // Allow all origins. Adjust as needed for security.
              methods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS'],
              allowedHeaders: ['Content-Type', 'Authorization'],
          }),
      );

      // Add health check endpoint
      app.get('/health', async (req, res) => {
          const status = await checkAllServicesHealth(config);
          res.json(status);
      });

      // Add ready check endpoint
      app.get('/ready', async (req, res) => {
          const status = await checkAllServicesReady(config);
          res.status(status.status === 'ok' ? 200 : 503).json(status);
      });

      // Process graphlettes
      for (const graphlette of config.graphlettes) {
          await processGraphlette(graphlette, auth, app, plugins);
      }

      // Process restlettes
      for (const restlette of config.restlettes) {
          const restletteAuth = restletteAuthOverrides[restlette.path] ?? auth;
          await processRestlette(restlette, restletteAuth, app, config.port, plugins);
      }

      return app;
  }
  ```
  No other function in the file changes — `processRestlette` already takes `auth` as a parameter per-call, it just always received the same value before.

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd core/server && yarn vitest run test/restlette-auth-override.spec.ts
  ```
  Expected: `2 passed`. Then run the full package suite to confirm no regression:
  ```bash
  cd core/server && yarn test
  ```

- [ ] **Step 5: Commit**

  ```bash
  git add core/server/src/server.ts core/server/test/restlette-auth-override.spec.ts core/server/package.json
  git commit -m "$(cat <<'EOF'
  feat(server): allow a distinct Auth per restlette via restletteAuthOverrides

  init() previously passed one shared Auth to every graphlette and
  restlette in a ServerConfig, with no way for a caller to give one
  restlette (e.g. hen_productivity) a different policy than another
  (e.g. farm). Backward compatible: defaults to {}, unused by existing
  callers.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 6: Migrate `lay_report` schema to `{henId, eggs, timeOfDay}`

**Context:** Per the spec, this is a field-casing change for TS (not the breaking `date`→`timeOfDay`/`count`→`eggs` rename Rust needs) — today's TS `lay_report.schema.json` is `{hen_id, time_of_day, eggs}`. `henId`/`timeOfDay` also appear as GraphQL field names in **three** files, not one: `config/graph/lay_report.graphql` (the canonical `LayReport` type) and two duplicate copies embedded in `config/graph/hen.graphql` and `config/graph/coop.graphql` (used for the nested `hens.layReports` / `hen.layReports` resolver chains — graphql-js's `defaultFieldResolver` does a plain object-key lookup, so these field names must match the renamed payload keys exactly or they silently resolve to `null`). `config.conf`'s `lay_report` graphlette also references the old field name in its resolver (`id = "hen_id"`) and vector query (`payload.hen_id`).

**Files:**
- Modify: `examples/farm/config/json/lay_report.schema.json`
- Modify: `examples/farm/config/graph/lay_report.graphql`
- Modify: `examples/farm/config/graph/hen.graphql`
- Modify: `examples/farm/config/graph/coop.graphql`
- Modify: `examples/farm/config/config.conf`
- Test: `examples/farm/test/lay-report-schema.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `examples/farm/test/lay-report-schema.spec.ts`:
  ```typescript
  import { describe, it, expect } from 'vitest';
  import * as fs from 'fs';
  import * as path from 'path';
  import { JSONSchemaValidator } from '@tailoredshapes/meshql-restlette';

  const schema = JSON.parse(
      fs.readFileSync(path.resolve(__dirname, '../config/json/lay_report.schema.json'), 'utf8'),
  );
  const validate = JSONSchemaValidator(schema);

  describe('lay_report schema', () => {
      it('accepts the new {henId, eggs, timeOfDay} shape', async () => {
          const valid = await validate({
              henId: '11111111-1111-1111-1111-111111111111',
              eggs: 2,
              timeOfDay: 'morning',
          });
          expect(valid).toBe(true);
      });

      it('rejects the old {hen_id, time_of_day, eggs} shape', async () => {
          const valid = await validate({
              hen_id: '11111111-1111-1111-1111-111111111111',
              eggs: 2,
              time_of_day: 'morning',
          });
          expect(valid).toBe(false);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/lay-report-schema.spec.ts
  ```
  Expected: first test fails (`valid` is `false` — schema still requires `hen_id`/`time_of_day`), second test fails (`valid` is `true` — old shape still validates, and `additionalProperties: false` doesn't yet reject `henId`/`timeOfDay` as unknown keys under the old schema).

- [ ] **Step 3: Write minimal implementation**

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

  In `examples/farm/config/graph/hen.graphql`, replace the trailing duplicate `LayReport` type:
  ```graphql
  type LayReport {
    timeOfDay: String!
    eggs: Int!
    id: ID
  }
  ```

  In `examples/farm/config/graph/coop.graphql`, replace its duplicate `LayReport` type identically:
  ```graphql
  type LayReport {
    timeOfDay: String!
    eggs: Int!
    id: ID
  }
  ```

  In `examples/farm/config/config.conf`, in the `/lay_report/graph` block, update the vector query and resolver:
  ```hocon
        vectors = [
          {
            name = "getByHen"
            query = "{\"payload.henId\": \"{{id}}\"}"
          }
        ]
        resolvers = [
          {
            name = "hen"
            id = "henId"
            queryName = "getById"
            url = "http://farm:"${?PORT}"/hen/graph"
          }
        ]
  ```

  > Note: `examples/farm/test/config.ts` also has a stale, hand-written mirror of `config.conf` with `hen_id` references — it is **not imported anywhere in the codebase** (verified: no file references `test/config.ts`; it predates the current docker-compose-based BDD/smoke-test setup, which reads the real `config.conf` via HOCON at container start). It is dead code, left untouched by this plan; do not "fix" it as part of this task.

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/lay-report-schema.spec.ts
  ```
  Expected: `2 passed`.

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/config/json/lay_report.schema.json examples/farm/config/graph/lay_report.graphql \
      examples/farm/config/graph/hen.graphql examples/farm/config/graph/coop.graphql \
      examples/farm/config/config.conf examples/farm/test/lay-report-schema.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(farm): migrate lay_report schema to {henId, eggs, timeOfDay}

  Standardizes on camelCase FK naming (<parent>Id) per meshql-patterns,
  matching the shape all three languages converge on for this retrofit.
  Field rename touches three GraphQL files (lay_report.graphql plus two
  duplicated LayReport type copies in hen.graphql/coop.graphql) since
  graphql-js resolves object-type fields by plain key lookup.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 7: Remove legacy `eggs` field from `hen` — schema, GraphQL, and the tests it breaks

**Context:** Confirmed directly: `examples/farm/config/json/hen.schema.json` carries an `eggs` integer property that nothing keeps in sync — `hen_productivity` (Task 8) becomes the sole source of truth for egg counts. `eggs: Int` also appears on the `Hen` GraphQL type in **four** files: `hen.graphql`, `farm.graphql`, `coop.graphql`, and `lay_report.graphql` (each has its own copy of `type Hen`, needed for their respective nested-query shapes). Removing the GraphQL field breaks two existing tests that select `hens { eggs name }`: the BDD feature (`test/features/farm.feature` + its query in `test/steps/farm_steps.ts`) and the older duplicate smoke test (`test/farm.spec.ts`). Both also POST hen payloads containing `eggs`, which will now be rejected by `additionalProperties: false` — those payloads need the field dropped too, in the same commit, so the tree stays green.

**Files:**
- Modify: `examples/farm/config/json/hen.schema.json`
- Modify: `examples/farm/config/graph/hen.graphql`
- Modify: `examples/farm/config/graph/farm.graphql`
- Modify: `examples/farm/config/graph/coop.graphql`
- Modify: `examples/farm/config/graph/lay_report.graphql`
- Modify: `examples/farm/test/features/farm.feature`
- Modify: `examples/farm/test/steps/farm_steps.ts`
- Modify: `examples/farm/test/farm.spec.ts`
- Test: `examples/farm/test/hen-schema.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `examples/farm/test/hen-schema.spec.ts`:
  ```typescript
  import { describe, it, expect } from 'vitest';
  import * as fs from 'fs';
  import * as path from 'path';
  import { JSONSchemaValidator } from '@tailoredshapes/meshql-restlette';

  const schema = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../config/json/hen.schema.json'), 'utf8'));
  const validate = JSONSchemaValidator(schema);

  describe('hen schema', () => {
      it('has no eggs property', () => {
          expect(schema.properties.eggs).toBeUndefined();
      });

      it('rejects a payload carrying the legacy eggs field', async () => {
          const valid = await validate({ name: 'chuck', eggs: 6 });
          expect(valid).toBe(false);
      });

      it('still accepts a hen payload without eggs', async () => {
          const valid = await validate({ name: 'chuck', coop_id: '11111111-1111-1111-1111-111111111111' });
          expect(valid).toBe(true);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/hen-schema.spec.ts
  ```
  Expected: `schema.properties.eggs` is defined (fails), and the "rejects" test gets `valid === true` (fails) — `eggs` is still present and permitted.

- [ ] **Step 3: Write minimal implementation**

  In `examples/farm/config/json/hen.schema.json`, remove the `eggs` property entirely:
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

  Remove the `eggs: Int` line from `type Hen { ... }` in each of these four files (leave every other field as-is):
  - `examples/farm/config/graph/hen.graphql`
  - `examples/farm/config/graph/farm.graphql`
  - `examples/farm/config/graph/coop.graphql`
  - `examples/farm/config/graph/lay_report.graphql`

  For example, `hen.graphql`'s `Hen` type becomes:
  ```graphql
  type Hen {
    name: String!
    coop: Coop
    dob: Date
    id: ID
    layReports: [LayReport]
  }
  ```

  In `examples/farm/test/features/farm.feature`, drop `eggs` from the selection set:
  ```gherkin
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
  ```

  In `examples/farm/test/steps/farm_steps.ts`, drop `eggs` from the hen fixtures:
  ```typescript
      const hens = [
          { name: 'chuck', coop_id: this.coop1_id },
          { name: 'duck', coop_id: this.coop1_id },
          { name: 'euck', coop_id: this.coop2_id },
          { name: 'fuck', coop_id: this.coop2_id },
      ];
  ```

  In `examples/farm/test/farm.spec.ts`, drop `eggs` from both the query and the hen fixtures:
  ```typescript
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
  ```
  ```typescript
      const hens = [
          { name: 'chuck', coop_id: coop1_id },
          { name: 'duck', coop_id: coop1_id },
          { name: 'euck', coop_id: coop2_id },
          { name: 'fuck', coop_id: coop2_id },
      ];
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/hen-schema.spec.ts
  ```
  Expected: `3 passed`. Then confirm the rest of the package's fast (non-Docker) tests still pass:
  ```bash
  cd examples/farm && yarn vitest run test/manifest.spec.ts test/manifest-generator.spec.ts test/manifest-serving.spec.ts test/lay-report-schema.spec.ts
  ```
  (`test/farm.spec.ts` and the cucumber BDD suite require Docker — they are validated in Task 12/13's manual verification pass, not part of this fast loop.)

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/config/json/hen.schema.json examples/farm/config/graph/hen.graphql \
      examples/farm/config/graph/farm.graphql examples/farm/config/graph/coop.graphql \
      examples/farm/config/graph/lay_report.graphql examples/farm/test/features/farm.feature \
      examples/farm/test/steps/farm_steps.ts examples/farm/test/farm.spec.ts \
      examples/farm/test/hen-schema.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(farm): remove legacy eggs field from hen

  hen_productivity becomes the sole source of truth for egg counts;
  leaving a stale, never-updated eggs field on hen would be actively
  misleading. Removes the field from the JSON schema and from all four
  GraphQL files carrying a copy of `type Hen`, and updates the two
  existing tests whose fixtures/queries referenced it.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 8: New `hen_productivity` entity — JSON schema + GraphQL schema

**Context:** Per the spec, exact aggregate fields are an implementation decision. **Chosen shape: `{henId, totalEggs, lastLaidAt}`** — `henId` (FK, camelCase per the same convention as `lay_report`), `totalEggs` (running count, the fold of all `lay_report.eggs` for that hen), `lastLaidAt` (ISO-8601 timestamp of the most recent `lay_report` folded in). This is a reasonable, minimal aggregate that satisfies the spec's example ("total eggs, last-laid timestamp, etc.") without inventing an unbounded per-day breakdown nobody asked for. Because `hen_productivity` is a 1:1 aggregate per hen (unlike `lay_report`, which has many records per hen), its `getByHen` query is modeled as a GraphQL **singleton** (one result), not a vector — mirroring `coop.graphql`'s `getByName` singleton pattern rather than `lay_report.graphql`'s `getByHen` vector pattern. `lastLaidAt` is typed as GraphQL `String` (not the custom `Date` scalar used for date-only fields like `dob`) since it's a full ISO-8601 timestamp, not a bare date.

**Files:**
- Create: `examples/farm/config/json/hen_productivity.schema.json`
- Create: `examples/farm/config/graph/hen_productivity.graphql`
- Test: `examples/farm/test/hen-productivity-schema.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `examples/farm/test/hen-productivity-schema.spec.ts`:
  ```typescript
  import { describe, it, expect } from 'vitest';
  import * as fs from 'fs';
  import * as path from 'path';
  import { JSONSchemaValidator } from '@tailoredshapes/meshql-restlette';

  const schemaPath = path.resolve(__dirname, '../config/json/hen_productivity.schema.json');

  describe('hen_productivity schema', () => {
      it('exists on disk', () => {
          expect(fs.existsSync(schemaPath)).toBe(true);
      });

      it('accepts {henId, totalEggs, lastLaidAt}', async () => {
          const schema = JSON.parse(fs.readFileSync(schemaPath, 'utf8'));
          const validate = JSONSchemaValidator(schema);

          const valid = await validate({
              henId: '11111111-1111-1111-1111-111111111111',
              totalEggs: 42,
              lastLaidAt: '2026-07-22T08:00:00.000Z',
          });

          expect(valid).toBe(true);
      });

      it('rejects a payload missing totalEggs', async () => {
          const schema = JSON.parse(fs.readFileSync(schemaPath, 'utf8'));
          const validate = JSONSchemaValidator(schema);

          const valid = await validate({
              henId: '11111111-1111-1111-1111-111111111111',
              lastLaidAt: '2026-07-22T08:00:00.000Z',
          });

          expect(valid).toBe(false);
      });
  });

  describe('hen_productivity.graphql', () => {
      it('exists on disk', () => {
          const graphPath = path.resolve(__dirname, '../config/graph/hen_productivity.graphql');
          expect(fs.existsSync(graphPath)).toBe(true);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/hen-productivity-schema.spec.ts
  ```
  Expected: `ENOENT` / `expect(fs.existsSync(...)).toBe(true)` fails — neither file exists yet.

- [ ] **Step 3: Write minimal implementation**

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

  Create `examples/farm/config/graph/hen_productivity.graphql`:
  ```graphql
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
    lastLaidAt: String
    hen: Hen
    id: ID
  }
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/hen-productivity-schema.spec.ts
  ```
  Expected: `4 passed`.

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/config/json/hen_productivity.schema.json examples/farm/config/graph/hen_productivity.graphql \
      examples/farm/test/hen-productivity-schema.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(farm): add hen_productivity JSON and GraphQL schemas

  Aggregate shape: {henId, totalEggs, lastLaidAt}. Read-only from the
  FE's perspective — populated by the (out-of-scope-here) worker
  described in the merkql-worker-pipeline companion spec, via this
  entity's own ordinary restlette. Not yet wired into config.conf/
  index.ts — that's Task 10.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 9: Casbin `model.conf` + `policy.csv` for the farm example

**Context:** Per the spec's target policy: a `worker` role authorized for `create`+`update` on `hen_productivity`; general/FE callers (no token, or a non-worker token) authorized for `create` on `farm`/`coop`/`hen`/`lay_report`, plus `update`/`delete` on `farm`/`coop`/`hen` only (not `lay_report`), and explicitly **not** authorized for any verb on `hen_productivity`. The model uses a flat `sub == p.sub` matcher (not `g()` in the matcher itself) because `CasbinAuth.getAuthToken` already pre-resolves the JWT `sub` to role names via `enforcer.getRolesForUser()` before `authorizeAction` ever calls `enforce()` — so by the time `enforce()` runs, `credentials` already **are** role names (or the literal `"anonymous"` fallback from Task 3). The model still declares `[role_definition] g = _, _` because `getRolesForUser` needs it to resolve `g` policy lines, even though the `enforce()` matcher itself doesn't call `g()`.

**Files:**
- Create: `examples/farm/config/casbin/model.conf`
- Create: `examples/farm/config/casbin/policy.csv`
- Modify: `examples/farm/package.json` (add `casbin_auth`/`jwt_auth`/`auth` deps, `jsonwebtoken` dev dep)
- Test: `examples/farm/test/casbin-policy.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `examples/farm/test/casbin-policy.spec.ts`:
  ```typescript
  import { describe, it, expect } from 'vitest';
  import * as path from 'path';
  import { newEnforcer } from 'casbin';

  const modelPath = path.resolve(__dirname, '../config/casbin/model.conf');
  const policyPath = path.resolve(__dirname, '../config/casbin/policy.csv');

  describe('farm casbin policy', () => {
      it('lets anonymous create farm/coop/hen/lay_report', async () => {
          const enforcer = await newEnforcer(modelPath, policyPath);

          for (const resource of ['farm', 'coop', 'hen', 'lay_report']) {
              expect(await enforcer.enforce('anonymous', resource, 'create')).toBe(true);
          }
      });

      it('lets anonymous update/delete farm/coop/hen but not lay_report', async () => {
          const enforcer = await newEnforcer(modelPath, policyPath);

          for (const resource of ['farm', 'coop', 'hen']) {
              expect(await enforcer.enforce('anonymous', resource, 'update')).toBe(true);
              expect(await enforcer.enforce('anonymous', resource, 'delete')).toBe(true);
          }

          expect(await enforcer.enforce('anonymous', 'lay_report', 'update')).toBe(false);
          expect(await enforcer.enforce('anonymous', 'lay_report', 'delete')).toBe(false);
      });

      it('denies anonymous every verb on hen_productivity', async () => {
          const enforcer = await newEnforcer(modelPath, policyPath);

          for (const action of ['create', 'update', 'delete']) {
              expect(await enforcer.enforce('anonymous', 'hen_productivity', action)).toBe(false);
          }
      });

      it('lets the worker role create and update hen_productivity, but not delete', async () => {
          const enforcer = await newEnforcer(modelPath, policyPath);

          expect(await enforcer.enforce('worker', 'hen_productivity', 'create')).toBe(true);
          expect(await enforcer.enforce('worker', 'hen_productivity', 'update')).toBe(true);
          expect(await enforcer.enforce('worker', 'hen_productivity', 'delete')).toBe(false);
      });

      it('resolves the farm-worker-service subject to the worker role via g', async () => {
          const enforcer = await newEnforcer(modelPath, policyPath);
          const roles = await enforcer.getRolesForUser('farm-worker-service');

          expect(roles).toContain('worker');
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/casbin-policy.spec.ts
  ```
  Expected: `ENOENT: no such file or directory, open '.../config/casbin/model.conf'`.

- [ ] **Step 3: Write minimal implementation**

  Create `examples/farm/config/casbin/model.conf`:
  ```ini
  [request_definition]
  r = sub, obj, act

  [policy_definition]
  p = sub, obj, act

  [role_definition]
  g = _, _

  [policy_effect]
  e = some(where (p.eft == allow))

  [matchers]
  m = r.sub == p.sub && r.obj == p.obj && r.act == p.act
  ```

  Create `examples/farm/config/casbin/policy.csv`:
  ```csv
  p, anonymous, farm, create
  p, anonymous, farm, update
  p, anonymous, farm, delete
  p, anonymous, coop, create
  p, anonymous, coop, update
  p, anonymous, coop, delete
  p, anonymous, hen, create
  p, anonymous, hen, update
  p, anonymous, hen, delete
  p, anonymous, lay_report, create
  p, worker, hen_productivity, create
  p, worker, hen_productivity, update
  g, farm-worker-service, worker
  ```

  Add dependencies to `examples/farm/package.json`. Under `"dependencies"`:
  ```json
      "@tailoredshapes/meshql-auth": "workspace:^",
      "@tailoredshapes/meshql-casbin_auth": "workspace:^",
      "@tailoredshapes/meshql-jwt_auth": "workspace:^",
  ```
  Under `"devDependencies"`, add (for Task 12's test-only JWT construction and Task 9's policy test):
  ```json
      "@tailoredshapes/meshql-sqlite_repo": "workspace:^",
      "@types/jsonwebtoken": "^9.0.5",
      "casbin": "^5.36.0",
      "jsonwebtoken": "^9.0.2",
  ```
  Then install:
  ```bash
  yarn install
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/casbin-policy.spec.ts
  ```
  Expected: `5 passed`.

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/config/casbin examples/farm/package.json yarn.lock examples/farm/test/casbin-policy.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(farm): add Casbin model/policy for farm's write authorization

  worker role: create+update hen_productivity. Everyone else: create
  farm/coop/hen/lay_report, update/delete farm/coop/hen only (not
  lay_report, not hen_productivity at all). Not yet wired into
  index.ts/config.conf — that's Task 10.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 10: Wire `hen_productivity` and per-restlette Casbin auth into `config.conf`/`index.ts`

**Context:** Adds the `hen_productivity` Mongo storage block, graphlette, and restlette to `config.conf` (an "ordinary restlette+graphlette pair", per the spec — nothing about its wiring differs structurally from `farm`/`coop`/`hen`). Then builds five resource-scoped `CasbinAuth` instances (one `enforcer`/policy load, cloned via `withResource` from Task 3) and passes them to `init()`'s new `restletteAuthOverrides` param (Task 5) so every restlette — not just the two new ones — gets real verb-aware authorization instead of the open-by-default `JWTSubAuthorizer`. The construction logic is extracted into its own exported function in `examples/farm/auth.ts` so it's independently testable (Task 12) without spinning up the whole app.

**Files:**
- Create: `examples/farm/auth.ts`
- Modify: `examples/farm/config/config.conf`
- Modify: `examples/farm/index.ts`
- Test: `examples/farm/test/auth-builder.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `examples/farm/test/auth-builder.spec.ts`:
  ```typescript
  import { describe, it, expect } from 'vitest';
  import * as path from 'path';
  import { buildRestletteAuthOverrides } from '../auth';

  const configDir = path.resolve(__dirname, '../config');

  describe('buildRestletteAuthOverrides', () => {
      it('returns a scoped Auth for every farm restlette path', async () => {
          const overrides = await buildRestletteAuthOverrides(configDir);

          expect(Object.keys(overrides).sort()).toEqual(
              ['/coop/api', '/farm/api', '/hen/api', '/hen_productivity/api', '/lay_report/api'].sort(),
          );
      });

      it('each override is independently resource-scoped', async () => {
          const overrides = await buildRestletteAuthOverrides(configDir);

          const farmAllowsCreate = await overrides['/farm/api'].authorizeAction([], 'create', { payload: {} });
          const layReportDeniesUpdate = await overrides['/lay_report/api'].authorizeAction([], 'update', {
              payload: {},
          });
          const productivityDeniesAnonymousCreate = await overrides['/hen_productivity/api'].authorizeAction(
              [],
              'create',
              { payload: {} },
          );

          expect(farmAllowsCreate).toBe(true);
          expect(layReportDeniesUpdate).toBe(false);
          expect(productivityDeniesAnonymousCreate).toBe(false);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/auth-builder.spec.ts
  ```
  Expected: `Cannot find module '../auth'` — the file doesn't exist yet.

- [ ] **Step 3: Write minimal implementation**

  Create `examples/farm/auth.ts`:
  ```typescript
  import * as path from 'path';
  import { CasbinAuth } from '@tailoredshapes/meshql-casbin_auth';
  import { JWTSubAuthorizer } from '@tailoredshapes/meshql-jwt_auth';
  import { Auth } from '@tailoredshapes/meshql-auth';

  const RESTLETTE_RESOURCES: Record<string, string> = {
      '/farm/api': 'farm',
      '/coop/api': 'coop',
      '/hen/api': 'hen',
      '/lay_report/api': 'lay_report',
      '/hen_productivity/api': 'hen_productivity',
  };

  /**
   * Builds one CasbinAuth per farm restlette, each scoped (via withResource)
   * to that restlette's own Casbin object, all sharing a single loaded
   * model.conf/policy.csv. Keyed by restlette path for direct use as
   * meshql-server's init()'s restletteAuthOverrides argument.
   */
  export async function buildRestletteAuthOverrides(configDir: string): Promise<Record<string, Auth>> {
      const modelPath = path.join(configDir, 'casbin', 'model.conf');
      const policyPath = path.join(configDir, 'casbin', 'policy.csv');

      const base = await CasbinAuth.create([modelPath, policyPath], new JWTSubAuthorizer());

      const overrides: Record<string, Auth> = {};
      for (const [restlettePath, resource] of Object.entries(RESTLETTE_RESOURCES)) {
          overrides[restlettePath] = base.withResource(resource);
      }
      return overrides;
  }
  ```

  In `examples/farm/config/config.conf`, add a new storage block (alongside `henDB`/`layReportDB`/etc):
  ```hocon
    henProductivityDB = {
      type = "mongo"
      uri = ${?MONGO_URI}
      db = ${?PREFIX}_${?ENV}
      collection = ${?PREFIX}-${?ENV}-hen_productivity
      options {
        directConnection = true
      }
    }
  ```
  Add a graphlette entry (inside the `graphlettes = [ ... ]` array, after `/lay_report/graph`):
  ```hocon
    {
      path = "/hen_productivity/graph"
      storage = ${henProductivityDB}
      schema = include file(./graph/hen_productivity.graphql)
      rootConfig {
        singletons = [
          {
            name = "getById"
            query = "{\"id\": \"{{id}}\"}"
          },
          {
            name = "getByHen"
            query = "{\"payload.henId\": \"{{id}}\"}"
          }
        ]
        vectors = []
        resolvers = [
          {
            name = "hen"
            id = "henId"
            queryName = "getById"
            url = "http://farm:"${?PORT}"/hen/graph"
          }
        ]
      }
    }
  ```
  Add a restlette entry (inside the `restlettes = [ ... ]` array, after `/lay_report/api`):
  ```hocon
    {
      path = "/hen_productivity/api"
      storage = ${henProductivityDB}
      schema = include file(json/hen_productivity.schema.json)
    }
  ```

  Replace `examples/farm/index.ts` in full:
  ```typescript
  import * as fs from 'fs';
  import * as path from 'path';
  import { init, Config } from '@tailoredshapes/meshql-server';
  import { MongoPlugin } from '@tailoredshapes/meshql-mongo_repo';
  import { getLogger } from '@tailoredshapes/meshql-common';
  import { mountManifestRoute } from './manifest';
  import { buildRestletteAuthOverrides } from './auth';
  const parser = require('@pushcorn/hocon-parser');

  const log = getLogger('meshql-ts/farm-example');

  async function main() {
      const configDir = path.resolve(__dirname, 'config');
      const configFile = path.join(configDir, 'config.conf');
      const config: Config = await parser.parse({ url: configFile });

      const restletteAuthOverrides = await buildRestletteAuthOverrides(configDir);
      const app = await init(config, { mongo: new MongoPlugin() }, restletteAuthOverrides);

      const manifestPath = path.join(configDir, 'manifest.json');
      const manifestJson = fs.readFileSync(manifestPath, 'utf8');
      mountManifestRoute(app, manifestJson);

      await app.listen(config.port);
      log.info(`Farm example running on port ${config.port} — manifest at /manifest`);
  }

  main().catch((err) => {
      log.error('Failed to start farm example:', err);
      process.exit(1);
  });
  ```

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/auth-builder.spec.ts
  ```
  Expected: `2 passed`. Then typecheck the whole package (catches config.conf typos only at runtime, but confirms `index.ts`/`auth.ts` compile):
  ```bash
  cd examples/farm && yarn build
  ```

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/auth.ts examples/farm/config/config.conf examples/farm/index.ts \
      examples/farm/test/auth-builder.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(farm): wire hen_productivity and per-restlette Casbin auth

  hen_productivity joins config.conf as an ordinary restlette+graphlette
  pair (Mongo-backed, same as every other farm entity). index.ts now
  builds five resource-scoped CasbinAuth instances from one loaded
  policy and passes them to init()'s restletteAuthOverrides, so every
  restlette gets real create/update/delete authorization instead of the
  previously-unauthenticated JWTSubAuthorizer default.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 11: Regenerate `manifest.json` and add `hen_productivity` conformance assertions

**Context:** Per the spec, the manifest generator (`examples/farm/manifest.ts`) needs no filtering logic removed — it already emits an `api` surface conditionally on whether a `.schema.json` file exists (confirmed: no `ALL_VERBS`-style noun/verb hiding was ever ported to farm's generator). This task just regenerates the manifest now that `hen_productivity`'s files exist (Task 8) and `lay_report`/`hen`'s schemas changed (Tasks 6–7), and adds an explicit assertion — beyond the generic "every graph file has an entry" loop already in `manifest.spec.ts` — that `hen_productivity` specifically shows both surfaces, per the spec's call for a regression check here.

**Files:**
- Modify: `examples/farm/config/manifest.json` (generated, not hand-edited)
- Modify: `examples/farm/test/manifest-generator.spec.ts`
- Modify: `examples/farm/test/manifest.spec.ts`

- [ ] **Step 1: Write the failing test**

  In `examples/farm/test/manifest-generator.spec.ts`, extend the first `it`:
  ```typescript
      it('produces an entry for every .graphql file', () => {
          const manifest = generate(configDir);

          expect(manifest.meshql).toBe(1);
          expect(manifest.entities.farm).toBeDefined();
          expect(manifest.entities.coop).toBeDefined();
          expect(manifest.entities.hen).toBeDefined();
          expect(manifest.entities.hen_productivity).toBeDefined();
      });
  ```

  In `examples/farm/test/manifest.spec.ts`, add a new `it` inside the `manifest conformance` describe block:
  ```typescript
      it('hen_productivity advertises both graph and api surfaces, same as every other entity', () => {
          const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
          const productivity = manifest.entities.hen_productivity;

          expect(productivity, 'hen_productivity missing from manifest').toBeDefined();
          expect(productivity.surfaces.graph.kind).toBe('graphql');
          expect(productivity.surfaces.graph.path).toBe('/hen_productivity/graph');
          expect(productivity.surfaces.api.kind).toBe('rest');
          expect(productivity.surfaces.api.path).toBe('/hen_productivity/api');
      });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/manifest-generator.spec.ts test/manifest.spec.ts
  ```
  Expected: `manifest.entities.hen_productivity` is `undefined` in the generator test (fails), and the new manifest.spec.ts test fails both on the stale `manifest.json` (doesn't have `hen_productivity` yet) and, separately, the existing `matches fresh regeneration` test now fails too since `generate(configDir)` picks up the Task 6/7/8/10 schema changes that `manifest.json` doesn't reflect yet.

- [ ] **Step 3: Write minimal implementation**

  Regenerate the manifest and format it:
  ```bash
  cd examples/farm && yarn gen-manifest && cd ../.. && yarn format
  ```
  This overwrites `examples/farm/config/manifest.json` from the current `config/graph/*.graphql` + `config/json/*.schema.json` — no hand-editing.

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/manifest-generator.spec.ts test/manifest.spec.ts
  ```
  Expected: all pass, including `matches fresh regeneration` (committed `manifest.json` now equals a fresh `generate(configDir)` call) and the new `hen_productivity advertises both graph and api surfaces` test.

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/config/manifest.json examples/farm/test/manifest-generator.spec.ts examples/farm/test/manifest.spec.ts
  git commit -m "$(cat <<'EOF'
  feat(farm): regenerate manifest.json for hen_productivity + schema changes

  No filtering logic changed — farm's generator already had nothing to
  remove here (confirmed: no ALL_VERBS-style noun/verb hiding was ever
  ported to it). Adds an explicit conformance assertion that
  hen_productivity advertises both graph and api surfaces, same as
  every other entity, per the spec's "always advertise both surfaces"
  rule.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 12: Fast in-process auth-behavior spec (no Docker)

**Context:** The task prompt calls for tests reflecting `lay_report`'s new create-only write contract, `hen_productivity`'s existence, and the new auth wiring. Rather than extending the slow (300s timeout, Docker-dependent) BDD suite, this exercises the real `buildRestletteAuthOverrides` helper (Task 10) and the real Casbin policy (Task 9) end-to-end through actual HTTP requests against an in-process Express app — using `@tailoredshapes/meshql-sqlite_repo` for storage instead of Mongo, so it runs in milliseconds with no container. This mirrors `core/server/test/health.spec.ts`'s existing fast in-process pattern.

**Files:**
- Test: `examples/farm/test/auth-wiring.spec.ts`

- [ ] **Step 1: Write the failing test**

  Create `examples/farm/test/auth-wiring.spec.ts`:
  ```typescript
  import { describe, it, expect, beforeAll, afterAll } from 'vitest';
  import * as fs from 'fs';
  import * as path from 'path';
  import jwt from 'jsonwebtoken';
  import { init, Config, Restlette } from '@tailoredshapes/meshql-server';
  import { SQLitePlugin, SQLConfig } from '@tailoredshapes/meshql-sqlite_repo';
  import { buildRestletteAuthOverrides } from '../auth';

  const configDir = path.resolve(__dirname, '../config');
  const port = 40520;

  const layReportSchema = JSON.parse(fs.readFileSync(path.join(configDir, 'json/lay_report.schema.json'), 'utf8'));
  const productivitySchema = JSON.parse(
      fs.readFileSync(path.join(configDir, 'json/hen_productivity.schema.json'), 'utf8'),
  );

  const storage = (collection: string): SQLConfig => ({ type: 'sqlite', uri: ':memory:', collection });

  const workerToken = jwt.sign({ sub: 'farm-worker-service' }, 'unused-in-decode-only-mode');

  describe('farm write authorization (in-process, sqlite-backed)', () => {
      let app: any;
      let server: any;
      let layReportId: string;

      beforeAll(async () => {
          const layReport: Restlette = { path: '/lay_report/api', storage: storage('lay_report'), schema: layReportSchema };
          const productivity: Restlette = {
              path: '/hen_productivity/api',
              storage: storage('hen_productivity'),
              schema: productivitySchema,
          };

          const config: Config = { port, graphlettes: [], restlettes: [layReport, productivity] };
          const overrides = await buildRestletteAuthOverrides(configDir);

          app = await init(config, { sqlite: new SQLitePlugin() }, overrides);
          server = app.listen(port);

          const created = await fetch(`http://localhost:${port}/lay_report/api`, {
              method: 'POST',
              body: JSON.stringify({ henId: '11111111-1111-1111-1111-111111111111', eggs: 1, timeOfDay: 'morning' }),
              headers: { 'Content-Type': 'application/json' },
          });
          layReportId = new URL(created.url).pathname.split('/').pop()!;
      });

      afterAll(() => {
          server.close();
      });

      it('anonymous can create a lay_report', async () => {
          const response = await fetch(`http://localhost:${port}/lay_report/api`, {
              method: 'POST',
              body: JSON.stringify({ henId: '22222222-2222-2222-2222-222222222222', eggs: 2, timeOfDay: 'evening' }),
              headers: { 'Content-Type': 'application/json' },
          });

          expect(response.status).toBe(200); // 303 redirect followed to read, which returns 200
      });

      it('anonymous cannot update a lay_report', async () => {
          const response = await fetch(`http://localhost:${port}/lay_report/api/${layReportId}`, {
              method: 'PUT',
              body: JSON.stringify({ henId: '11111111-1111-1111-1111-111111111111', eggs: 3, timeOfDay: 'afternoon' }),
              headers: { 'Content-Type': 'application/json' },
          });

          expect(response.status).toBe(403);
      });

      it('anonymous cannot delete a lay_report', async () => {
          const response = await fetch(`http://localhost:${port}/lay_report/api/${layReportId}`, {
              method: 'DELETE',
          });

          expect(response.status).toBe(403);
      });

      it('anonymous cannot create hen_productivity', async () => {
          const response = await fetch(`http://localhost:${port}/hen_productivity/api`, {
              method: 'POST',
              body: JSON.stringify({
                  henId: '11111111-1111-1111-1111-111111111111',
                  totalEggs: 1,
                  lastLaidAt: '2026-07-22T08:00:00.000Z',
              }),
              headers: { 'Content-Type': 'application/json' },
          });

          expect(response.status).toBe(403);
      });

      it('the worker role can create and update hen_productivity', async () => {
          const createResponse = await fetch(`http://localhost:${port}/hen_productivity/api`, {
              method: 'POST',
              body: JSON.stringify({
                  henId: '11111111-1111-1111-1111-111111111111',
                  totalEggs: 1,
                  lastLaidAt: '2026-07-22T08:00:00.000Z',
              }),
              headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${workerToken}` },
          });
          expect(createResponse.status).toBe(200);

          const id = new URL(createResponse.url).pathname.split('/').pop()!;

          const updateResponse = await fetch(`http://localhost:${port}/hen_productivity/api/${id}`, {
              method: 'PUT',
              body: JSON.stringify({
                  henId: '11111111-1111-1111-1111-111111111111',
                  totalEggs: 2,
                  lastLaidAt: '2026-07-22T09:00:00.000Z',
              }),
              headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${workerToken}` },
          });
          expect(updateResponse.status).toBe(200);
      });
  });
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cd examples/farm && yarn vitest run test/auth-wiring.spec.ts
  ```
  Expected (before Tasks 6–10 land, or if run in isolation before this branch's other work is present): fails on schema field names or on every write returning `200`/`303` regardless of credentials, since without the earlier tasks nothing denies anonymous writes. If run after Tasks 1–11 are all committed (the expected order), this should already mostly pass — treat any failure here as a real signal to go back and check the specific wiring (which restlette path, which resource string) rather than a routine red step.

- [ ] **Step 3: Write minimal implementation**

  No production code changes expected at this point — Tasks 1–10 already implemented everything this test exercises. If a specific assertion fails, the most likely causes, in order of likelihood:
  1. A restlette `path` in the test's inline `Config` doesn't exactly match a key in `RESTLETTE_RESOURCES` (`examples/farm/auth.ts`) — `buildRestletteAuthOverrides` silently returns `{}` for unmatched paths, meaning `init()` falls back to the open default `JWTSubAuthorizer` for that path instead of the intended `CasbinAuth`.
  2. `policy.csv` (Task 9) has a typo in a resource or action string.
  3. `jwt.sign`'s payload key isn't `sub`, or `g, farm-worker-service, worker` in `policy.csv` doesn't match the token's `sub` claim exactly.

- [ ] **Step 4: Run test to verify it passes**

  ```bash
  cd examples/farm && yarn vitest run test/auth-wiring.spec.ts
  ```
  Expected: `5 passed`.

- [ ] **Step 5: Commit**

  ```bash
  git add examples/farm/test/auth-wiring.spec.ts
  git commit -m "$(cat <<'EOF'
  test(farm): end-to-end auth-wiring coverage for lay_report + hen_productivity

  In-process, sqlite-backed (no Docker) — exercises the real
  buildRestletteAuthOverrides + Casbin policy through actual HTTP
  requests. Confirms lay_report is create-only for anonymous callers
  and hen_productivity is worker-only for every verb.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 13: Update `examples/farm/README.md`, then hand off for manual push

**Context:** `examples/farm/README.md` currently describes a plain-CRUD, three-heterogeneous-database example (its "Coops (PostgreSQL)"/"Hens (MySQL)" section is already stale relative to `config.conf`, which uses Mongo for everything — that pre-existing inaccuracy is out of scope for this plan and is left as-is). This task adds a section describing the event-sourced shape this retrofit introduces: `lay_report` as a create-only domain event, `hen_productivity` as a worker-populated read model, and the write-authorization model now in effect.

**Files:**
- Modify: `examples/farm/README.md`

- [ ] **Step 1: Write the failing test**

  Not applicable — documentation-only task, no automated assertion. Skip directly to Step 3.

- [ ] **Step 2: Run test to verify it fails**

  Not applicable.

- [ ] **Step 3: Write minimal implementation**

  In `examples/farm/README.md`, add a new section after "### Key Features" and before "## Getting Started":
  ```markdown
  ### Event-Sourced Entities

  Two entities depart from the plain-CRUD pattern the rest of this example uses:

  - **`lay_report`** (`POST /lay_report/api`) is a domain event, not a mutable
    record: `{henId, eggs, timeOfDay}`. It's create-only — `PUT`/`DELETE`
    against `/lay_report/api/:id` are rejected (`403`) for every caller. A
    correction, if ever needed, is a new event, not an edit.
  - **`hen_productivity`** (`/hen_productivity/api`, `/hen_productivity/graph`)
    is a read model folded from `lay_report` events — `{henId, totalEggs,
    lastLaidAt}`. It's an ordinary restlette+graphlette pair like every other
    entity here; what's unusual is *who* writes to it: only a `worker`-role
    caller (simulating the CDC-driven worker described in the
    `merkql-worker-pipeline` companion spec, which is out of scope for this
    TS example) may `create`/`update` it. Every other caller gets `403` on
    every verb.

  ### Write Authorization

  `farm`/`coop`/`hen` stay fully CRUD-able by any caller. `lay_report` is
  create-only for everyone. `hen_productivity` is writable only by the
  `worker` role. This is enforced by a Casbin policy
  (`config/casbin/model.conf`, `config/casbin/policy.csv`) loaded once and
  scoped per-restlette in `auth.ts`'s `buildRestletteAuthOverrides` — see
  that file and `index.ts` for the wiring. GraphQL reads stay open to
  everyone; this only affects REST writes.
  ```

- [ ] **Step 4: Run test to verify it passes**

  Not applicable — visually confirm the rendered Markdown reads sensibly:
  ```bash
  cd examples/farm && head -60 README.md
  ```

- [ ] **Step 5: Commit, then hand off**

  ```bash
  git add examples/farm/README.md
  git commit -m "$(cat <<'EOF'
  docs(farm): document the event-sourced lay_report/hen_productivity shape

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

  **This plan is now complete.** Run the full package suite one more time from the worktree root to confirm nothing regressed:
  ```bash
  cd core/auth && yarn test && cd ../jwt_auth && yarn test && cd ../casbin_auth && yarn test && \
  cd ../restlette && yarn test && cd ../graphlette && yarn test && cd ../server && yarn test && \
  cd ../../examples/farm && yarn test
  ```

  **Hand off to the user.** This environment has no push credentials configured for the AI agent. All 13 tasks are committed locally on the `farm-retrofit-ts` branch in the worktree created at the start of this plan. Tell the user the worktree path and branch name, and that they need to review the diff and push manually (e.g. `git push -u origin farm-retrofit-ts`) before opening a PR. Do not attempt to push on their behalf.
