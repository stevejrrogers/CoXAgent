#!/usr/bin/env node
// codemap visual test — maps the code dependency graph
const { test, expect } = require('@playwright/test');
const { ADMIN_USER, ADMIN_PASS } = require('./helpers/creds');

const BASE = 'http://localhost:4000';

async function login(page) {
  await page.goto(BASE + '/');
  await page.waitForSelector('#lg-user', { timeout: 8000 });
  await page.fill('#lg-user', ADMIN_USER);
  await page.fill('#lg-pass', ADMIN_PASS);
  await page.click('button:has-text("Sign in")');
  await page.waitForSelector('.side', { timeout: 10000 });
}

test('code map renders with graph', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('#codemap-body', { timeout: 15000 });

  await expect(page.locator('#cmt-map.on')).toBeVisible();

  const stats = page.locator('.cg-stats');
  await expect(stats).toBeVisible();
  const txt = await stats.textContent();
  expect(txt).toContain('336');
  expect(txt).toMatch(/\d{4}/); // 6542

  const langs = page.locator('.cg-lang');
  expect(await langs.count()).toBeGreaterThanOrEqual(1);

  // Dependency graph SVG
  const svg = page.locator('.cg-svg');
  await expect(svg).toBeVisible({ timeout: 12000 });

  const nodes = svg.locator('.cgn');
  expect(await nodes.count()).toBeGreaterThan(10);

  // Hubs
  const hubs = page.locator('.cg-hubs');
  await expect(hubs).toBeVisible();
  expect(await hubs.locator('.cg-hub').count()).toBeGreaterThanOrEqual(1);

  // Legend
  const legs = page.locator('.cg-leg');
  expect(await legs.count()).toBeGreaterThanOrEqual(1);

  // Top files
  expect(await page.locator('.cg-file').count()).toBeGreaterThan(5);
});

test('graph node click shows info', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('.cg-svg', { timeout: 15000 });

  // Force-click a node (labels may intercept pointer events)
  await page.locator('.cgn').nth(2).click({ force: true });

  await expect(page.locator('.cg-card')).toBeVisible({ timeout: 3000 });

  // Dismiss
  await page.locator('.cg-svg').click({ position: { x: 5, y: 5 } });
  await expect(page.locator('.cg-card')).not.toBeVisible({ timeout: 2000 });
});

test('symbol search works', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('.cg-svg', { timeout: 15000 });

  await expect(page.locator('#cg-q')).toBeVisible({ timeout: 5000 });
  await page.fill('#cg-q', 'lex');
  await page.waitForTimeout(800);

  const results = page.locator('.cg-sym');
  expect(await results.count()).toBeGreaterThan(0);
  await expect(results.first()).toBeVisible();
});

test('files tab shows file tree', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('#codemap-body', { timeout: 15000 });

  await page.click('#cmt-files');
  await expect(page.locator('#cmt-files.on')).toBeVisible();
  await page.waitForSelector('#ws-panel:not([hidden])', { timeout: 8000 });

  // File browser uses wslist > wsrow
  const rows = page.locator('#ws-panel .wslist .wsrow');
  await expect(rows.first()).toBeVisible({ timeout: 8000 });
  expect(await rows.count()).toBeGreaterThan(3);
});

test('zoom and pan works', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('.cg-svg', { timeout: 15000 });

  const svg = page.locator('.cg-svg');
  const box = await svg.boundingBox();

  const initial = await svg.evaluate(el => {
    const g = el.querySelector('.cg-view');
    return g.getAttribute('transform');
  });

  // Zoom in
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, -200);

  const zoomed = await svg.evaluate(el => {
    const g = el.querySelector('.cg-view');
    return g.getAttribute('transform');
  });
  expect(zoomed).not.toBe(initial);
});
