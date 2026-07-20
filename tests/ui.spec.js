const { test, expect } = require('@playwright/test');

async function loginViaApi(page) {
  // Get session cookie via API
  const resp = await page.request.post('http://localhost:4000/api/auth/login', {
    data: { username: 'root', password: 'Str@wb3rry' }
  });
  expect(resp.status()).toBe(200);
  const cookies = resp.headers()['set-cookie'];
  if (cookies) {
    const sessionCookie = cookies.split(';')[0];
    await page.context().addCookies([{
      name: 'cox_session',
      value: sessionCookie.split('=')[1],
      domain: 'localhost',
      path: '/',
      httpOnly: true,
      sameSite: 'Strict'
    }]);
  }
}

async function initPage(page) {
  await page.context().clearCookies();
  await loginViaApi(page);
  await page.goto('/', { waitUntil: 'networkidle' });
  // Wait for boot to complete
  await page.waitForFunction(() => {
    const el = document.getElementById('ub-name');
    return el && el.textContent && el.textContent.trim().length > 0;
  }, { timeout: 15000 });
  // Force workspace mode
  await page.evaluate(() => {
    localStorage.setItem('cox_mode', 'workspace');
    if (typeof applyMode === 'function') applyMode();
    if (typeof updateSegments === 'function') updateSegments();
  });
  await page.waitForTimeout(500);
}

test.describe('CoXAgent UI', () => {
  test.beforeEach(async ({ page }) => {
    await initPage(page);
  });

  test('user badge shows root super', async ({ page }) => {
    await expect(page.locator('#ub-name')).toContainText('root');
    await expect(page.locator('#ub-role')).toContainText('super');
  });

  test('sidebar shows Administration section', async ({ page }) => {
    await expect(page.locator('#adm-section')).toBeVisible({ timeout: 3000 });
    await expect(page.locator('#adm-nav')).toBeVisible();
    await expect(page.locator('#adm-ws')).toContainText('Manage workspace');
  });

  test('manage workspace opens space cards', async ({ page }) => {
    await page.locator('#adm-ws').click();
    await page.waitForSelector('#ws-admin:not([hidden])', { timeout: 5000 });
    await expect(page.locator('#sp-cards')).toBeVisible();
  });

  test('space cards have content', async ({ page }) => {
    await page.locator('#adm-ws').click();
    await page.waitForTimeout(1500);
    const count = await page.locator('.wscard.mgcard').count();
    expect(count).toBeGreaterThan(0);
  });

  test('search filters spaces', async ({ page }) => {
    await page.locator('#adm-ws').click();
    await page.waitForTimeout(1500);
    const before = await page.locator('.wscard.mgcard').count();
    expect(before).toBeGreaterThan(0);
    await page.locator('#sp-search').fill('zzznomatch');
    await page.waitForTimeout(500);
    const after = await page.locator('.wscard.mgcard').count();
    expect(after).toBe(0);
  });

  test('nav overview visible', async ({ page }) => {
    await page.locator('a[data-v="overview"]').click();
    await page.waitForTimeout(500);
    await expect(page.locator('#view-overview')).toBeVisible();
  });

  test('nav settings visible', async ({ page }) => {
    await page.locator('a[data-v="settings"]').click();
    await page.waitForTimeout(500);
    await expect(page.locator('#view-settings')).toBeVisible();
  });

  test('nav team visible', async ({ page }) => {
    await page.locator('a[data-v="team"]').click();
    await page.waitForTimeout(500);
    await expect(page.locator('#view-team')).toBeVisible();
  });

  test('nav board visible', async ({ page }) => {
    await page.locator('a[data-v="board"]').click();
    await page.waitForTimeout(500);
    await expect(page.locator('#view-board')).toBeVisible();
  });

  test('api manage/overview returns spaces', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/manage/overview');
      return r.json();
    });
    expect(data.spaces).toBeDefined();
    expect(data.spaces.length).toBeGreaterThan(0);
  });
});
