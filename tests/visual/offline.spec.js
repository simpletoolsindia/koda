import { test, expect } from '@playwright/test';
// The page must work with no network at all. koda's only environment is a
// developer's machine, often offline, and the UI used to load Tailwind, React
// twice and an unversioned transpiler from two CDNs — so it did not.
test('the UI works with no network at all', async ({ page }) => {
  const blocked = [];
  await page.route('**', route => {
    const u = route.request().url();
    if (u.startsWith('http://127.0.0.1:8790')) return route.continue();
    blocked.push(u); return route.abort();
  });
  const errs = [];
  page.on('pageerror', e => errs.push(String(e)));
  await page.goto('http://127.0.0.1:8790', { waitUntil: 'networkidle' });
  await expect(page.getByText('koda · control center')).toBeVisible();
  await expect(page.getByRole('listbox', { name: 'Agent turns' })).toBeVisible();
  console.log('OFFLINE: external requests attempted =', blocked.length, '| page errors =', errs.length);
  expect(blocked, `the page reached out to: ${blocked.join(', ')}`).toEqual([]);
  expect(errs).toEqual([]);
});
