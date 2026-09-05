// CXA-F306: the lesson efficacy loop in the browser.
//
// Lessons were write-only — recorded into the ledger, ranked into briefs,
// never measured. The Overview's "Lesson efficacy" panel must render, for a
// SEEDED repeater (the exact persisted shape the cycle writes:
// `state.lesson_records`, the CXA-F306 ledger), the repeating section with the
// lesson's recorded-at date, its recurrence count, its most recent recurrence,
// the linked incidents with their dismiss affordance, and the one-click
// "File prevention ticket" action — all without a single console error.
//
// The suite never runs the agent cycle, so the ledger is seeded through the
// store-RPC surface the runners themselves use (load → mutate → save — the
// deploy-failure-bundle.spec.ts pattern) and restored afterwards.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

const base = process.env.E2E_BASE ?? 'http://127.0.0.1:4517';
const rpc = async (op, body) => {
  const r = await fetch(`${base}/api/projects/default/store?op=${op}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  if (!r.ok) throw new Error(`store op ${op}: ${r.status} ${await r.text()}`);
  return r.json();
};

const LESSON_REPEATING =
  'docker build fails when the base image tag moves — pin the base image version';
const LESSON_WATCHED = 'route PRs that touch gating files to their human approver at open';
const LESSON_PLAIN = 'clear every run error before cycle close';
const LESSON_SHIPPED =
  'keep tmp and build artifacts out of the repo — build outputs and scratch dirs belong in target/, /tmp or gitignored paths';

const recurrence = (day, reason) => ({
  at: `2026-09-${day}T10:00:00Z`,
  incident_at: `2026-09-${day}T09:00:00Z`,
  incident_reason: reason,
});

// Seed (or clear) the lesson ledger exactly as the cycle persists it. The
// fixture's original lesson state is captured on first load and restored by
// every cleanup, so the specs that run after this one see the world unchanged.
let originalLessons = null;
const setLessonLedger = async (lessonRecords) => {
  const state = await rpc('load');
  if (originalLessons === null) {
    originalLessons = { lessons: state.lessons ?? [], had_records: 'lesson_records' in state };
  }
  if (lessonRecords) {
    state.lesson_records = lessonRecords;
    state.lessons = lessonRecords.map((l) => l.text);
  } else {
    delete state.lesson_records;
    delete state.dismissed_matches;
    state.lessons = originalLessons.lessons;
  }
  await rpc('save', { data: JSON.stringify(state) });
};

const seededLedger = [
  {
    text: LESSON_REPEATING,
    at: '2026-08-30T10:00:00Z',
    cycle: 3,
    re_recordings: 0,
    recurrences: [
      recurrence('01', 'deploy failed'),
      recurrence('02', 'deploy failed'),
    ],
  },
  {
    text: LESSON_WATCHED,
    at: '2026-09-01T10:00:00Z',
    cycle: 4,
    re_recordings: 1,
    recurrences: [],
  },
  {
    text: LESSON_PLAIN,
    at: '2026-08-28T10:00:00Z',
    cycle: 2,
    re_recordings: 0,
    recurrences: [],
  },
];

test('a repeating lesson renders its recurrence history with the prevention action', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await setLessonLedger(seededLedger);
  try {
    await openApp(page);

    const panel = page.locator('#ov-lessons');
    // The repeating section exists and names the repeating lesson (AC2).
    await expect(panel).toContainText(/Repeating/i);
    await expect(panel).toContainText(LESSON_REPEATING);
    // Recorded-at, recurrence count and most recent recurrence are on the row.
    await expect(panel).toContainText(/recorded 2026-08-30/);
    await expect(panel).toContainText(/2 recurrences · last 2026-09-02/);
    // The linked incidents are visible with their dismissal affordance (AC1+AC3).
    await expect(panel).toContainText(/deploy failed · 2026-09-02/);
    await expect(panel.getByText('dismiss').first()).toBeVisible();
    // The one-click structural escalation exists (AC2). Every lesson row
    // carries the action, so pin the repeating section's own button.
    await expect(
      panel.getByRole('button', { name: /file prevention ticket/i }).first(),
    ).toBeVisible();
    // The watched lessons render their own lines without joining the
    // repeaters — including the plain lesson that never recurred (AC2:
    // "for every lesson ... when it was recorded").
    await expect(panel).toContainText(/Watched/i);
    await expect(panel).toContainText(LESSON_WATCHED);
    await expect(panel).toContainText(LESSON_PLAIN);
    await expect(panel).toContainText(/recorded 2026-08-28 · 0 recurrences/);

    await assertNoConsoleErrors(errors);
    await expect(page).toHaveScreenshot('lesson-efficacy.png');
  } finally {
    // Restore the world for the specs that run after this one.
    await setLessonLedger(null);
  }
});

test('a shipped bootstrap lesson is badged apart from locally learned ones', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  // The exact persisted shape the CXA-F371 seed writes: a ledger record with
  // the stable id and the "shipped" source marker, next to locally learned
  // rows that carry no source at all.
  await setLessonLedger([
    ...seededLedger,
    {
      text: LESSON_SHIPPED,
      at: '2026-09-05T08:00:00Z',
      cycle: 0,
      re_recordings: 0,
      recurrences: [],
      id: 'CXA-F371-tmp-build-artifact-hygiene',
      source: 'shipped',
    },
  ]);
  try {
    await openApp(page);
    const panel = page.locator('#ov-lessons');
    await expect(panel).toContainText(LESSON_SHIPPED);
    // Exactly one badge — on the shipped lesson; the local rows stay clean.
    const badge = panel.getByTitle(/shipped with the binary/i);
    await expect(badge).toHaveCount(1);
    await expect(badge).toHaveText(/shipped/i);
    await assertNoConsoleErrors(errors);
  } finally {
    // Restore the world for the specs that run after this one.
    await setLessonLedger(null);
  }
});

test('a project with no recorded lessons renders no efficacy panel', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await setLessonLedger(null);
  try {
    await openApp(page);
    // Zero gates render nothing — the overview reads exactly as before.
    await expect(page.locator('#ov-lessons')).toHaveText('');
    await assertNoConsoleErrors(errors);
  } finally {
    await setLessonLedger(null);
  }
});
