#!/usr/bin/env node
// Kareenos Desktop Agent — Brand v5 assets (decisions 21–22 of the wave 2 plan):
//   * the app icon set (the .exe and its shortcut, the title bar, Add/Remove
//     Programs): the v5 mark as icon.ico (16–256) + the PNG sizes Tauri lists,
//   * the tray icons by state (the v5 mark with a corner state dot), 32 px PNG,
//   * the installer's WiX banner (493×58) and dialog (493×312) images, BMP.
// Rendered with headless Chrome (puppeteer from the kareenos_frontend checkout —
// there is no npm install in this fork) from the brand tokens; the outputs are
// COMMITTED, CI does not regenerate them. Run on the Kareenos Mac:
//   node scripts/brand-assets.cjs            # everything
//   node scripts/brand-assets.cjs icons      # only the app icon set (also: tray, wix)
'use strict';
const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');
const FRONTEND = process.env.KAREENOS_FRONTEND || '/Users/hakimamir/Projects/gitKareenos/kareenos_frontend';
// puppeteer 25 is an ES module: loaded with a dynamic import inside main().
const PUPPETEER = require('url').pathToFileURL(path.join(FRONTEND, 'node_modules', 'puppeteer', 'lib', 'puppeteer', 'puppeteer.js')).href;
const ROOT = path.resolve(__dirname, '..');
const ICONS = path.join(ROOT, 'apps/desktop-agent/src-tauri/icons');
const WIX = path.join(ROOT, 'apps/desktop-agent/src-tauri/wix');
const FONTS = path.join(FRONTEND, 'public/vendor/kfonts/files');
const MARK = path.join(ROOT, 'apps/desktop-agent/ui/logo.png');

const ONLY = (process.argv[2] || 'all').replace(/^--/, '');
const want = (step) => ONLY === 'all' || ONLY === step;

