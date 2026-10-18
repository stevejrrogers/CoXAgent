// Shared helpers: console-error gate + navigation that waits for the app.
import { expect } from '@playwright/test';

/// Fail the test on any console error or page crash — a refactor that keeps
/// pixels but breaks a module load shows up here, not in the screenshot.
export function armConsoleGate(page, errors) {
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('pageerror', (err) => errors.push(String(err)));
}

export async function assertNoConsoleErrors(errors) {
  expect(errors, `console errors:\n${errors.join('\n')}`).toEqual([]);
}

/// Wait until the app's classic-script STATE global actually holds the fixture
/// tickets. The snapshot arrives over the 1 Hz SSE stream AFTER load, so a
/// spec that interacts immediately races it; this is the hydration gate every
/// openApp must pass before handing control to a spec. Both suites (open and
/// auth) hydrate over the same stream, so the wait is defined here and shared.
export async function awaitStateSnapshot(page) {
  try {
    await page.waitForFunction(
      () => {
        try {
          /* eslint-disable no-undef */
          return typeof STATE !== 'undefined' && Array.isArray(STATE.tickets) && STATE.tickets.length > 0;
        } catch {
          return false;
        }
      },
      null,
      { timeout: 15000 },
    );
  } catch {
    // Name the unhydrated app state (CXA-F315 AC5): a bare function timeout
    // reads as an unrelated spec bug deep inside the interactions.
    throw new Error(
      'fixture readiness failed: the state snapshot never arrived — app STATE holds no tickets ' +
        'after 15s, so the app is unhydrated (the fixture server did not deliver the SSE snapshot)',
    );
  }
}

/// Open the app and wait until it has painted real content AND hydrated the
/// fixture state.
export async function openApp(page) {
  // Pin a runnable engine before first paint. On load, CoXAgent asks
  // /api/engines what agent CLIs this machine has; a bare runner (Linux CI,
  // this repo's docker images) has none, which pops the "Connect a coding
  // agent" modal over every click and blocks the whole suite. Screenshots must
  // not depend on which host renders them, so report one engine: wizard
  // suppressed, pixels identical on darwin and linux alike.
  await page.route('**/api/engines', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([
        { name: 'opencode', path: '/usr/local/bin/opencode', where: 'hub' },
      ]),
    }),
  );
  // Same determinism rule for the go-live preflight (CXA-F239): its line
  // items probe THIS host (docker, free ports, accounts), so the Overview
  // panel would otherwise render differently per machine. Freeze it to a
  // healthy report; the real endpoint is covered by the Rust contract tests.
  await page.route('**/api/projects/*/preflight', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        ready: true,
        blocking: [],
        items: [
          { id: 'config', label: 'coxagent.json', status: 'ok', detail: 'parses; policy gates intact' },
          { id: 'engine.default', label: 'Engine · default', status: 'ok', detail: 'opencode · mock/model ready' },
          { id: 'host_port', label: 'Deploy host_port', status: 'ok', detail: 'assigned to 8101' },
          { id: 'publish', label: 'Publish port', status: 'ok', detail: '8101 is free to publish on this host' },
          { id: 'auth', label: 'Auth & accounts', status: 'ok', detail: 'RBAC enabled — 1 account(s) provisioned' },
          { id: 'docker', label: 'Docker', status: 'ok', detail: 'docker CLI present, daemon up' },
          { id: 'compose', label: 'Docker compose', status: 'ok', detail: 'docker compose answers (CLI + plugin present)' },
        ],
      }),
    }),
  );
  await page.goto('/');
  // networkidle can NEVER fire once the SSE stream holds its connection open,
  // so it is bounded and tolerated — the snapshot wait below subsumes what it
  // was for (the stream opens as soon as the app connects; if idle wins the
  // race it fires first, exactly as before, just without unbounded waiting).
  await page.waitForLoadState('networkidle', { timeout: 5000 }).catch(() => {});
  await awaitStateSnapshot(page);
}
