// CXA-F285 — close out P5a: POST /api/projects/:pid/store under gateway auth.
//
// The enforcement is server-side (auth_mw write gate + membership branch in
// auth.rs, plus authorize_store_call in store_rpc.rs as defense-in-depth).
// The Rust guards pin it in-process (store_rpc_auth_gate.rs over all twelve
// ops; store_rpc_guard_tests.rs the in-handler bodies) — but no auth spec
// drove /store against the REAL auth-enabled hub over the wire until this
// file: the bearer path a real runner uses was never exercised e2e.
//
// Hub under test: run-server-auth.sh (port 4518, RBAC on, project `default`,
// throwaway .state-auth state wiped every boot — the token store at
// .state-auth/ however PERSISTS across boots, so token labels here carry a
// run-unique stamp and must never be minted twice under the same name).
//
// Rate-limit identity: the hub runs with COXAGENT_TRUST_PROXY=1 (see
// run-server-auth.sh), so a request carrying X-Forwarded-For is limited as
// its own client instead of sharing the suite's one loopback bucket. This
// file IS one logical client (a runner/watchdog operator); the constant XFF
// below gives its handful of /api/auth POSTs their own window, leaving the
// shared bucket to the specs that send no XFF.
const CLIENT_XFF = { 'X-Forwarded-For': '10.7.0.42' };

// Rate-limit budget: every non-GET /api/auth/* call shares one 20-per-60s
// window per client IP (rate_limit_mw), and the whole suite runs in well
// under that window on one loopback IP. Measured on this suite: the other
// specs spend 16 POSTs of the window, so this file historically budgeted
// exactly FOUR. CXA-F350 adds the lead-tier persona (one user upsert, one
// lead login, one lead personal-token mint) — three more /api/auth POSTs —
// and run-server-auth.sh now raises the window with the AUTH_RATE_MAX env
// var that same ticket introduced, so the budget note below is the
// without-override shape, kept for operators running default limits.
// The viewer account itself is NOT unconditionally created:
// rbac-viewer provisions it earlier in the run (the suite's documented
// cross-file fixture sharing); a member-assign probe detects its absence
// (standalone runs) and only then pays the create POST. No retry loops over
// credential endpoints.
//
// Cookie-jar discipline: a login leaves cox_session in this file's request
// jar, and resolve_principal falls back to the cookie when a bearer is
// absent or rejected (guards.rs) — so a stale session silently authenticates
// a call meant to be anonymous (an invalid bearer + valid cookie resolves as
// the cookie's principal). Every assertion about credential semantics
// therefore clears the context cookies first; bearer callers then prove the
// BEARER alone carries them.
//
// Wire-shape note (verified against the code, CXA-F285): with auth enabled
// the middleware answers first, so an unauthenticated call sees 401
// {"error":"unauthenticated"} from auth_mw and a refused principal sees 403
// {"error":"insufficient role"} (write gate) / {"error":"not a member of
// this project"} (membership branch) — the in-handler bodies ("sign in
// first" / "management role required") sit behind the middleware as
// defense-in-depth and are pinned in store_rpc_guard_tests.rs. This spec
// asserts statuses (and the membership-branch message), not which layer drew.
import { test, expect } from '@playwright/test';
import { execFile } from 'node:child_process';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ADMIN_USER, ADMIN_PASSWORD } from './helpers-auth.mjs';

const PID = 'default';
const STORE = `/api/projects/${PID}/store`;
const VIEWER_USER = 'viewere2e';
const VIEWER_PASSWORD = 'ViewerPass_12345';
// CXA-F350 persona: a lead-tier member whose personal bearer token must
// inherit the member's project memberships. Run-unique like every label
// here — the token store persists across boots.
const LEAD_USER = 'leade2e';
const LEAD_PASSWORD = 'LeadPass_12345';

/// The full twelve-op wire surface RestStateStore drives over this route
/// (crates/infrastructure/src/state/rest_store.rs; pinned in-process by
/// store_rpc_auth_gate.rs::OPS).
const OPS = [
  'load',
  'version',
  'save',
  'claim_ticket',
  'acquire_leader',
  'claim_stage',
  'release_stage',
  'heartbeat',
  'workers',
  'set_desired',
  'get_desired',
  'acquire_operator',
];

