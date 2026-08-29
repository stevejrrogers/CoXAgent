// CXA-F238: the brake cockpit API — RBAC, bounded holds, audit visibility.
//
// API-level spec (no UI chrome): the cockpit is a read model plus two
// operator verbs. Runs against the auth-enabled server (specs-auth webServer)
// because the whole point of half the criteria is WHO may steer the brakes:
// viewers read, writers hold, expiry bounds everything.
import { test, expect } from '@playwright/test';
import { ADMIN_USER, ADMIN_PASSWORD, apiLogin } from './helpers-auth.mjs';

const VIEWER_USER = 'brakeviewere2e';
const VIEWER_PASSWORD = 'BrakeView_12345';
const futureStamp = (ms) => new Date(Date.now() + ms).toISOString();

async function loginAsAdmin(page) {
  const admin = await apiLogin(page.request);
  expect(admin.status).toBe(200);
  await page.context().addCookies([
    { name: 'cox_session', value: admin.cookie, domain: '127.0.0.1', path: '/' },
  ]);
}

test.describe('brake cockpit', () => {
  // Every test gets a fresh browser context in Playwright, so every test
  // establishes its own admin session (cheap API login).
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test('the cockpit read model exposes cards, signals, thresholds and holds', async ({
    page,
  }) => {
    const res = await page.request.get('/api/projects/default/brakes');
    expect(res.status()).toBe(200);
    const body = await res.json();
    expect(body.cards).toHaveLength(2);
    const quality = body.cards.find((c) => c.id === 'quality');
    const intake = body.cards.find((c) => c.id === 'intake');
    for (const card of [quality, intake]) {
      expect(card).toBeTruthy();
      expect(typeof card.effectiveValue).toBe('boolean');
      expect(['auto', 'held-freeze', 'overridden']).toContain(card.mode);
      expect(typeof card.autoWouldBe).toBe('boolean');
    }
    expect(quality.fieldName).toBe('bugs_first');
    expect(intake.fieldName).toBe('skip_ba');
    expect(typeof body.inputs.churnPerShip).toBe('number');
    expect(typeof body.inputs.shippedLast7Days).toBe('number');
    expect(typeof body.inputs.backlog).toBe('number');
    expect(body.thresholds).toEqual({
      churnOn: 1.5,
      churnOff: 0.8,
      backlogOn: 25,
      backlogOff: 12,
    });
    expect(Array.isArray(body.activeHolds)).toBe(true);
    expect(Array.isArray(body.history)).toBe(true);
    expect(typeof body.asOf).toBe('string');
  });

  test('a write-role operator can set a bounded hold, see it, and clear it', async ({
    page,
  }) => {
    const set = await page.request.post('/api/projects/default/brakes/bugs_first/hold', {
      data: {
        pinnedValue: true,
        reason: 'e2e: pin quality brake to verify the cockpit',
        expiresAt: futureStamp(3_600_000),
      },
    });
    expect(set.status()).toBe(200);

    const after = await (await page.request.get('/api/projects/default/brakes')).json();
    const quality = after.cards.find((c) => c.id === 'quality');
    expect(quality.mode).toBe('overridden');
    expect(quality.effectiveValue).toBe(true);
    expect(after.activeHolds).toHaveLength(1);
    expect(after.activeHolds[0].brake).toBe('bugs_first');
    expect(after.activeHolds[0].actor).toBe(ADMIN_USER);
    expect(after.activeHolds[0].reason).toContain('e2e: pin quality brake');
    // The intervention is in the audit trail for viewers to inspect.
    expect(
      after.history.some(
        (e) => e.source === 'hold' && e.brake === 'bugs_first' && e.actor === ADMIN_USER,
      ),
    ).toBe(true);

    const cleared = await page.request.delete('/api/projects/default/brakes/bugs_first/hold');
    expect(cleared.status()).toBe(200);
    const restored = await (await page.request.get('/api/projects/default/brakes')).json();
    expect(restored.cards.find((c) => c.id === 'quality').mode).toBe('auto');
    expect(restored.activeHolds).toHaveLength(0);

    // Clearing again is honestly 404 — there is no hold left to clear.
    const again = await page.request.delete('/api/projects/default/brakes/bugs_first/hold');
    expect(again.status()).toBe(404);
  });

  test('a hold is always bounded: bad input is refused, not stored', async ({ page }) => {
    const noReason = await page.request.post('/api/projects/default/brakes/bugs_first/hold', {
      data: { pinnedValue: null, reason: '   ', expiresAt: futureStamp(3_600_000) },
      failOnStatusCode: false,
    });
    expect(noReason.status()).toBe(400);
    const pastExpiry = await page.request.post('/api/projects/default/brakes/bugs_first/hold', {
      data: { pinnedValue: null, reason: 'x', expiresAt: futureStamp(-3_600_000) },
      failOnStatusCode: false,
    });
    expect(pastExpiry.status()).toBe(400);
    const unparsable = await page.request.post('/api/projects/default/brakes/bugs_first/hold', {
      data: { pinnedValue: null, reason: 'x', expiresAt: 'next tuesday' },
      failOnStatusCode: false,
    });
    expect(unparsable.status()).toBe(400);
    const unknownBrake = await page.request.post('/api/projects/default/brakes/burn_mode/hold', {
      data: { pinnedValue: true, reason: 'x', expiresAt: futureStamp(3_600_000) },
      failOnStatusCode: false,
    });
    expect(unknownBrake.status()).toBe(400);
    const body = await (await page.request.get('/api/projects/default/brakes')).json();
    expect(body.activeHolds).toHaveLength(0);
  });

  test('a viewer may inspect the cockpit but never steer it', async ({ page }) => {
    // Provision the viewer through the admin session (idempotent, like
    // rbac-viewer.spec.ts) and make them a project member so the per-project
    // membership gate is the thing under test, not the member list.
    await page.request.post('/api/auth/users', {
      data: { username: VIEWER_USER, password: VIEWER_PASSWORD, role: 'viewer' },
    });
    await page.request.post('/api/projects/default/members', {
      data: { username: VIEWER_USER },
    });

    await page.context().clearCookies();
    const vlogin = await page.request.post('/api/auth/login', {
      data: { username: VIEWER_USER, password: VIEWER_PASSWORD },
    });
    expect(vlogin.status()).toBe(200);

    const read = await page.request.get('/api/projects/default/brakes');
    expect(read.status()).toBe(200);

    const steer = await page.request.post('/api/projects/default/brakes/skip_ba/hold', {
      data: { pinnedValue: false, reason: 'viewer must not steer', expiresAt: futureStamp(3_600_000) },
      failOnStatusCode: false,
    });
    expect(steer.status()).toBe(403);
    const clear = await page.request.delete('/api/projects/default/brakes/skip_ba/hold', {
      failOnStatusCode: false,
    });
    expect(clear.status()).toBe(403);
    const body = await (await page.request.get('/api/projects/default/brakes')).json();
    expect(body.activeHolds).toHaveLength(0);
  });

  test('an expired hold stops riding on the brakes at read time', async ({ page }) => {
    // Set a hold with a 1.5s bound, then read AFTER it elapsed.
    const set = await page.request.post('/api/projects/default/brakes/skip_ba/hold', {
      data: { pinnedValue: true, reason: 'e2e: expiry bound', expiresAt: futureStamp(1_500) },
    });
    expect(set.status()).toBe(200);
    await new Promise((r) => setTimeout(r, 2_000));
    const body = await (await page.request.get('/api/projects/default/brakes')).json();
    expect(body.activeHolds).toHaveLength(0);
    expect(body.cards.find((c) => c.id === 'intake').mode).toBe('auto');
  });
});
