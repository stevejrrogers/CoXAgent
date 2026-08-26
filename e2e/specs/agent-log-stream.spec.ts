// COX-B081 regression: the live-transcript SSE stream must surface a
// connection failure in the UI, not hang silently. Before the fix,
// `es.onerror` just closed the EventSource and there was no error badge in
// the DOM at all — this test fails on that code (`#agent-err-badge` never
// exists/never becomes visible) and passes once the fix's retry+badge logic
// ships.
import { test, expect } from '@playwright/test';
import { openApp } from './helpers.mjs';

// No console-error gate here: the test deliberately 404s a request, which
// the browser itself logs as a resource-load error — that's the fault
// injection working, not an app bug to catch.
test('killing the agent-log stream shows an error badge within 5s', async ({ page }) => {
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

  // Tear down the retry loop and stream route before the test ends so it
  // can't bleed a background retry/backoff timer into the next test.
  await page.evaluate(() => (window as unknown as { stopAgentLog: () => void }).stopAgentLog());
  await page.unroute('**/agent-log/stream**');
});