/// POST one store op; returns { status, body }. Credentials via `headers` —
/// a bearer for the runner class, the request jar's cox_session for a
/// signed-in browser-class caller.
async function storeOp(request, op, headers = {}, data = {}) {
  const resp = await request.post(`${STORE}?op=${op}`, {
    data,
    headers,
    failOnStatusCode: false,
  });
  let body = null;
  try {
    body = await resp.json();
  } catch {
    body = await resp.text();
  }
  return { status: resp.status(), body };
}

// Minted secrets live only for this boot's process: labels are stamped with
// a run-unique suffix because the token store persists across boots and a
// repeated label answers 409 "label already in use". One mint per account
// per run, cached module-wide — the suite's rate-limit window is shared.
let adminTokenPromise;

/// The hub admin's personal API token: the Super-tier runner credential the
/// boot provisions (bootstrap_admin -> role Super), minted exactly the way a
/// runner operator mints theirs — sign in, POST /api/auth/my/tokens — and
/// member-exempt on /store by the gate's Super/Admin exemption.
function adminToken(page) {
  if (!adminTokenPromise) {
    adminTokenPromise = (async () => {
      const login = await page.request.post('/api/auth/login', {
        data: { username: ADMIN_USER, password: ADMIN_PASSWORD },
        headers: CLIENT_XFF,
      });
      expect(login.status(), 'adminos should log in').toBe(200);
      const minted = await page.request.post('/api/auth/my/tokens', {
        data: { label: `f285-admin-${Date.now()}` },
        headers: CLIENT_XFF,
      });
      expect(minted.status(), 'admin token mint').toBe(200);
      const token = (await minted.json()).token;
      // mint_token returns 32 random bytes as 64 hex chars.
      expect(token).toMatch(/^[0-9a-f]{64}$/);
      return token;
    })();
  }
  return adminTokenPromise;
}

test('AC1: without any credential every store op is refused 401 and the state stays untouched', async ({
  page,
}) => {
  const token = await adminToken(page);
  const auth = { Authorization: `Bearer ${token}` };
  // The mint above signed this context in; drop the session so the calls
  // below carry no credential but the ones with the explicit bearer.
  await page.context().clearCookies();
  const before = await storeOp(page.request, 'load', auth);
  expect(before.status).toBe(200);

  for (const op of OPS) {
    const refused = await storeOp(page.request, op);
    expect(refused.status, `anonymous op=${op}`).toBe(401);
  }

  const after = await storeOp(page.request, 'load', auth);
  expect(after.status).toBe(200);
  expect(after.body, 'no refused op may have modified the store').toEqual(
    before.body,
  );
});

test('AC1: a forged bearer is exactly as good as none', async ({ page }) => {
  await adminToken(page); // the same boot the other tests run against
  // A stale valid session would authenticate the call through the cookie
  // fallback even with the forged bearer — the jar must be empty.
  await page.context().clearCookies();
  const refused = await storeOp(page.request, 'load', {
    Authorization: 'Bearer f285-forged-token',
  });
  expect(refused.status).toBe(401);
});

test('AC3: an admin personal API token drives the exact runner wire shape', async ({
  page,
}) => {
  await page.context().clearCookies();
  const auth = { Authorization: `Bearer ${await adminToken(page)}` };
  const load = await storeOp(page.request, 'load', auth);
  expect(load.status).toBe(200);
  expect(Array.isArray(load.body.tickets), 'op=load yields the ticket array')
    .toBe(true);

  // op=version answers the caller's read-modify-write token. The file-backed
  // e2e hub tracks no revisions (StateStorePort default -> `revision: null`);
  // Postgres-backed hubs answer a number. The numeric-revision + 409 contract
  // is pinned in-process (rest_state_store_contract.rs,
  // store_rpc_stale_write_tests.rs) — here we pin that the op is reachable
  // and answers the documented envelope, not which backend the hub runs.
  const version = await storeOp(page.request, 'version', auth);
  expect(version.status).toBe(200);
  expect(version.body).toHaveProperty('revision');
});

