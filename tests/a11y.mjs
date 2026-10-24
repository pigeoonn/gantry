// The accessibility audit that tests/ui_smoke.mjs runs inside the page: every control that can be reached has a
// name a screen reader can say, and every piece of text has enough contrast against what it sits on.
// It returns a list of problems (empty = fine).

export const AUDIT_JS = `(() => {
  const out = [];
  const seen = new Set();
  if (!document.getElementById('a11yFreeze')) { const st = document.createElement('style'); st.id = 'a11yFreeze'; st.textContent = '*, *::before, *::after { transition: none !important; animation: none !important; }'; document.head.appendChild(st); }
  const visible = e => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden' && !e.closest('[aria-hidden="true"], [hidden]');
  const letters = s => /[\\p{L}\\p{N}]/u.test(s || '');
  const where = e => e.tagName.toLowerCase() + (e.id ? '#' + e.id : '') + (e.className && typeof e.className === 'string' ? '.' + e.className.trim().split(/\\s+/).slice(0, 2).join('.') : '');
  const report = (kind, e, extra) => { const key = kind + where(e) + (extra || ''); if (seen.has(key)) return; seen.add(key); out.push(kind + ': ' + where(e) + (extra ? ' ' + extra : '')); };

  // 1. names
  const ref = id => { const n = document.getElementById(id); return n ? n.textContent : ''; };
  const nameOf = e => {
    const aria = e.getAttribute('aria-label') || (e.getAttribute('aria-labelledby') ? ref(e.getAttribute('aria-labelledby')) : '');
    if (letters(aria)) return aria;
    if (e.matches('input, select, textarea')) {
      if (e.labels && [...e.labels].some(l => letters(l.textContent))) return 'label';
      if (letters(e.getAttribute('placeholder')) || letters(e.getAttribute('title'))) return 'hint';
      if (e.type === 'checkbox' || e.type === 'radio') { const p = e.closest('label'); if (p && letters(p.textContent)) return 'wrapped'; }
      return '';
    }
    if (letters(e.innerText)) return 'text';
    // a row scrolled out of view is not laid out (content-visibility), so it has no innerText; screen readers still read it
    if (typeof e.checkVisibility === 'function' && e.checkVisibility() && !e.checkVisibility({ contentVisibilityAuto: true }) && letters(e.textContent)) return 'text';
    if ([...e.querySelectorAll('img[alt]')].some(i => letters(i.alt))) return 'alt';
    if (letters(e.getAttribute('title'))) return 'title';
    return '';
  };
  document.querySelectorAll('button, [role="button"], a[onclick], a[href], input:not([type="hidden"]), select, textarea, [role="checkbox"], [role="tab"]').forEach(e => {
    if (!visible(e) || e.disabled && !nameOf(e)) return;
    if (!nameOf(e)) report('no name', e, e.outerHTML.slice(0, 110).replace(/\\s+/g, ' '));
  });
  // an element that does something when clicked has to be reachable by keyboard as well
  document.querySelectorAll('[onclick]').forEach(e => {
    if (!visible(e) || e.matches('button, a, input, select, textarea, summary, label, .modal-backdrop') || e.closest('button, a')) return;
    if (!e.textContent.trim() && !e.querySelector('svg, img') && !e.getAttribute('aria-label')) return; // nothing there to click
    if (e.tabIndex < 0 && !e.getAttribute('role')) report('clickable but not focusable', e);
  });
  // dialogs say what they are
  document.querySelectorAll('.modal-backdrop.open .modal-window').forEach(w => {
    if (w.getAttribute('role') !== 'dialog' || w.getAttribute('aria-modal') !== 'true') report('dialog without role', w);
    if (!w.getAttribute('aria-labelledby') && !w.getAttribute('aria-label')) report('dialog without a name', w, w.innerHTML.slice(0, 90).replace(/\\s+/g, ' '));
  });

  // 2. contrast
  const parse = c => {
    let m = c.match(/rgba?\\(([^)]+)\\)/);
    if (m) { const p = m[1].split(/[ ,\\/]+/).filter(Boolean).map(Number); return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 }; }
    m = c.match(/color\\(srgb ([^)]+)\\)/);
    if (m) { const p = m[1].split(/[ \\/]+/).filter(Boolean).map(Number); return { r: p[0] * 255, g: p[1] * 255, b: p[2] * 255, a: p.length > 3 ? p[3] : 1 }; }
    return c && c !== 'transparent' ? { unknown: c, r: 0, g: 0, b: 0, a: 0 } : null;
  };
  const over = (top, under) => ({ r: top.r * top.a + under.r * (1 - top.a), g: top.g * top.a + under.g * (1 - top.a), b: top.b * top.a + under.b * (1 - top.a), a: 1 });
  const lum = c => { const f = v => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }; return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b); };
  const ratio = (a, b) => { const l1 = lum(a), l2 = lum(b); return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05); };
  const backdrop = e => {
    // the colours the element sits on, from the page up to it
    const chain = [];
    for (let n = e; n && n.nodeType === 1; n = n.parentElement) chain.push(n);
    let bg = parse(getComputedStyle(document.documentElement).backgroundColor) || { r: 255, g: 255, b: 255, a: 1 };
    if (bg.a < 1) bg = over(bg, { r: 255, g: 255, b: 255, a: 1 });
    let unsure = false;
    for (const n of chain.reverse()) {
      const cs = getComputedStyle(n);
      const c = parse(cs.backgroundColor);
      if (cs.backgroundImage && cs.backgroundImage !== 'none') unsure = true;
      if (c && c.unknown) unsure = true;
      if (c && c.a > 0) bg = over(c, bg);
    }
    return { bg, unsure };
  };
  const opacityOf = e => { let o = 1; for (let n = e; n && n.nodeType === 1; n = n.parentElement) o *= Number(getComputedStyle(n).opacity); return o; };
  const skip = e => e.closest('[disabled], [aria-disabled="true"], script, style, option, .ghost-text, .fm-opt.off, [data-a11y-skip]');
  document.querySelectorAll('body *').forEach(e => {
    if (!visible(e) || skip(e)) return;
    const own = [...e.childNodes].filter(n => n.nodeType === 3 && letters(n.textContent)).map(n => n.textContent).join('').trim();
    if (!own) return;
    const cs = getComputedStyle(e);
    if (parseFloat(cs.fontSize) < 1 || e.getBoundingClientRect().width < 2) return;
    const fg0 = parse(cs.color);
    if (!fg0 || fg0.unknown || opacityOf(e) < 0.05) return;
    const { bg, unsure } = backdrop(e);
    if (unsure) return;
    const fg = over({ ...fg0, a: fg0.a * opacityOf(e) }, bg);
    const size = parseFloat(cs.fontSize), bold = Number(cs.fontWeight) >= 700;
    const need = size >= 24 || (size >= 18.66 && bold) ? 3 : 4.5;
    const r = ratio(fg, bg);
    if (r < need) report('low contrast', e, r.toFixed(2) + ' < ' + need + ' "' + own.slice(0, 30) + '"');
  });
  return out;
})()`;
