// Takes the pictures in docs/img from made-up games and mods, in a headless browser, so nothing opens on your screen and
// no real library, name or path ends up in the repository.
//
//   node tools/screenshots.mjs <path to rig> <path to a Chromium based browser> [folder, default docs/img]
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [rigBin, browserBin, outArg] = process.argv.slice(2);
if (!rigBin || !browserBin) { console.error('usage: node tools/screenshots.mjs <rig> <browser> [folder]'); process.exit(2); }
const out = outArg || new URL('../docs/img', import.meta.url).pathname;
mkdirSync(out, { recursive: true });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const home = mkdtempSync(join(tmpdir(), 'gantry-shots-'));
const port = 19600 + Math.floor(Math.random() * 300);
const dbg = 9600 + Math.floor(Math.random() * 300);
// nothing the app starts may reach the real desktop
const stubDir = join(home, 'stubs'); mkdirSync(stubDir);
for (const n of ['xdg-open', 'open', 'dolphin', 'nautilus', 'thunar', 'steam', 'notify-send', 'kdialog', 'zenity', 'gamemoderun', 'mangohud', 'wine']) {
  writeFileSync(join(stubDir, n), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
}
const quiet = Object.fromEntries(Object.entries(process.env).filter(([k]) => !['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS'].includes(k)));
const rig = spawn(rigBin, [], { env: { ...quiet, PATH: stubDir + ':' + (process.env.PATH || ''), HOME: home, USERPROFILE: home, RIG_PORT: String(port), RIG_NO_WINDOW: '1' }, stdio: 'ignore' });
let browser = null;
const finish = code => { try { rig.kill(); } catch (e) {} try { browser && browser.kill(); } catch (e) {} try { rmSync(home, { recursive: true, force: true }); } catch (e) {} process.exit(code); };
process.on('SIGINT', () => finish(1));

// small made-up pictures (a sky, a road, a field) so the shelves are not empty boxes
const scene = (a, b, c) => 'data:image/svg+xml;utf8,' + encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs><rect width="640" height="360" fill="url(#g)"/><circle cx="470" cy="110" r="46" fill="${c}" opacity=".85"/><path d="M0 300 L120 210 L230 285 L350 190 L470 280 L640 200 L640 360 L0 360Z" fill="#000" opacity=".35"/></svg>`);
const SC = [scene('#1b2a4e', '#7a4fa3', '#ffd27a'), scene('#0f3b3a', '#2f8f6b', '#e8f6c9'), scene('#4a1d2b', '#d9663c', '#fff0b8'), scene('#122033', '#3d6fb5', '#cfe8ff')];

try {
  for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${port}/`); break; } catch (e) { await sleep(200); } }
  let up = false, dbgPort = dbg;
  for (let attempt = 0; attempt < 3 && !up; attempt++) {
    dbgPort = dbg + attempt;
    browser = spawn(browserBin, ['--headless=new', '--disable-gpu', '--no-sandbox', '--disable-dev-shm-usage', '--no-first-run', `--remote-debugging-port=${dbgPort}`, `--user-data-dir=${join(home, 'profile' + attempt)}`, 'about:blank'], { stdio: 'ignore' });
    for (let i = 0; i < 150 && !up; i++) { try { await fetch(`http://127.0.0.1:${dbgPort}/json/version`); up = true; } catch (e) { await sleep(200); } }
    if (!up) { try { browser.kill(); } catch (e) {} }
  }
  if (!up) throw new Error('the browser did not start its debugging port');
  const target = await (await fetch(`http://127.0.0.1:${dbgPort}/json/new?http://127.0.0.1:${port}/`, { method: 'PUT' })).json();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise(r => (ws.onopen = r));
  let id = 0; const pending = new Map();
  let send;
  ws.onmessage = e => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m.result || m.error); pending.delete(m.id); }
    if (m.method === 'Runtime.exceptionThrown') console.error('script error:', JSON.stringify(m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 300));
    // every picture the page asks for (covers, mod pictures, screenshots) is answered here with a made-up scene: nothing
    // leaves the computer and nothing real is shown
    if (m.method === 'Fetch.requestPaused') {
      const q = m.params;
      if (q.resourceType === 'Image' && !q.request.url.startsWith('data:')) {
        let h = 0; for (const c of q.request.url) h = (h * 31 + c.charCodeAt(0)) >>> 0;
        const svg = decodeURIComponent(SC[h % SC.length].split(',')[1]);
        send('Fetch.fulfillRequest', { requestId: q.requestId, responseCode: 200, responseHeaders: [{ name: 'Content-Type', value: 'image/svg+xml' }, { name: 'Access-Control-Allow-Origin', value: '*' }], body: Buffer.from(svg).toString('base64') });
      } else send('Fetch.continueRequest', { requestId: q.requestId });
    }
  };
  send = (method, params = {}) => new Promise(r => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async expr => { const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true }); if (r.exceptionDetails) console.error('evaluate failed:', expr.slice(0, 80), JSON.stringify(r.exceptionDetails.exception?.description || r.exceptionDetails.text).slice(0, 300)); return r.result?.value; };
  await send('Runtime.enable'); await send('Page.enable'); await send('Fetch.enable', { patterns: [{ urlPattern: '*', resourceType: 'Image' }] });
  await send('Emulation.setDeviceMetricsOverride', { width: 1360, height: 860, deviceScaleFactor: 1, mobile: false });
  await sleep(2500);
  if (!(await ev('typeof appState === "object" && Array.isArray(appState.games)'))) throw new Error('the page did not start');

  // the made-up library
  await ev(`(() => {
    const SC = ${JSON.stringify(SC)};
    const g = (appid, name, extra) => Object.assign({ appid, name, game_dir: '/games/' + name, exe_path: '', shipping_exe: '', bin_dir: '', engine: 'Unity', compatdata: '', pfx: '', has_pfx: true, is_custom: false, is_installed: true, steam_appid: appid, launch_args: '', env_vars: '', trainer_name: '', mod_count: 0, source: 'steam', proton_path: '', ue4ss: false, loader: 'bepinex', cover_id: '' }, extra || {});
    appState.games = [
      g('11001', 'Dustline', { mod_count: 3, engine: 'Standard / Non-UE', loader: '' }),
      g('11002', 'Hollow Tactics', { engine: 'Unreal Engine 5.4', loader: '', ue4ss: true, mod_count: 2 }),
      g('11003', 'Iron Harbor', { source: 'hydra', engine: 'Standard / Non-UE', loader: '', steam_appid: '', mod_count: 1 }),
      g('11004', 'Lantern Keep', { mod_count: 4 }),
      g('11005', 'Neon Rally', { mod_count: 5, engine: 'Standard / Non-UE', loader: '' }),
      g('11006', 'Orbital Garage', { engine: 'Unity', mod_count: 2 }),
      g('11007', 'Pixel Farmhand 2', { source: 'heroic', engine: 'Unity', steam_appid: '', mod_count: 0 }),
      g('11008', 'Skyward Foundry', { engine: 'Unreal Engine 5.5', loader: '', ue4ss: false, mod_count: 0 }),
      g('11009', 'Starfall Drifters', { source: 'lutris', engine: 'Standard / Non-UE', loader: '', steam_appid: '', mod_count: 1 }),
      g('11010', 'Cinder and Salt', { is_installed: false, mod_count: 0 }),
    ];
    appState.selected_idx = 4;
    appState.unity = { il2cpp: false, x64: true, loader: 'bepinex', recommended: 'bepinex' };
    appState.health = { '11005': 'ok', '11002': 'warn' };
    const ago = h => Date.now() - h * 3600000;
    appState.meta = { '11005': { fav: true, notes: '', tags: ['racing'], playtime_s: 126000, last_played_ms: ago(5) }, '11004': { fav: true, notes: '', tags: [], playtime_s: 54000, last_played_ms: ago(30) }, '11001': { fav: false, notes: '', tags: ['co-op'], playtime_s: 36000, last_played_ms: ago(96) }, '11006': { fav: false, notes: '', tags: [], playtime_s: 9000, last_played_ms: ago(240) } };
    appState.vanilla = [];
    appState.settings = { tour_done: true, install_preview: true, keep_background: false };
    appState.library = { root: '/lib', dirty: false, pending_pack: null, deployed: 14, overwrites: 0, overrides: [], anti_cheat: null, drift: { count: 0, files: [] }, separate_saves: true, save_folders: 2, plugins: false, saves_of: 'Everything', conflicts: [{ path: 'Data/lighting.cfg', mods: ['Better Lighting', 'Night Skies'], ids: ['a', 'd'], winner: 'Night Skies', winner_id: 'd', pinned: false }], profiles: ['Everything', 'Vanilla plus', 'Clean drive'], undo: 'Disabled Photo Mode', history: [{ label: 'Disabled Photo Mode', ts: ago(0.1) }], trash: [],
      mods: [
        { id: 'a', name: 'Better Lighting', version: '1.2', enabled: true, target: '', source: 'Better Lighting-123-1-2.zip', files: 12, size: 5100000, conflicts: true, nexus: { game: 'neonrally', mod_id: 123, file_id: 4 }, thunderstore: null, notes: 0, note: '', depends_on: [], needed_by: [], placed_by: 'names' },
        { id: 'b', name: 'Handling Overhaul', version: '3.1', enabled: true, target: '', source: 'handling.7z', files: 6, size: 2400000, conflicts: false, nexus: { game: 'neonrally', mod_id: 88, file_id: 9 }, thunderstore: null, notes: 0, note: '', depends_on: [], needed_by: [], placed_by: 'names' },
        { id: 'c', name: 'HD Road Textures', version: '2.0', enabled: true, target: 'Game', source: 'roads.7z', files: 2, size: 774000000, conflicts: false, nexus: null, thunderstore: null, notes: 0, note: '', depends_on: [], needed_by: [], placed_by: 'names' },
        { id: 'd', name: 'Night Skies', version: '0.9', enabled: true, target: '', source: 'skies.zip', files: 4, size: 12000000, conflicts: true, nexus: null, thunderstore: null, notes: 0, note: '', depends_on: [], needed_by: [], placed_by: 'names' },
        { id: 'e', name: 'Photo Mode Unlocked', version: '1.0', enabled: false, target: '', source: 'photo.zip', files: 3, size: 400000, conflicts: false, nexus: null, thunderstore: null, notes: 0, note: '', depends_on: [], needed_by: [], placed_by: 'names' },
      ] };
    appState.nexus = Object.assign(appState.nexus || {}, { configured: true, premium: false, updates: [{ appid: '11005', lib_id: 'b', name: 'Handling Overhaul', installed_version: '3.1', new_version: '3.2', new_file_id: 12, new_file_name: 'handling-3-2.7z', changes: 'Fixed the brake bias on wet roads.', nexus: { game: 'neonrally', mod_id: 88, file_id: 9 }, page_url: 'https://www.nexusmods.com/neonrally/mods/88' }], needs: [], checking: false });
    appState.pending_install = null;
    appState.notes = { unread: 0, items: [] };
    window.__SC = SC;
    document.head.insertAdjacentHTML('beforeend', '<style>#toastPill { display: none !important }</style>');
    renderUI(true);
  })()`);
  // answers the page would get from the program and the mod sites
  await ev(`window.fetch = ((orig) => (u, o) => {
    const s = String(u); const j = x => Promise.resolve(new Response(JSON.stringify(x)));
    if (s.startsWith('/api/health')) return j(window.__HEALTH);
    if (s.startsWith('/api/store?')) return j({ more: false, next: { n: 0, t: 0 }, curated: false, notes: [], categories: [], cards: [
      { source: 'nexus', id: '101', page_url: 'https://www.nexusmods.com/neonrally/mods/101', name: 'Better Lighting', author: 'lumina', version: '1.2', summary: 'Softer shadows, warmer evenings and proper headlight cones.', downloads: 184200, endorsements: 9120, updated_ms: Date.now() - 12 * 86400000, size_kb: 5100, picture: window.__SC[0], installed: true },
      { source: 'nexus', id: '102', page_url: 'https://www.nexusmods.com/neonrally/mods/102', name: 'Handling Overhaul', author: 'torque_works', version: '3.2', summary: 'Every car gets its own weight, grip and brake feel.', downloads: 121400, endorsements: 6800, updated_ms: Date.now() - 40 * 86400000, size_kb: 2400, picture: window.__SC[2] },
      { source: 'thunderstore', id: 'Ravenwood-Neon_Radio', page_url: 'https://thunderstore.io/package/Ravenwood-Neon_Radio', name: 'Neon Radio', author: 'Ravenwood', version: '2.4.0', summary: 'Adds twelve new radio stations with licence-free music.', downloads: 88100, endorsements: 0, updated_ms: Date.now() - 6 * 86400000, size_kb: 48000, picture: window.__SC[1] },
      { source: 'nexus', id: '103', page_url: 'https://www.nexusmods.com/neonrally/mods/103', name: 'Photo Mode Unlocked', author: 'framegrab', version: '1.0', summary: 'Free camera, depth of field and a hidden-UI shot for every car.', downloads: 73900, endorsements: 4100, updated_ms: Date.now() - 90 * 86400000, size_kb: 400, picture: window.__SC[3] },
      { source: 'nexus', id: '104', page_url: 'https://www.nexusmods.com/neonrally/mods/104', name: 'HD Road Textures', author: 'asphalt_lab', version: '2.0', summary: '4K roads, kerbs and road markings.', downloads: 65300, endorsements: 3900, updated_ms: Date.now() - 200 * 86400000, size_kb: 774000, picture: window.__SC[2] },
      { source: 'thunderstore', id: 'Pitwall-Race_Engineer', page_url: 'https://thunderstore.io/package/Pitwall-Race_Engineer', name: 'Race Engineer', author: 'Pitwall', version: '1.1.3', summary: 'A voice that tells you tyre wear and gaps to the cars around you.', downloads: 41200, endorsements: 0, updated_ms: Date.now() - 20 * 86400000, size_kb: 9000, picture: window.__SC[0] },
    ] });
    if (s.startsWith('/api/screenshots')) return j({ modded: 4, plain: 2, items: [0, 1, 2, 3, 4, 5].map(n => ({ n, name: n + '.jpg', at_ms: Date.now() - (n + 1) * 86400000 * 1.7, kind: n === 2 || n === 5 ? 'plain' : 'modded' })) });
    if (s.startsWith('/api/workshop')) return j({ items: [
      { id: '1', title: 'Wide Tracks Pack', size: 48000000, updated: Math.floor(Date.now() / 1000) - 3 * 86400, subscribers: 21400, preview: '', removed: false },
      { id: '2', title: 'Rain Tweaks', size: 620000, updated: Math.floor(Date.now() / 1000) - 30 * 86400, subscribers: 8300, preview: '', removed: false },
      { id: '3', title: 'Better AI Drivers', size: 1900000, updated: Math.floor(Date.now() / 1000) - 140 * 86400, subscribers: 15700, preview: '', removed: false },
    ] });
    return orig(u, o);
  })(window.fetch);`);
  await ev(`window.__HEALTH = { loader: 'bepinex', code: 'ok', level: 'ok', count: 4, health: { score: 96, grade: 'great' }, issues: [] }`);
  const shot = async name => { await sleep(900); const s = await send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(out, name + '.png'), Buffer.from(s.data, 'base64')); console.log('wrote', name + '.png'); };
  const show = async js => { await ev(`closeModals(); ${js}`); };

  await show(`homeTrendData = [{ appid: '11005', name: 'Neon Rally', cards: [{ source: 'nexus', id: '101', page_url: 'https://www.nexusmods.com/neonrally/mods/101', name: 'Better Lighting', downloads: 184200, picture: window.__SC[0] }, { source: 'nexus', id: '102', page_url: 'https://www.nexusmods.com/neonrally/mods/102', name: 'Handling Overhaul', downloads: 121400, picture: window.__SC[2] }, { source: 'thunder', id: 'Ravenwood-Neon_Radio', name: 'Neon Radio', downloads: 88100, picture: window.__SC[1] }] }]; homeTrendAt = Date.now(); openLibrary()`);
  await shot('home');
  await show(`setTab('mods'); refreshHealth()`);
  await shot('mod-library');
  await show(`setTab('tools')`);
  await shot('tools');
  await show(`openFind('store')`);
  await sleep(1500);
  console.log('store list:', (await ev(`document.getElementById('tsList').innerHTML.length + ' ' + document.querySelectorAll('#tsList .s-card').length`)));
  await shot('find-mods');
  await ev(`window.__HEALTH = { loader: 'bepinex', code: 'ok', level: 'warn', count: 4, health: { score: 72, grade: 'good' }, issues: [{ code: 'drift', level: 'warn', args: [2], fix: 'redeploy' }, { code: 'needs', level: 'warn', args: ['Handling Lib'], fix: '' }, { code: 'dirty', level: 'info', args: [], fix: 'deploy' }] }`);
  await show(`setTab('mods'); refreshHealth()`);
  await shot('health');
  await show(`openShots()`);
  await sleep(800);
  await shot('screenshots');
  await show(`openWorkshop()`);
  await sleep(800);
  await shot('workshop');
  ws.close();
} catch (e) {
  console.error('failed:', e);
  finish(1);
}
finish(0);
