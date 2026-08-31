import { test, expect } from '@playwright/test';
import fs from 'fs';

const BASE = process.env.BASE || 'http://127.0.0.1:8790';
const SHOTS = 'shots';
fs.mkdirSync(SHOTS, { recursive: true });

test.use({ viewport: { width: 1280, height: 860 } });

test('web UI navigation exposes all features', async ({ page }) => {
  const consoleErrors = [];
  page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('pageerror', e => consoleErrors.push(String(e)));

  await page.goto(BASE, { waitUntil: 'networkidle' });

  // --- Navigation: all five tabs must be present and reachable ---
  const tabs = ['Live Logs', 'LLM Debug', 'Code Graph', 'Agents & Skills', 'System Prompt'];
  for (const label of tabs) {
    await expect(page.getByRole('tab', { name: label })).toBeVisible();
  }
  await page.screenshot({ path: `${SHOTS}/01-logs.png`, fullPage: false });

  // --- LLM Debug: request query + response, incl. the live "processing" one ---
  await page.getByRole('tab', { name: 'LLM Debug' }).click();
  await expect(page.getByText('rr-session-1').first()).toBeVisible();
  await expect(page.getByText('rr-session-2').first()).toBeVisible();
  // A processing badge proves the live prompt/response view works.
  await expect(page.getByText('processing').first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/02-llm-debug.png` });
  // The request (prompt) and response tabs are present.
  await expect(page.getByRole('button', { name: /Prompt \(our request\)/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Response \(LLM\)/ })).toBeVisible();
  // Inspect the first (completed) session's request/prompt explicitly.
  await page.getByText('rr-session-1').first().click();
  await expect(page.getByRole('button', { name: /Prompt \(our request\)/ })).toBeVisible();
  await page.getByRole('button', { name: /Prompt \(our request\)/ }).click();
  await expect(page.getByText(/off-by-one/).first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/03-llm-request.png` });

  // --- Code Graph ---
  await page.getByRole('tab', { name: 'Code Graph' }).click();
  await expect(page.getByText(/symbols/).first()).toBeVisible();
  await page.waitForTimeout(800); // let the force layout settle a little
  await page.screenshot({ path: `${SHOTS}/04-graph.png` });

  // --- Agents & Skills: the add form and existing entries are visible ---
  await page.getByRole('tab', { name: 'Agents & Skills' }).click();
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOTS}/05-agents-skills.png` });
  await expect(page.getByText(/rust-error-handling/).first()).toBeVisible();
  await expect(page.getByText(/senior-reviewer/).first()).toBeVisible();
  await expect(page.getByLabel('Name')).toBeVisible();          // add form present
  await expect(page.getByLabel('When to use')).toBeVisible();
  await expect(page.getByLabel('Body')).toBeVisible();
  // Add a new skill through the form.
  await page.getByLabel('Name').fill('playwright-added-skill');
  await page.getByLabel('When to use').fill('demonstrating the add form works');
  await page.getByLabel('Body').fill('This skill was added by the visual test.');
  await page.getByRole('button', { name: /Save Skill/ }).click();
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${SHOTS}/06-skill-added.png` });
  await expect(page.getByText(/playwright-added-skill/).first()).toBeVisible({ timeout: 5000 });

  // --- System Prompt: the newly-added navigation + editor ---
  await page.getByRole('tab', { name: 'System Prompt' }).click();
  await expect(page.getByRole('heading', { name: 'System Prompt' })).toBeVisible();
  await expect(page.getByLabel('System prompt')).toBeVisible();  // the textarea
  await expect(page.getByRole('button', { name: /Save prompt/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Load built-in/ })).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/07-system-prompt.png` });
  // Edit and save a custom prompt.
  const ta = page.getByLabel('System prompt');
  await ta.fill('You are a terse, senior Rust reviewer. Prefer small diffs.');
  await page.getByRole('button', { name: /Save prompt/ }).click();
  await expect(page.getByText(/System prompt saved/)).toBeVisible({ timeout: 5000 });
  await page.screenshot({ path: `${SHOTS}/08-system-prompt-saved.png` });

  // No uncaught JS/render errors anywhere in the flow.
  expect(consoleErrors, consoleErrors.join('\n')).toEqual([]);
});
