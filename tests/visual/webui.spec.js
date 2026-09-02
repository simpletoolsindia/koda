import { test, expect } from '@playwright/test';
import fs from 'fs';

const BASE = process.env.BASE || 'http://127.0.0.1:8790';
const SHOTS = 'shots';
fs.mkdirSync(SHOTS, { recursive: true });

test.use({ viewport: { width: 1440, height: 900 } });

// The fixture server is stateful; start every test from the same state so
// assertions never depend on run order.
test.beforeEach(async ({ request }) => {
  await request.post(`${BASE}/api/__reset`, { data: {} });
});

test('trace console shows a live turn and its real payloads', async ({ page }) => {
  const consoleErrors = [];
  page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('pageerror', e => consoleErrors.push(String(e)));

  await page.goto(BASE, { waitUntil: 'networkidle' });

  // --- Turn rail: both turns listed, the running one marked as such ---
  await expect(page.getByText('koda · control center')).toBeVisible();
  const rail = page.getByRole('listbox', { name: 'Agent turns' });
  await expect(rail.getByText(/Now write the changelog entry/)).toBeVisible();
  await expect(rail.getByText(/Fix the off-by-one/)).toBeVisible();
  await expect(rail.getByText('running').first()).toBeVisible();
  // The live turn is followed automatically, so its step is already on screen.
  await expect(page.getByRole('list', { name: 'Turn steps' }).getByText('live')).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/01-console-live.png` });

  // --- Select the finished turn: the waterfall reconstructs every step ---
  await rail.getByText(/Fix the off-by-one/).click();
  const steps = page.getByRole('list', { name: 'Turn steps' });
  await expect(steps.getByText('read_file').first()).toBeVisible();
  await expect(steps.getByText('edit_file').first()).toBeVisible();
  await expect(steps.getByText('run_command').first()).toBeVisible();
  await expect(steps.getByText('compaction').first()).toBeVisible();
  // Model, tool and compaction rows are visually distinct kinds.
  await expect(steps.getByText('model').first()).toBeVisible();
  // Failures, denials and retries are called out inline.
  await expect(steps.getByText('denied').first()).toBeVisible();
  await expect(steps.getByText(/1 retry/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/02-waterfall.png` });

  // --- Inspect a model call: the exact request and raw response ---
  await steps.getByLabel(/^step 0: model/).click();
  await expect(page.getByRole('tab', { name: 'Request' })).toBeVisible();
  await expect(page.getByText(/"model": "mtplx-qwen38-27b"/)).toBeVisible();
  await expect(page.getByText(/off-by-one/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/03-inspect-request.png` });

  await page.getByRole('tab', { name: 'Response' }).click();
  await expect(page.getByText(/Raw stream/)).toBeVisible();
  await expect(page.getByText(/data: \{"choices"/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/04-inspect-response.png` });

  await page.getByRole('tab', { name: 'Reasoning' }).click();
  await expect(page.getByText(/slice bound looks wrong/)).toBeVisible();

  // --- Prompt diff against the previous model call (compaction is visible) ---
  await steps.getByLabel(/^step 3: model/).click();
  await page.getByRole('tab', { name: 'Prompt Δ' }).click();
  await expect(page.getByText(/Context was compacted here/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/05-prompt-diff.png` });

  // --- Inspect a tool call: args, result, and the applied diff ---
  await steps.getByLabel('step 4: tool edit_file').click();
  await expect(page.getByRole('tab', { name: 'Tool' })).toBeVisible();
  await expect(page.getByText(/"path": "src\/view.rs"/)).toBeVisible();
  await page.getByRole('tab', { name: 'Change' }).click();
  await expect(page.getByText(/\.min\(len\)/)).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/06-inspect-tool.png` });

  expect(consoleErrors, consoleErrors.join('\n')).toEqual([]);
});

