#!/usr/bin/env python3
"""Render the real koda web UI in a real browser.

probe_trace.py proves the API; this proves the page a person actually sees works
against that API (not the fixture server): it boots koda with the web UI on,
drives one turn, then runs a Playwright spec against the live port.

Usage:
  python3 tests/probe_web_live.py                 # hermetic (mock LLM server)
  BACKEND=config python3 tests/probe_web_live.py   # real backend (OmniRoute)
"""
import importlib.util, os, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
os.chdir(ROOT)

spec = importlib.util.spec_from_file_location("probe_trace", os.path.join(HERE, "probe_trace.py"))
pt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pt)

WEB_PORT = int(os.environ.get("WEB_PORT", "8792"))
pt.WEB_PORT = WEB_PORT
MODEL = pt.pc.MODEL

LIVE_SPEC = """
import { test, expect } from '@playwright/test';
const BASE = process.env.BASE;
const MODEL = process.env.MODEL;
test.use({ viewport: { width: 1440, height: 900 } });
test('the real koda API renders in the console', async ({ page }) => {
  const errors = [];
  page.on('pageerror', e => errors.push(String(e)));
  page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
  await page.goto(BASE, { waitUntil: 'networkidle' });

  // The turn koda actually ran is listed and selectable.
  const rail = page.getByRole('listbox', { name: 'Agent turns' });
  await expect(rail.getByText(/replace hello with goodbye/).first()).toBeVisible({ timeout: 15000 });
  await rail.getByText(/replace hello with goodbye/).first().click();

  // Its real steps render, including a write tool it called.
  const steps = page.getByRole('list', { name: 'Turn steps' });
  await expect(steps.getByLabel(/^step 0: model/)).toBeVisible({ timeout: 15000 });
  await expect(steps.getByLabel(/tool (edit_file|write_file)/).first()).toBeVisible();

  // The inspector shows the real request body koda sent.
  await steps.getByLabel(/^step 0: model/).click();
  await expect(page.getByText(new RegExp(`"model": "${MODEL.replace(/[.*+?^${}()|[\\]\\\\]/g, '\\\\$&')}"`)))
    .toBeVisible({ timeout: 15000 });
  await page.getByRole('tab', { name: 'Response' }).click();
  await expect(page.getByText(/data: \\{/).first()).toBeVisible();

  // The control rail reads the live config.
  await page.getByRole('tab', { name: 'Control' }).click();
  await expect(page.getByLabel('Model id')).toHaveValue(MODEL, { timeout: 15000 });

  await page.screenshot({ path: 'shots/live-01-console.png' });
  expect(errors, errors.join('\\n')).toEqual([]);
});
"""


def main():
    mock = pt.start_mock()
    ws = tempfile.mkdtemp()
    open(os.path.join(ws, "demo.txt"), "w").write("hello world\nsecond line\n")
    open(os.path.join(ws, "koda.toml"), "w").write(
        f"web_ui = true\nweb_ui_port = {WEB_PORT}\nmode = \"execute\"\n"
    )
    # Isolated config home, but carrying the api_key a real backend needs.
    cfg_home = tempfile.mkdtemp()
    os.makedirs(os.path.join(cfg_home, "koda"), exist_ok=True)
    if os.path.exists(pt.USER_CONFIG):
        with open(pt.USER_CONFIG) as src, open(os.path.join(cfg_home, "koda", "config.toml"), "w") as dst:
            dst.write(src.read())
    os.environ["XDG_CONFIG_HOME"] = cfg_home

    t = pt.pc.Tui(ws)
    t.read(3.0, until="ready")
    if not pt.wait_for_web():
        print("FAIL: koda did not serve the web UI")
        t.close(); sys.exit(1)

    demo = os.path.join(ws, "demo.txt")
    t.send("replace hello with goodbye in demo.txt\r")
    deadline = time.time() + (120.0 if pt.BACKEND == "config" else 20.0)
    while time.time() < deadline:
        t.read(1.0)
        if "goodbye" in open(demo).read():
            break
    t.read(3.0)

    spec_path = os.path.join(HERE, "visual", "live.spec.js")
    open(spec_path, "w").write(LIVE_SPEC)
    env = dict(os.environ, BASE=f"http://127.0.0.1:{WEB_PORT}", MODEL=MODEL)
    res = subprocess.run(["npx", "playwright", "test", "live.spec.js", "--reporter=line"],
                         cwd=os.path.join(HERE, "visual"), env=env,
                         capture_output=True, text=True)
    print(res.stdout[-3000:])
    if res.returncode != 0:
        print(res.stderr[-2000:])
    os.remove(spec_path)

    t.close()
    subprocess.run(["rm", "-rf", ws, cfg_home])
    if mock:
        mock.kill()
    sys.exit(res.returncode)


if __name__ == "__main__":
    main()
