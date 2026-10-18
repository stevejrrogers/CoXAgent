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

// CXA-B131 regression: the Work log panel is embedded in the page DOM on
// every view, but only openAgent() ever painted a terminal state into it.
// Visiting Transcripts & alerts and never opening the drawer left the panel
// on its bare 'loading…' placeholder forever — no stream, no error, no
// empty state. The activity render path must land it on a terminal state.
test('the activity view never leaves the work log panel on loading… without the drawer', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  // Exactly the deployed-hub repro: navigate to Transcripts & alerts and
  // give any phantom work a moment — the panel must already be terminal.
  await page.evaluate(() => {
    AGENT_LOG_ROLE = null;
    AGENT_LOG_WORKER = '';
    nav('activity');
  });
  const body = page.locator('#agent-transcript');
  await expect(body).toContainText("hasn't run yet", { timeout: 5000 });
  await expect(body).not.toContainText('loading…');
  // No stream was started behind the closed drawer, so neither badge lies.
  await expect(page.locator('#agent-live-badge')).toBeHidden();
  await expect(page.locator('#agent-err-badge')).toBeHidden();
  await assertNoConsoleErrors(errors);
});

// CXA-B131 guard: the idle painter must never fight an ENGAGED drawer.
// renderActive() fires on every state snapshot, so while the agent drawer is
// open the activity branch runs constantly — a painter without the role guard
// would wipe a live log back to the empty state on the next snapshot.
test('the idle painter leaves an engaged drawer panel untouched', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  // Simulate an engaged drawer (the state openAgent leaves behind: role set,
  // panel owned by the stream) without a live socket, then re-render the
  // activity view exactly as an arriving state snapshot would.
  await page.evaluate(() => {
    AGENT_LOG_ROLE = 'dev';
    AGENT_LOG_WORKER = '';
    document.getElementById('agent-transcript')!.innerHTML =
      '<div class="wl-item wl-line">sentinel log line</div>';
    nav('activity');
  });
  await expect(page.locator('#agent-transcript')).toContainText('sentinel log line');

  // Drawer released: the same painter now normalises the panel again.
  await page.evaluate(() => {
    AGENT_LOG_ROLE = null;
    (window as unknown as { paintAgentLogIdle: () => void }).paintAgentLogIdle();
  });
  await expect(page.locator('#agent-transcript')).toContainText("hasn't run yet");
  await expect(page.locator('#agent-transcript')).not.toContainText('sentinel log line');
  await assertNoConsoleErrors(errors);
});