test('control rail edits the running session', async ({ page }) => {
  const consoleErrors = [];
  page.on('pageerror', e => consoleErrors.push(String(e)));
  await page.goto(BASE, { waitUntil: 'networkidle' });

  await page.getByRole('tab', { name: 'Control' }).click();
  await expect(page.getByLabel('Model id')).toBeVisible();
  await expect(page.getByLabel('Endpoint')).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/07-control-rail.png` });

  // Change the mode and apply: the POST must be accepted and reported.
  const mode = page.getByLabel('Mode', { exact: true });
  const before = await mode.inputValue();
  const target = ['plan', 'execute', 'vibe'].find(m => m !== before);
  await mode.selectOption(target);
  await page.getByRole('button', { name: 'Apply' }).click();
  await expect(page.getByText(/Settings applied/)).toBeVisible({ timeout: 6000 });
  await expect(mode).toHaveValue(target, { timeout: 6000 });

  // A feature toggle round-trips too.
  const webSearch = page.getByRole('switch', { name: 'Web search' });
  await webSearch.click();
  await page.getByRole('button', { name: 'Apply' }).click();
  await expect(page.getByText(/Settings applied/)).toBeVisible({ timeout: 6000 });

  // Memory: add a note and see it listed.
  const note = `visual test note ${Date.now()}`;
  await page.getByLabel('New memory note').fill(note);
  await page.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByText(note)).toBeVisible({ timeout: 6000 });

  // Learned rules: accepting a candidate removes it from the pending list.
  const candidates = page.getByRole('list', { name: 'Rule candidates' });
  await expect(candidates.getByText(/Functions here use snake_case/)).toBeVisible();
  await candidates.getByRole('button', { name: 'Accept', exact: true }).first().click();
  await expect(candidates.getByText(/Functions here use snake_case/)).toHaveCount(0, { timeout: 6000 });

  // Sessions are listed with resume/fork.
  await expect(page.getByText(/Add document parsing/)).toBeVisible();
  await page.getByRole('button', { name: 'Resume' }).first().click();
  await expect(page.getByText(/Resumed/)).toBeVisible({ timeout: 6000 });
  await page.screenshot({ path: `${SHOTS}/08-control-applied.png` });

  expect(consoleErrors, consoleErrors.join('\n')).toEqual([]);
});

test('palette, logs drawer and manage panel keep every feature reachable', async ({ page }) => {
  const consoleErrors = [];
  page.on('pageerror', e => consoleErrors.push(String(e)));
  await page.goto(BASE, { waitUntil: 'networkidle' });

  // Command palette: open, filter, and look a symbol up in the code graph.
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+k' : 'Control+k');
  const palette = page.getByRole('dialog', { name: 'Command palette' });
  await expect(palette).toBeVisible();
  await page.getByLabel('Command or symbol').fill('paginate');
  await page.keyboard.press('Shift+Enter');
  await expect(palette.getByText(/defined at src\/view.rs:120/)).toBeVisible({ timeout: 6000 });
  await page.screenshot({ path: `${SHOTS}/09-palette-symbol.png` });
  await page.keyboard.press('Escape');
  await expect(palette).toHaveCount(0);

  // Logs drawer.
  await page.getByRole('button', { name: 'Logs' }).click();
  await expect(page.getByRole('log', { name: 'Live log output' })).toBeVisible();
  await expect(page.getByText(/stream complete/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/10-logs-drawer.png` });
  await page.getByRole('button', { name: 'Hide logs' }).click();

  // Manage panel: graph, skills, prompt, raw captures.
  await page.getByRole('button', { name: 'Manage' }).click();
  const manage = page.getByRole('dialog', { name: 'Manage' });
  await expect(manage).toBeVisible();
  await expect(manage.getByText(/symbols/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/11-manage-graph.png` });

  await manage.getByRole('tab', { name: 'Agents & Skills' }).click();
  await expect(manage.getByText(/rust-error-handling/).first()).toBeVisible();
  await manage.getByRole('tab', { name: 'System Prompt' }).click();
  await expect(manage.getByRole('textbox', { name: 'System prompt', exact: true })).toBeVisible();
  await manage.getByRole('tab', { name: 'Raw Captures' }).click();
  await expect(manage.getByText('rr-session-1').first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/12-manage-captures.png` });
  await manage.getByRole('button', { name: 'Close manage' }).click();
  await expect(manage).toHaveCount(0);

  expect(consoleErrors, consoleErrors.join('\n')).toEqual([]);
});

test('mobile layout reaches every region without horizontal overflow', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(BASE, { waitUntil: 'networkidle' });

  for (const label of ['Turns', 'Trace', 'Inspect', 'Control']) {
    await expect(page.getByRole('tab', { name: label })).toBeVisible();
  }

  await page.getByRole('tab', { name: 'Turns' }).click();
  await expect(page.getByText(/Fix the off-by-one/).first()).toBeVisible();
  await page.getByText(/Fix the off-by-one/).first().click();
  await expect(page.getByRole('list', { name: 'Turn steps' }).getByText('read_file').first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/13-mobile-trace.png` });

  await page.getByRole('tab', { name: 'Control' }).click();
  await expect(page.getByLabel('Model id')).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/14-mobile-control.png` });

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(1);
});