const STATES = { idle: null, working: '#4F46E5', paused: '#C2410C', error: '#DC2626', offline: '#94A3B8' };
// The app icon: the mark on its tile (the canonical 2/32 inset, rx 9/32). At
// 24 px and under the K gets a heavier stroke (optical size) so the title-bar
// and taskbar icons stay legible.
const APP_ICON_SIZES = [16, 24, 32, 48, 64, 128, 256, 512];
const APP_ICON_SVG = (size) => {
  const stroke = size <= 24 ? 6.8 : 5.2;
  return `<svg width="${size}" height="${size}" viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#7C3AED"/><stop offset="0.45" stop-color="#4F46E5"/><stop offset="1" stop-color="#06B6D4"/></linearGradient></defs><rect x="4" y="4" width="56" height="56" rx="18" fill="url(#g)"/><path d="M23 19 V45 M41 19 L23 32 M23 32 L42 45" fill="none" stroke="#fff" stroke-width="${stroke}" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
};
// A Windows .ico made of PNG-compressed entries (what `tauri icon` writes too):
// ICONDIR (6 bytes) + one ICONDIRENTRY (16 bytes) per image + the PNG streams.
function writeIco(images, out) {
  const head = Buffer.alloc(6);
  head.writeUInt16LE(0, 0); head.writeUInt16LE(1, 2); head.writeUInt16LE(images.length, 4);
  const dir = Buffer.alloc(16 * images.length);
  let offset = head.length + dir.length;
  images.forEach((img, i) => {
    const e = i * 16;
    dir.writeUInt8(img.size >= 256 ? 0 : img.size, e);      // 0 means 256
    dir.writeUInt8(img.size >= 256 ? 0 : img.size, e + 1);
    dir.writeUInt8(0, e + 2); dir.writeUInt8(0, e + 3);      // palette, reserved
    dir.writeUInt16LE(1, e + 4); dir.writeUInt16LE(32, e + 6); // planes, bpp
    dir.writeUInt32LE(img.png.length, e + 8); dir.writeUInt32LE(offset, e + 12);
    offset += img.png.length;
  });
  fs.writeFileSync(out, Buffer.concat([head, dir, ...images.map((i) => i.png)]));
}
const MARK_SVG = (size) => `<svg width="${size}" height="${size}" viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#7C3AED"/><stop offset="0.45" stop-color="#4F46E5"/><stop offset="1" stop-color="#06B6D4"/></linearGradient></defs><rect x="4" y="4" width="56" height="56" rx="18" fill="url(#g)"/><path d="M23 19 V45 M41 19 L23 32 M23 32 L42 45" fill="none" stroke="#fff" stroke-width="5.2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

function fontFaces() {
  const f = (family, file, weight) => `@font-face{font-family:'${family}';font-weight:${weight};src:url(file://${path.join(FONTS, file)}) format('woff2')}`;
  return [
    f('Space Grotesk', 'space-grotesk-latin-700-normal.woff2', 700),
    f('Space Grotesk', 'space-grotesk-latin-600-normal.woff2', 600),
    f('Manrope', 'manrope-latin-500-normal.woff2', 500),
    f('JetBrains Mono', 'jetbrains-mono-latin-500-normal.woff2', 500),
  ].join('\n');
}

const page_html = (body, css) => `<!doctype html><html><head><meta charset="utf-8"><style>${fontFaces()} html,body{margin:0;padding:0;background:transparent} ${css}</style></head><body>${body}</body></html>`;

async function shot(page, html, selector, out, opts) {
  // A file:// page (not about:blank), so the file:// font faces are allowed to load.
  const tmp = path.join(require('os').tmpdir(), `kareenos-brand-${process.pid}.html`);
  fs.writeFileSync(tmp, html);
  await page.goto('file://' + tmp, { waitUntil: 'load' });
  await page.evaluate(() => document.fonts.ready);
  await new Promise((r) => setTimeout(r, 150));
  const el = await page.$(selector);
  await el.screenshot(Object.assign({ path: out }, opts || {}));
}

(async () => {
  const puppeteer = (await import(PUPPETEER)).default;
  const browser = await puppeteer.launch({ headless: true, args: ['--allow-file-access-from-files', '--no-sandbox'] });
  const page = await browser.newPage();
  await page.setViewport({ width: 600, height: 400, deviceScaleFactor: 1 });

  // 0. The app icon set: every size rendered from the vector at 1:1 (never a
  //    downscale), icon.ico from 16 to 256, and the PNGs tauri.conf.json lists.
  if (want('icons')) {
    const rendered = {};
    for (const size of APP_ICON_SIZES) {
      const html = page_html(`<div id="i" style="width:${size}px;height:${size}px;line-height:0">${APP_ICON_SVG(size)}</div>`, '');
      const out = path.join(require('os').tmpdir(), `kareenos-icon-${size}.png`);
      await shot(page, html, '#i', out, { omitBackground: true });
      rendered[size] = fs.readFileSync(out);
      fs.unlinkSync(out);
    }
    writeIco(APP_ICON_SIZES.filter((s) => s <= 256).map((s) => ({ size: s, png: rendered[s] })), path.join(ICONS, 'icon.ico'));
    fs.writeFileSync(path.join(ICONS, '32x32.png'), rendered[32]);
    fs.writeFileSync(path.join(ICONS, '128x128.png'), rendered[128]);
    fs.writeFileSync(path.join(ICONS, '128x128@2x.png'), rendered[256]);
    fs.writeFileSync(path.join(ICONS, 'icon.png'), rendered[512]);
  }

  // 1. Tray icons: the mark at 32 px with a 12 px state dot in the lower-right corner, white ring.
  for (const [state, color] of Object.entries(STATES)) {
    if (!want('tray')) break;
    const dot = color ? `<div style="position:absolute;right:-1px;bottom:-1px;width:12px;height:12px;border-radius:50%;background:${color};box-shadow:0 0 0 2px #fff"></div>` : '';
    const html = page_html(`<div id="t" style="position:relative;width:32px;height:32px">${MARK_SVG(32)}${dot}</div>`, '');
    await shot(page, html, '#t', path.join(ICONS, `tray-${state}.png`), { omitBackground: true });
  }

  if (!want('wix')) {
    await browser.close();
    console.log('brand assets written (' + ONLY + '):', fs.readdirSync(ICONS).join(', '));
    return;
  }
  // 2. The installer banner: the dark panel with the light lockup, 493×58.
  const lockup = (markPx, wmPx, dark) => `<div style="display:flex;align-items:center;gap:${Math.round(markPx * 0.4)}px">${MARK_SVG(markPx)}<span style="font-family:'Space Grotesk';font-weight:700;font-size:${wmPx}px;letter-spacing:-.02em;color:${dark ? '#fff' : '#0F172A'}">kareen<span style="color:${dark ? '#818CF8' : '#4F46E5'}">os</span></span></div>`;
  const banner = page_html(
    `<div id="b" style="width:493px;height:58px;background:#0B1220;display:flex;align-items:center;justify-content:space-between;padding:0 18px;box-sizing:border-box;border-bottom:1px solid rgba(148,163,184,.16)">${lockup(28, 20, true)}<span style="font-family:'JetBrains Mono';font-weight:500;font-size:10px;letter-spacing:.12em;text-transform:uppercase;color:#22D3EE">Desktop Agent</span></div>`, '');
  await shot(page, banner, '#b', path.join(WIX, 'banner.png'));

  // 3. The installer dialog: the gradient panel with the mark and one line, 493×312 (the left 164 px are visible beside the dialog text).
  const dialog = page_html(
    `<div id="d" style="width:493px;height:312px;background:linear-gradient(120deg,#5B21B6,#4338CA 45%,#0E7490);position:relative;overflow:hidden">
       <div style="position:absolute;left:0;top:0;width:164px;height:312px;display:flex;flex-direction:column;justify-content:flex-end;padding:22px;box-sizing:border-box">
         ${MARK_SVG(44)}
         <div style="margin-top:14px;font-family:'Space Grotesk';font-weight:700;font-size:18px;line-height:1.15;letter-spacing:-.02em;color:#fff">Your agents,<br>in your programs</div>
         <div style="margin-top:10px;font-family:'JetBrains Mono';font-weight:500;font-size:9px;letter-spacing:.12em;text-transform:uppercase;color:#A5F3FC">Desktop Agent</div>
       </div>
       <div style="position:absolute;left:164px;top:0;right:0;bottom:0;background:#fff"></div>
     </div>`, '');
  await shot(page, dialog, '#d', path.join(WIX, 'dialog.png'));
  await browser.close();

  // WiX wants BMP: convert with sips (macOS) and drop the PNGs.
  for (const name of ['banner', 'dialog']) {
    const png = path.join(WIX, `${name}.png`), bmp = path.join(WIX, `${name}.bmp`);
    execFileSync('sips', ['-s', 'format', 'bmp', png, '--out', bmp], { stdio: 'ignore' });
    fs.unlinkSync(png);
  }
  console.log('brand assets written:', fs.readdirSync(ICONS).filter((f) => f.startsWith('tray-')).join(', '), '| wix:', fs.readdirSync(WIX).join(', '));
})().catch((e) => { console.error(e); process.exit(1); });
