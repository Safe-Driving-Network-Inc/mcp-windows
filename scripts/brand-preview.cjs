#!/usr/bin/env node
// Kareenos Desktop Agent — renders the three pages with a stubbed Tauri API and a
// sample status, so the Brand v5 surfaces can be checked without a Windows build.
//   node scripts/brand-preview.cjs <out dir>
'use strict';
const path = require('path');
const fs = require('fs');
const FRONTEND = process.env.KAREENOS_FRONTEND || '/Users/hakimamir/Projects/gitKareenos/kareenos_frontend';
const PUPPETEER = require('url').pathToFileURL(path.join(FRONTEND, 'node_modules', 'puppeteer', 'lib', 'puppeteer', 'puppeteer.js')).href;
const UI = path.resolve(__dirname, '..', 'apps/desktop-agent/ui');
const OUT = process.argv[2] || path.join(process.cwd(), 'brand-preview');
fs.mkdirSync(OUT, { recursive: true });
const sample = {
  state: 'connected', signed_in: true, label: 'Lab PC (LAB-01) · Acme', project: 'Sales', account: 'Acme Tecnologia', user: 'Noura',
  server: 'wss://ap4.sdnvision.services:8092/desktop-channel', app_url: 'https://kareenos.com', version: '0.2.0', token_expires: '2026-12-02T00:00:00Z', renewable: true,
  paused: false, locked: false, managed: false, computer_name: 'Lab PC', computer_code: 'LAB-01', computer_mode: 'attended', actor: "Sara's Invoice Clerk", contract: '1.1',
  busy: "Sara's Invoice Clerk · Typing into Name", last_error: null, last_result_code: null, commands_done: 42, last_command_at: Date.now(), install_id: '7f3a…', hostname: 'DESKTOP-LAB01',
  data_dir: 'C:\\Users\\noura\\AppData\\Local\\Kareenos\\DesktopAgent', logs_dir: 'C:\\Users\\noura\\AppData\\Local\\Kareenos\\DesktopAgent\\logs', policy_path: 'C:\\ProgramData\\Kareenos\\DesktopAgent\\policy.json', updated_at: Date.now()
};
const trail = [{ at: Date.now() - 60000, level: 'info', text: 'bound — waiting for commands (Lab PC (LAB-01) · Acme)' }, { at: Date.now() - 20000, level: 'info', text: 'run command=1c79d9 action=fill lane=run:abc budget_ms=14000' }, { at: Date.now() - 5000, level: 'warn', text: 'HUMAN_ACTIVE: the person is using the computer (412 ms ago)' }];
function stub(status, mode) {
  return `window.__TAURI__ = { core: { invoke: function (name) { if (name === 'get_status') return Promise.resolve(${JSON.stringify(status)}); if (name === 'take_mode') return Promise.resolve(${JSON.stringify(mode)}); if (name === 'get_trail') return Promise.resolve(${JSON.stringify(trail)}); return Promise.resolve(null); } }, event: { listen: function () { return Promise.resolve(function () {}); } } };`;
}
(async () => {
  const puppeteer = (await import(PUPPETEER)).default;
  const browser = await puppeteer.launch({ headless: true, args: ['--allow-file-access-from-files', '--no-sandbox'] });
  const shots = [
    ['welcome', 'index.html', Object.assign({}, sample, { state: 'signed_out', signed_in: false, busy: null, actor: null, computer_name: '', computer_code: '' }), 'welcome', 560, 700],
    ['connected', 'index.html', sample, 'connected', 560, 700],
    ['connected-managed-locked', 'index.html', Object.assign({}, sample, { managed: true, locked: true, busy: null, actor: null, computer_mode: 'unattended' }), 'connected', 560, 700],
    ['diagnostics', 'diagnostics.html', sample, '', 760, 640],
    ['overlay', 'overlay.html', sample, '', 380, 48],
  ];
  for (const [name, file, status, mode, w, h] of shots) {
    const page = await browser.newPage();
    await page.setViewport({ width: w, height: h, deviceScaleFactor: 2 });
    await page.evaluateOnNewDocument(stub(status, mode));
    await page.goto('file://' + path.join(UI, file), { waitUntil: 'load' });
    await page.evaluate(() => document.fonts.ready);
    await new Promise((r) => setTimeout(r, 400));
    await page.screenshot({ path: path.join(OUT, name + '.png'), omitBackground: name === 'overlay' });
    await page.close();
  }
  await browser.close();
  console.log('previews in', OUT, fs.readdirSync(OUT).join(', '));
})().catch((e) => { console.error(e); process.exit(1); });