test('AC3: a full read-modify-write over the wire lands and never clobbers', async ({
  page,
}) => {
  await page.context().clearCookies();
  const auth = { Authorization: `Bearer ${await adminToken(page)}` };

  const version = await storeOp(page.request, 'version', auth);
  expect(version.status).toBe(200);
  const load = await storeOp(page.request, 'load', auth);
  expect(load.status).toBe(200);

  // Save exactly what was loaded, with the revision token as captured — the
  // RestStateStore save_expecting wire shape.
  const save = await storeOp(page.request, 'save', auth, {
    revision: version.body.revision,
    data: JSON.stringify(load.body),
  });
  expect(save.status).toBe(200);
  expect(save.body).toEqual({ ok: true });

  const reread = await storeOp(page.request, 'load', auth);
  expect(reread.status).toBe(200);
  expect(reread.body).toEqual(load.body);

  // Replay the identical save. On this file-backed hub (no revision
  // tracking) the replay is accepted, not 409'd — the stale-revision 409 is
  // a property of revision-tracking backends (SqlStateStore / the contract
  // double) and is pinned in-process; see the note on the version test.
  const replay = await storeOp(page.request, 'save', auth, {
    revision: version.body.revision,
    data: JSON.stringify(load.body),
  });
  expect(replay.status).toBe(200);
  expect(replay.body).toEqual({ ok: true });
});

test('AC2: an authenticated write-tier member is refused 403 and no op reaches the store', async ({
  page,
}) => {
  const admin = { Authorization: `Bearer ${await adminToken(page)}` };

  // Provision the persona on the admin's bearer (management surfaces are
  // role-gated, not session-gated — a bearer principal is a principal).
  // rbac-viewer creates this same account earlier in the run; the assign
  // probe below is the create-on-missing fallback that keeps this file
  // runnable standalone — and assignment is a /api/projects call, outside
  // the /api/auth rate window either way.
  const assigned = await page.request.post(`/api/projects/${PID}/members`, {
    data: { username: VIEWER_USER },
    headers: admin,
  });
  if (assigned.status() !== 200) {
    const made = await page.request.post('/api/auth/users', {
      data: {
        username: VIEWER_USER,
        password: VIEWER_PASSWORD,
        role: 'viewer',
      },
      headers: { ...admin, ...CLIENT_XFF },
    });
    expect(made.status(), 'viewer user upsert').toBe(200);
    const retried = await page.request.post(`/api/projects/${PID}/members`, {
      data: { username: VIEWER_USER },
      headers: admin,
    });
    expect(retried.status(), 'viewer assigned to the project').toBe(200);
  }
  // The refusal below must be the WRITE gate (manage bar), not the
  // membership branch: this persona IS on the project and still refused.

  // Sign in as the viewer; the session cookie in this file's request jar is
  // the credential (the browser/watchdog class, which DOES carry project
  // memberships — unlike bearer service tokens). The jar was cleared above
  // before provisioning, so the login below is the only session it holds.
  await page.context().clearCookies();
  const vlogin = await page.request.post('/api/auth/login', {
    data: { username: VIEWER_USER, password: VIEWER_PASSWORD },
    headers: CLIENT_XFF,
  });
  if (vlogin.status() !== 200) {
    // Usually the shared fixture's password drifted from the constant this
    // file carries (create_user upserts hash/role): re-assert OURS, then
    // sign in again. A throttled window also lands here — the recovery is
    // bounded to one re-assert, not a loop.
    const made = await page.request.post('/api/auth/users', {
      data: {
        username: VIEWER_USER,
        password: VIEWER_PASSWORD,
        role: 'viewer',
      },
      headers: { ...admin, ...CLIENT_XFF },
    });
    expect(made.status(), 'viewer password re-assert').toBe(200);
    const vlogin2 = await page.request.post('/api/auth/login', {
      data: { username: VIEWER_USER, password: VIEWER_PASSWORD },
      headers: CLIENT_XFF,
    });
    expect(vlogin2.status(), 'viewer should log in after re-assert').toBe(200);
  } else {
    expect(vlogin.status(), 'viewer should log in').toBe(200);
  }

  for (const op of ['load', 'save', 'claim_ticket', 'heartbeat']) {
    const refused = await storeOp(page.request, op);
    expect(refused.status, `write-tier member op=${op}`).toBe(403);
  }

  // The gate did not wedge: after the refused ops the admin bearer still
  // reads the project. (The untouched-state PROOF is the AC1 equality; this
  // is only the liveness check.)
  expect((await storeOp(page.request, 'load', admin)).status).toBe(200);
});

