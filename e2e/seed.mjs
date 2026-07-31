// Deterministic UI fixture: seeds an ephemeral CoXAgent server through its own
// HTTP API, so the fixture can never drift from the state schema.
const base = process.env.E2E_BASE ?? 'http://127.0.0.1:4517';
const p = `${base}/api/projects/default`;
const send = async (method, path, body) => {
  const r = await fetch(`${p}${path}`, {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
  return r.json().catch(() => ({}));
};
const post = (path, body) => send('POST', path, body);
const put = (path, body) => send('PUT', path, body);

await post('/tickets', { title: 'Search box scopes per tab', description: 'Search results must match the active tab (Chat: channels/people/messages; Space: tickets/pages/people; Manage: people/projects).', ticket_type: 'feature', priority: 'high', has_ui: true, acceptance_criteria: ['Switching tabs switches the search scope', 'Cmd+K focuses the box'] });
await post('/tickets', { title: 'Delivery timeline connector line breaks between versions', description: 'The vertical line joining releases has gaps at each card boundary.', ticket_type: 'bug', priority: 'medium', has_ui: true });
await post('/tickets', { title: 'Sub-channels under a parent channel', description: 'A channel can own child channels; the sidebar nests them.', ticket_type: 'feature', priority: 'low' });
await post('/chat', { body: 'Standup: timeline fix is in review, search scoping starts today.', channel: 'general' });
await post('/chat', { body: 'Reminder: never bind port 4000 from a dogfood build.', channel: 'general' });
await put('/docs/deploy-health-gate', { folder: 'guides', title: 'Deploy health gate', body: '# Deploy health gate\n\nThe deploy is only successful once the health endpoint answers on the published port.\n' });
console.log('seeded');
