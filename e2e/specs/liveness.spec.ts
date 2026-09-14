// Loop-liveness watchdog (CXA-F259): the team panel surfaces the hub
// watchdog's open stall episode — a quiet team renders nothing, a stalled
// loop gets one red chip naming the silent worker. Console-clean like every
// view (the shared console gate).
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

// The episode is seeded through the store-RPC surface the runners themselves
// use (load → mutate → save) — no new API, and the seeded shape is exactly
// the persisted StallEpisode the watchdog writes.
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

const setEpisode = async (episode) => {
  const state = await rpc('load');
  if (episode) state.liveness = episode;
  else delete state.liveness;
  await rpc('save', { data: JSON.stringify(state) });
};

test('a quiet team renders no liveness chip', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await setEpisode(null);
  await openApp(page);
  await page.evaluate(() => { (window as any).nav('team'); });
  // Healthy = the container stays empty: no chip, no noise.
  await expect(page.locator('#team-liveness')).toHaveText('');
  await assertNoConsoleErrors(errors);
});

test('an open stall episode renders the red chip naming the worker, then clears', async ({
  page,
}) => {
  const errors = [];
  armConsoleGate(page, errors);
  const now = new Date().toISOString();
  await setEpisode({
    since: now,
    worker: 'e2e@runner',
    last_activity_at: now,
    last_alert_at: now,
    escalations: 0,
  });
  try {
    await openApp(page);
    await page.evaluate(() => { (window as any).nav('team'); });
    // The 1 Hz state stream re-renders the team panel with the episode.
    const chip = page.locator('.lv-chip');
    await expect(chip).toContainText('loop stalled');
    await expect(chip).toContainText('e2e@runner');
    await assertNoConsoleErrors(errors);
  } finally {
    // Restore the world for the specs that run after this one.
    await setEpisode(null);
  }
  await expect(page.locator('#team-liveness')).toHaveText('');
});