test('AC2: a manage-tier non-member is refused by the project-membership branch', async ({
  page,
}) => {
  const admin = { Authorization: `Bearer ${await adminToken(page)}` };

  // A lead-tier service token: role clears the manage bar, but bearer
  // principals carry no project memberships by design (FileAuthService and
  // SqlAuthService both resolve service tokens to `projects: []`), so the
  // per-project branch refuses it. Minted on the admin manage surface — one
  // POST, no second sign-in (the suite's shared rate window is tight).
  const minted = await page.request.post('/api/auth/tokens', {
    data: { label: `f285-lead-${Date.now()}`, role: 'techlead' },
    headers: { ...admin, ...CLIENT_XFF },
  });
  expect(minted.status(), 'lead-role token mint').toBe(200);
  const token = (await minted.json()).token;
  expect(token).toBeTruthy();

  // Only the lead bearer may speak for this call: drop any session cookie a
  // previous test's context left, so the cookie fallback cannot mask the
  // bearer's refusal.
  await page.context().clearCookies();
  const refused = await storeOp(page.request, 'load', {
    Authorization: `Bearer ${token}`,
  });
  expect(refused.status).toBe(403);
  expect(refused.body).toMatchObject({ error: 'not a member of this project' });
});

test('CXA-F350: a lead-tier member’s personal token inherits the member’s project memberships', async ({
  page,
}) => {
  const admin = { Authorization: `Bearer ${await adminToken(page)}` };

  // Provision the lead persona: an upserted TechLead account (idempotent
  // across boots — create_user upserts hash/role) assigned to the project.
  const made = await page.request.post('/api/auth/users', {
    data: { username: LEAD_USER, password: LEAD_PASSWORD, role: 'techlead' },
    headers: { ...admin, ...CLIENT_XFF },
  });
  expect(made.status(), 'lead user upsert').toBe(200);
  const assigned = await page.request.post(`/api/projects/${PID}/members`, {
    data: { username: LEAD_USER },
    headers: admin,
  });
  expect(assigned.status(), 'lead assigned to the project').toBe(200);

  // Mint the personal token exactly the way a runner operator does: sign in
  // as the member, POST /api/auth/my/tokens. The session cookie carries
  // this mint (bearer callers mint through the same endpoint).
  await page.context().clearCookies();
  const login = await page.request.post('/api/auth/login', {
    data: { username: LEAD_USER, password: LEAD_PASSWORD },
    headers: CLIENT_XFF,
  });
  expect(login.status(), 'lead should log in').toBe(200);
  const minted = await page.request.post('/api/auth/my/tokens', {
    data: { label: `f350-lead-${Date.now()}` },
    headers: CLIENT_XFF,
  });
  expect(minted.status(), 'lead personal token mint').toBe(200);
  const leadToken = (await minted.json()).token;
  expect(leadToken).toMatch(/^[0-9a-f]{64}$/);

  // The BEARER alone carries the membership: drop the session so the cookie
  // fallback cannot authenticate, then read the project's state.
  await page.context().clearCookies();
  const lead = { Authorization: `Bearer ${leadToken}` };
  const load = await storeOp(page.request, 'load', lead);
  expect(
    load.status,
    'a lead-tier member’s personal token passes the membership branch',
  ).toBe(200);
  expect(Array.isArray(load.body.tickets), 'op=load yields the ticket array')
    .toBe(true);

  // Live resolution: revoking the membership revokes the token's project
  // reach on the NEXT call, with no re-mint — the same secret is refused.
  const removed = await page.request.delete(
    `/api/projects/${PID}/members/${LEAD_USER}`,
    { headers: admin },
  );
  expect(removed.status(), 'membership removal').toBe(200);
  const revoked = await storeOp(page.request, 'load', lead);
  expect(revoked.status, 'the same token after unassignment').toBe(403);
  expect(revoked.body).toMatchObject({ error: 'not a member of this project' });

  // The gate did not wedge: the admin bearer still reads the project.
  expect((await storeOp(page.request, 'load', admin)).status).toBe(200);
});

