// Overview KPI depth (CXA-F360): every tile carries a 14-day sparkline and a
// signed delta vs the prior 14 days; a tile at 0 with no history shows a
// what-fills-this hint instead of a bare 0.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('each overview tile shows a sparkline or a zero-state hint, console-clean', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const kpis = page.locator('#kpis');
  for (const label of ['Shipped', 'In flight', 'Documented', 'Releases', 'Cost']) {
    await expect(kpis).toContainText(label);
  }
  await expect(kpis).toContainText('vs prior 14d');
  // The frozen fixture has no release history, so the Shipped tile permanently
  // sits in its zero state — the hint must be there, not a bare 0.
  await expect(kpis).toContainText('ships land here when a ticket reaches documented');
  // Every tile charted OR hinted — never a bare number without context.
  const shaped = await page.$$eval('#kpis .kpi', els =>
    els.map(el => ({
      spark: !!el.querySelector('svg.spark'),
      hint: !!el.querySelector('.vhint'),
    })),
  );
  expect(shaped).toHaveLength(5);
  for (const t of shaped) expect(t.spark || t.hint, 'tile has spark or hint').toBe(true);
  await assertNoConsoleErrors(errors);
});

test('series math: deltas and sparklines from synthetic state (pure functions)', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page); // boots the real bundle; the evaluate below runs its actual code
  const html = await page.evaluate(() => {
    const iso = (daysAgo: number) => new Date(Date.now() - daysAgo * 864e5).toISOString();
    const day = (daysAgo: number) => iso(daysAgo).slice(0, 10);
    const s = {
      tickets: [
        { id: 'F1', type: 'feature', status: 'done', created_at: iso(20) },
        { id: 'F2', type: 'feature', status: 'documented', created_at: iso(4) },
        { id: 'F3', type: 'feature', status: 'documented', created_at: iso(5) },
        { id: 'B1', type: 'bug', status: 'fixed', created_at: iso(2) },
      ],
      history: [
        { version: '0.1.0', ticket: 'F1', title: 'a', at: iso(20) },
        { version: '0.2.0', ticket: 'F2', title: 'b', at: iso(4) },
        { version: '0.3.0', ticket: 'F3', title: 'd', at: iso(3) },
        { version: '0.4.0', ticket: 'B1', title: 'c', at: iso(2) },
      ],
      spend: { total_cost_usd: 9.5 },
      spend_history: [{ day: day(1), usd: 2.0 }, { day: day(20), usd: 1.0 }],
      spend_day: day(0),
      spend_today_usd: 1.0,
    };
    return (window as any).overviewKpis(s);
  });
  // 5 tiles, all charted (no hints — every stream has events).
  expect((html.match(/<svg class="spark"/g) || [])).toHaveLength(5);
  expect(html).not.toContain('vhint');
  // Shipped: feature ships 2 in the current window vs 1 in the prior one.
  expect(html).toContain('<span class="kd up">+1</span>');
  // In flight: 3 filed in the window vs 1 prior.
  expect(html).toContain('<span class="kd up">+2 filed</span>');
  // Releases: 3 ships in the window (2 features + 1 bug) vs 1 prior.
  expect(html).toContain('<span class="kd up">+2</span>');
  // Cost: window spend 2.0 + 1.0 today = 3.0 vs 1.0 prior; neutral colour.
  expect(html).toContain('<span class="kd">+$2.00</span>');
  // Every tile states its window.
  expect((html.match(/vs prior 14d/g) || [])).toHaveLength(5);
  // Sparkline is themed SVG: the stroke resolves through var(--accent2).
  const stroke = await page.evaluate(() => {
    const el = document.createElement('div');
    el.innerHTML = (window as any).overviewKpis({ tickets: [], history: [{ version: '0.1.0', ticket: 'F1', title: 'a', at: new Date().toISOString() }], spend: {} });
    document.body.appendChild(el);
    const line = el.querySelector('.spark-l');
    const c = line ? getComputedStyle(line).stroke : '';
    el.remove();
    return c;
  });
  expect(stroke).not.toBe('');
  expect(stroke).not.toBe('none');
  await assertNoConsoleErrors(errors);
});

test('delta signs: zero, negative-count and negative-money deltas render their classes', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const html = await page.evaluate(() => {
    const iso = (daysAgo: number) => new Date(Date.now() - daysAgo * 864e5).toISOString();
    const day = (daysAgo: number) => iso(daysAgo).slice(0, 10);
    // One ticket shipped once in each window (delta ±0), filed only in the
    // prior one (filings delta -1), spend only in the prior one (-$2.00).
    return (window as any).overviewKpis({
      tickets: [{ id: 'F1', type: 'feature', status: 'done', created_at: iso(20) }],
      history: [
        { version: '0.1.0', ticket: 'F1', title: 'a', at: iso(20) },
        { version: '0.2.0', ticket: 'F1', title: 'a', at: iso(4) },
      ],
      spend: { total_cost_usd: 2.0 },
      spend_history: [{ day: day(20), usd: 2.0 }],
    });
  });
  // Shipped: 1 ship in each window — no movement.
  expect(html).toContain('<span class="kd z">±0</span>');
  // In flight: 0 filings in the window vs 1 prior.
  expect(html).toContain('<span class="kd dn">-1 filed</span>');
  // Cost: nothing spent in the window vs $2.00 prior; stays neutral.
  expect(html).toContain('<span class="kd">-$2.00</span>');
  await assertNoConsoleErrors(errors);
});

test('all-zero state: hints replace the bare zeros', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const html = await page.evaluate(() => (window as any).overviewKpis({ tickets: [], history: [], spend: {} }));
  expect((html.match(/class="vhint"/g) || [])).toHaveLength(5);
  expect(html).not.toContain('svg');
  for (const hint of [
    'ships land here when a ticket reaches documented',
    'work lands here when a ticket is readied for an agent',
    'docs land here when DOCS documents a shipped ticket',
    'releases land here when a ticket ships',
    'cost accrues here as agent runs burn tokens',
  ]) {
    expect(html).toContain(hint);
  }
  await assertNoConsoleErrors(errors);
});
