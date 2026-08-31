// COX-B081 regression: the live-transcript SSE stream must surface a
// connection failure in the UI, not hang silently. Before the fix,
// `es.onerror` just closed the EventSource and there was no error badge in
// the DOM at all — this test fails on that code (`#agent-err-badge` never
// exists/never becomes visible) and passes once the fix's retry+badge logic
// ships.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

// No console-error gate here: the test deliberately 404s a request, which
// the browser itself logs as a resource-load error — that's the fault
// injection working, not an app bug to catch.
test('a dead agent-log stream shows the error state, then recovers to the empty state', async ({ page }) => {
  await openApp(page);

  // Simulate the failure modes the ticket calls out: an old server that 404s
  // the stream endpoint, or a connection that drops mid-stream. Either way
  // the browser's EventSource fires `error`.
  await page.route('**/agent-log/stream**', (route) => route.fulfill({ status: 404, body: 'not found' }));

  await page.evaluate(() => {
    // `AGENT_LOG_ROLE`/`AGENT_LOG_WORKER` are top-level `let`s in a classic
    // script — reachable as bare identifiers from evaluate, not as
    // `window.*` (see sprint.spec.ts). `restartAgentLog` is a top-level
    // `function` declaration, so it IS on `window`.
    AGENT_LOG_ROLE = 'dev';
    AGENT_LOG_WORKER = '';
    document.getElementById('ov-agent')!.classList.add('open');
    (window as unknown as { restartAgentLog: () => void }).restartAgentLog();
  });

  const badge = page.locator('#agent-err-badge');
  await expect(badge).toBeVisible({ timeout: 5000 });
  await expect(badge).toContainText(/STREAM LOST|RECONNECTING/);
  // CXA-B128: the panel BODY must carry the failure too — with nothing
  // buffered it may not sit on the indefinite 'loading…' placeholder.
  await expect(page.locator('#agent-transcript')).toContainText('connection lost');

  // Recovery: with the fault lifted and the stream reconnected, the init
  // kick-off must repaint the panel back to its terminal empty state — a
  // recovered stream may not leave the error state stuck on screen.
  await page.unroute('**/agent-log/stream**');
  await page.evaluate(() => {
    (window as unknown as { restartAgentLog: () => void }).restartAgentLog();
  });
  await expect(page.locator('#agent-transcript')).toContainText("hasn't run yet", { timeout: 5000 });
  await expect(badge).toBeHidden();

  // Tear down the retry loop and stream route before the test ends so it
  // can't bleed a background retry/backoff timer into the next test.
  await page.evaluate(() => (window as unknown as { stopAgentLog: () => void }).stopAgentLog());
});

// CXA-B128 regression: the Work-log panel (Work log ● LIVE in the agent
// modal) must reach a terminal state, never sit on its indefinite
// 'loading…' placeholder. The fixture ships no logs/live/<role>.log, so the
// stream used to answer a healthy connection with NO events at all — no
// init kick-off, no error — and the panel hung on 'loading…' forever.
test('the work log opens into a terminal state, never stuck on loading…', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  // Open the modal the same way the agent card's click handler does, and
  // await the async openAgent so any throw fails HERE, not as a confusing
  // 'loading…' assertion failure below.
  await page.evaluate(async () => { await openAgent('DEV-FEATURE'); });
  const body = page.locator('#agent-transcript');
  // The stream (healthy, empty file) must land on the honest empty state.
  await expect(body).toContainText("hasn't run yet", { timeout: 5000 });
  await expect(body).not.toContainText('loading…');
  // And it must not pretend a run is live.
  await expect(page.locator('#agent-live-badge')).toBeHidden();
  await assertNoConsoleErrors(errors);

  // Tear down the stream so the retry timer can't bleed into the next test.
  await page.evaluate(() => {
    (window as unknown as { closeAgent: () => void }).closeAgent();
  });
});
