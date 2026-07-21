const { test, expect } = require('@playwright/test');

async function loginViaApi(page) {
  const resp = await page.request.post('http://localhost:4000/api/auth/login', {
    data: { username: 'root', password: 'Str@wb3rry' }
  });
  expect(resp.status()).toBe(200);
  const cookies = resp.headers()['set-cookie'];
  if (cookies) {
    await page.context().addCookies([{
      name: 'cox_session', value: cookies.split(';')[0].split('=')[1],
      domain: 'localhost', path: '/', httpOnly: true, sameSite: 'Strict'
    }]);
  }
}

async function initPage(page) {
  await page.context().clearCookies();
  await loginViaApi(page);
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await page.waitForFunction(() => {
    const el = document.getElementById('ub-name');
    return el && el.textContent && el.textContent.trim().length > 0;
  }, { timeout: 15000 });
  await page.evaluate(() => {
    localStorage.setItem('cox_mode', 'workspace');
    MODE = 'workspace';
    if (typeof applyMode === 'function') applyMode();
    if (typeof updateSegments === 'function') updateSegments();
  });
  await page.waitForTimeout(500);
}

test.describe('CoXAgent UI', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('user badge shows root super', async ({ page }) => {
    await expect(page.locator('#ub-name')).toContainText('root');
    await expect(page.locator('#ub-role')).toContainText('super');
  });

  test('nav overview visible', async ({ page }) => {
    await page.click('a[data-v="overview"]');
    await page.waitForTimeout(500);
    await expect(page.locator('#view-overview')).toBeVisible();
  });

  test('nav settings visible', async ({ page }) => {
    await page.click('a[data-v="settings"]');
    await page.waitForTimeout(500);
    await expect(page.locator('#view-settings')).toBeVisible();
  });

  test('nav team visible', async ({ page }) => {
    await page.click('a[data-v="team"]');
    await page.waitForTimeout(500);
    await expect(page.locator('#view-team')).toBeVisible();
  });

  test('nav board visible', async ({ page }) => {
    await page.click('a[data-v="board"]');
    await page.waitForTimeout(500);
    await expect(page.locator('#view-board')).toBeVisible();
  });

  test('chat mode toggle', async ({ page }) => {
    await page.click('#mode-chat');
    await page.waitForTimeout(500);
    await expect(page.locator('#chat-side')).toBeVisible();
    await page.click('#mode-ws');
    await page.waitForTimeout(500);
    await expect(page.locator('#ws-nav')).toBeVisible();
  });

  test('manage spaces API accessible for super', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/manage/overview');
      return r.json();
    });
    expect(data.spaces.length).toBeGreaterThan(0);
    expect(data.totals).toBeDefined();
  });

  test('api auth/me returns super role', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/auth/me');
      return r.json();
    });
    expect(data.role).toBe('super');
  });

  test('api spaces returns space list', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/spaces');
      return r.json();
    });
    expect(Array.isArray(data.spaces || data)).toBe(true);
  });

  test('home view has ws-hero element', async ({ page }) => {
    const exists = await page.locator('#ws-hero').count();
    expect(exists).toBeGreaterThan(0);
  });
});
