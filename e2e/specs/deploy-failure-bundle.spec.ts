// CXA-F289: the deploy failure forensics bundle in the browser.
//
// A deliberately failing deploy (bad compose service) must leave, on the
// deploy attempt's record, a size-capped bundle — the stderr tail plus recent
// per-container logs — and the Overview deploy surface must show that bundle
// with the REAL compose error text, beside the rollback outcome, copyable in
// full, with secret-shaped strings masked.
//
// The suite never starts a real deploy, so the failed attempt is seeded in the
// exact persisted shape the cycle writes (`state.deploy.failure_bundle`,
// `state.last_rollback`) through the store-RPC surface the runners themselves
// use (load → mutate → save) — the liveness.spec.ts pattern. The compose error
// text is the REAL text `docker compose` prints for a service defined without
// an image or build context — the "bad compose service" this spec simulates.
// The one-line `summary` deliberately carries only the reduced form, so every
// assertion below discriminates the BUNDLE (rendered from the stderr tail),
// not the summary line.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

const base = process.env.E2E_BASE ?? 'http://127.0.0.1:4517';
const rpc = async (op, body) => {
  // Every store op rides the POST RPC surface (GET is the read-only audit).
  const r = await fetch(`${base}/api/projects/default/store?op=${op}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  if (!r.ok) throw new Error(`store op ${op}: ${r.status} ${await r.text()}`);
  return r.json();
};

// Seed (or clear) the failed deploy attempt + rollback outcome exactly as the
// cycle persists them. `null` restores the fixture's original shape.
const setFailedDeploy = async (deploy, rollback) => {
  const state = await rpc('load');
  state.deploy = deploy;
  if (rollback) state.last_rollback = rollback;
  else delete state.last_rollback;
  await rpc('save', { data: JSON.stringify(state) });
};

// The real compose error for a bad service (no image, no build context).
const COMPOSE_ERROR =
  'service "web" has neither an image nor a build context specified. At least one must be provided.';
// A secret-shaped line the capture must mask — an env-assigned credential.
const SECRET_VALUE = '9f86d081884c7d659a2feaa0c55ad015';
// Distinctive tail marker: proves the copy carries the FULL text, not a preview.
const TAIL_END = 'TAIL-END-MARKER-7f3a';

const stderrTail = [
  ' => ERROR [web internal] load build definition from Dockerfile',
  `validating docker-compose.yml: ${COMPOSE_ERROR}`,
  `PG_PASSWORD=${SECRET_VALUE}`,
  'exit status 1',
  TAIL_END,
].join('\n');

const failedAttempt = (bundle) => ({
  at: new Date().toISOString(),
  ok: false,
  summary: 'docker compose failed: exit 1',
  failure_bundle: bundle,
});

const rollbackOutcome = {
  at: new Date().toISOString(),
  reason: 'deploy failed',
  to_sha: 'abc1234def5678',
  ok: true,
  summary: 'rolled back to abc1234: running: web',
  stale: false,
  migration_blocked: false,
};

test('a failed deploy shows its bundle with the real compose error beside the rollback outcome', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await setFailedDeploy(
    failedAttempt({
      stderr_tail: stderrTail,
      container_logs: [{ service: 'db', tail: 'pg_ctl: could not start server' }],
      no_container_logs: false,
    }),
    rollbackOutcome,
  );
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  try {
    await openApp(page);

    // The bundle is visible: the real compose error text renders (the summary
    // only ever said "exit 1", so this discriminates the bundle).
    const surface = page.locator('body');
    await expect(surface).toContainText(COMPOSE_ERROR);
    // Per-container logs render, attributed to their service.
    await expect(surface).toContainText('pg_ctl: could not start server');

    // The rollback outcome is visible together with the bundle (AC2).
    await expect(surface).toContainText(/rolled back/i);

    // The secret never renders raw (AC4, render half).
    await expect(surface).not.toContainText(SECRET_VALUE);

    // Copy or download the bundle IN FULL (AC2): the copied text carries the
    // whole stderr tail — through the tail-end marker — and the container log,
    // still masked.
    const control = page.locator('button, a').filter({ hasText: /copy|download/i }).first();
    await expect(control).toBeVisible();
    await control.click();
    const copied = await page.evaluate(() => navigator.clipboard.readText());
    expect(copied).toContain(COMPOSE_ERROR);
    expect(copied).toContain(TAIL_END);
    expect(copied).toContain('pg_ctl: could not start server');
    expect(copied).not.toContain(SECRET_VALUE);

    await assertNoConsoleErrors(errors);
    await expect(page).toHaveScreenshot('deploy-failure-bundle.png');
  } finally {
    // Restore the world for the specs that run after this one.
    await setFailedDeploy(null, null);
  }
});

test('a compose failure before any container existed states that no container logs are available', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await setFailedDeploy(
    failedAttempt({
      stderr_tail: `validating docker-compose.yml: ${COMPOSE_ERROR}`,
      container_logs: [],
      no_container_logs: true,
    }),
    null,
  );
  try {
    await openApp(page);

    // The explicit statement — never an empty log section (AC3).
    const surface = page.locator('body');
    await expect(surface).toContainText(/no container logs available/i);
    await expect(surface).toContainText(COMPOSE_ERROR);

    await assertNoConsoleErrors(errors);
  } finally {
    await setFailedDeploy(null, null);
  }
});