test('AC3: a runner process with env credentials completes its store round-trip', async ({
  page,
}) => {
  const token = await adminToken(page);
  const auth = { Authorization: `Bearer ${token}` };
  // Same binary run-server-auth.sh boots the hub from (built relative to the
  // e2e dir, the documented invocation root).
  const bin = join(process.cwd(), '..', 'target', 'debug', 'coxagent');

  // Seed a state delta the report can only have received OVER THE WIRE: the
  // spawned process reads through make_store's RestStateStore, so without a
  // working remote path its local fallback store would print a DIFFERENT
  // ticket count and this test would fail.
  const base = await storeOp(page.request, 'load', auth);
  expect(base.status).toBe(200);
  // The id mirrors the fixture's own shape ('F001' — this project's alias is
  // empty), so the write-time validation accepts the clone.
  const probeTicket = {
    ...base.body.tickets[base.body.tickets.length - 1],
    id: 'F285',
    type: 'bug',
    status: 'open',
    title: 'CXA-F285 wire probe',
  };
  const seededCount = base.body.tickets.length + 1;
  const save = await storeOp(page.request, 'save', auth, {
    data: JSON.stringify({
      ...base.body,
      tickets: [...base.body.tickets, probeTicket],
    }),
  });
  expect(save.status, `seeding the wire probe failed: ${JSON.stringify(save.body)}`)
    .toBe(200);

  // The runner credential wiring under test: COXAGENT_REMOTE_STORE_URL +
  // COXAGENT_REMOTE_TOKEN -> make_store builds RestStateStore and every op
  // goes over /store (builders.rs). `--state-dir <tmp>/default/state` makes
  // the CLI derive project id `default` — the project this hub serves.
  const stateDir = await mkdtemp(join(tmpdir(), 'f285-runner-'));
  const env = { ...process.env };
  // Hermetic, like run-server-auth.sh: the runner must front the gateway, not
  // any ambient shared database; REMOTE-first precedence would hide a leaked
  // DSN, so remove the ambiguity at the source.
  delete env.COXAGENT_DB_DSN;
  delete env.COXAGENT_REDIS_URL;
  // And no ambient operator.token may answer for us: point the harvest at a
  // path that does not exist, so the negative half below can only fail
  // because the token really is missing.
  env.COXAGENT_TOKEN_FILE = join(stateDir, 'no-operator.token');
  env.COXAGENT_REMOTE_STORE_URL = 'http://127.0.0.1:4518';
  env.COXAGENT_REMOTE_TOKEN = token;

  const run = (extra) =>
    new Promise((resolve) => {
      execFile(
        bin,
        ['--state-dir', join(stateDir, 'default', 'state'), 'report'],
        { env: { ...env, ...extra }, timeout: 30_000 },
        (err, stdout, stderr) => resolve({ err, stdout, stderr }),
      );
    });

  const ok = await run({});
  expect(ok.err, `runner report should succeed: ${ok.stderr}`).toBeFalsy();
  expect(ok.stdout).toContain('=== COXAGENT STATE ===');
  // The seeded delta came back through the gateway — the local fallback
  // store would report one ticket fewer.
  expect(ok.stdout).toMatch(new RegExp(`tickets\\s*:\\s*${seededCount}`));

  // The negative half proves the env-wired runner class is actually gated:
  // without the token the same process is refused 401, and the adapter's
  // error names the exact remedy.
  const refused = await run({ COXAGENT_REMOTE_TOKEN: '' });
  expect(refused.err, 'runner without a token must fail').toBeTruthy();
  expect(refused.stderr).toContain('401');
  expect(refused.stderr).toContain('COXAGENT_REMOTE_TOKEN');
});
