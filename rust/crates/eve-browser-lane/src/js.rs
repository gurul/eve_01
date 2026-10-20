//! The page scripts, byte for byte as browser_lane.py evaluates them (generated from the Python module's
//! strings; `_IDENTITY_JS` is shared by the collector and the click-time recheck exactly as there).

macro_rules! identity_js {
    () => {
        r#"
  const roleOf = (e) => {
    const r = e.getAttribute('role'); if (r) return r;
    const t = e.tagName.toLowerCase();
    if (t === 'a') return 'link';
    if (t === 'input') { const ty = (e.type || 'text').toLowerCase();
      if (ty === 'submit' || ty === 'button' || ty === 'reset') return 'button';
      if (ty === 'checkbox') return 'checkbox'; if (ty === 'radio') return 'radio';
      if (ty === 'search') return 'searchbox'; if (ty === 'password') return 'password'; return 'textbox'; }
    if (t === 'textarea') return 'textbox'; if (t === 'select') return 'combobox';
    if (t === 'summary') return 'button'; if (e.isContentEditable) return 'textbox';
    return t; };
  const tokens = (e) => (e.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/);
  const passwordLike = (e) => (e.type || '').toLowerCase() === 'password' ||
    tokens(e).some(a => a === 'current-password' || a === 'new-password');
  const secret = (e) => passwordLike(e) || e.hasAttribute('data-eve-filled') ||
    tokens(e).some(a => a.startsWith('cc-') || a === 'one-time-code');
  const nameOf = (e) => {
    const lab = e.getAttribute('aria-label'); if (lab) return lab;
    const by = e.getAttribute('aria-labelledby');
    if (by) { const t = by.split(/\s+/).map(id => (document.getElementById(id) || {}).innerText || '').join(' ').trim(); if (t) return t; }
    if (e.labels && e.labels.length) { const t = Array.from(e.labels).map(l => l.innerText).join(' ').trim(); if (t) return t; }
    const own = (e.innerText || (secret(e) ? '' : e.value) || e.placeholder || e.title || e.getAttribute('alt') || '').trim();
    if (own) return own;
    const img = e.querySelector('img[alt],svg[aria-label],[aria-label]');
    return img ? (img.getAttribute('alt') || img.getAttribute('aria-label') || '').trim() : ''; };
  const hrefOf = (e) => e.tagName.toLowerCase() === 'a' && e.hasAttribute('href') ? e.href : '';
  const domIdOf = (e) => String(e.id || '').slice(0, 120);
"#
    };
}

/// `_COLLECT_JS`: every interactive element with its accessible name, role, state and box (MAX_CANDIDATES = 120).
pub const COLLECT_JS: &str = concat!(
    r#"() => {
  const sel = 'a[href],button,input,textarea,select,summary,[role=button],[role=link],[role=tab],[role=menuitem],' +
              '[role=checkbox],[role=radio],[role=textbox],[role=combobox],[role=option],[role=switch],' +
              '[role=searchbox],[role=menuitemcheckbox],[role=menuitemradio],[contenteditable=true]';"#,
    identity_js!(),
    r#"
  for (const e of document.querySelectorAll('[data-eve-id]')) e.removeAttribute('data-eve-id');
  const out = []; const H = window.innerHeight, W = window.innerWidth; let i = 0;
  const onscreen = [], offscreen = [];
  for (const e of document.querySelectorAll(sel)) {
    const r = e.getBoundingClientRect();
    if (r.width < 2 || r.height < 2 || r.right < 0 || r.left > W) continue;
    const cs = getComputedStyle(e); if (cs.visibility === 'hidden' || cs.display === 'none' || cs.opacity === '0') continue;
    ((r.bottom < 0 || r.top > H) ? offscreen : onscreen).push([e, r]); }
  for (const [e, r] of onscreen.concat(offscreen)) {
    const role = roleOf(e); const name = nameOf(e).replace(/\s+/g, ' ').slice(0, 120);
    let value = '';
    if (role === 'checkbox' || role === 'switch' || role === 'menuitemcheckbox') value = (e.checked || e.getAttribute('aria-checked') === 'true') ? 'checked' : 'unchecked';
    else if (role === 'radio' || role === 'menuitemradio' || role === 'tab' || role === 'option') value = (e.checked || e.getAttribute('aria-checked') === 'true' || e.getAttribute('aria-selected') === 'true') ? 'selected' : '';
    else if (e.getAttribute('aria-expanded') !== null) value = e.getAttribute('aria-expanded') === 'true' ? 'expanded' : 'collapsed';
    else if (secret(e)) value = '';
    else if ((role === 'textbox' || role === 'searchbox' || role === 'combobox') && typeof e.value === 'string') value = e.value.slice(0, 40);
    const editable = ['textbox', 'searchbox', 'combobox'].includes(role) && !(e.tagName.toLowerCase() === 'select');
    e.setAttribute('data-eve-id', String(i));
    out.push({id: String(i++), role, name, value, x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width),
              h: Math.round(r.height), enabled: !(e.disabled || e.getAttribute('aria-disabled') === 'true'),
              secure: role === 'password' || passwordLike(e), editable, dialog: !!e.closest('dialog,[role=dialog],[role=alertdialog]'),
              focused: document.activeElement === e, offscreen: r.bottom < 0 || r.top > H,
              href: hrefOf(e), dom_id: domIdOf(e)});
    if (out.length >= 120) break; }
  const dlg = Array.from(document.querySelectorAll('dialog[open],[role=dialog],[role=alertdialog]')).find(d => {
    const r = d.getBoundingClientRect(); const cs = getComputedStyle(d);
    return r.width >= 2 && r.height >= 2 && cs.visibility !== 'hidden' && cs.display !== 'none'; });
  const focused = document.activeElement;
  return {elements: out, title: document.title, url: location.href,
          dialog: dlg ? (dlg.innerText || '').trim().slice(0, 80) : '',
          focused_id: focused && focused.getAttribute ? focused.getAttribute('data-eve-id') : null,
          document_id: String(performance.timeOrigin), head: (document.body ? document.body.innerText : '').slice(0, 600),
          text_hash: (document.body ? document.body.innerText : '').length + ':' + document.body?.innerText?.slice(0, 4000)}; }
"#
);

/// `_RECHECK_JS`: the control under one data-eve-id, read as the collector read it, and its document.
pub const RECHECK_JS: &str = concat!(
    r#"(want) => {"#,
    identity_js!(),
    r#"
  const e = document.querySelector('[data-eve-id="' + want.id + '"]');
  const now = {url: location.href, doc: String(performance.timeOrigin), found: !!e};
  if (!e) return now;
  return Object.assign(now, {role: roleOf(e), name: nameOf(e).replace(/\s+/g, ' ').slice(0, 120), href: hrefOf(e),
                             dom_id: domIdOf(e)}); }
"#
);

/// `_WEBMCP_JS`: whether the page offers WebMCP tools (measurement only; never called).
pub const WEBMCP_JS: &str = r#"async () => {
  const mc = document.modelContext;
  if (!mc) return {api: false};
  if (typeof mc.getTools !== 'function') return {api: true, names: null};
  try {
    const tools = await Promise.race([mc.getTools(), new Promise(r => setTimeout(() => r(null), 300))]);
    if (!Array.isArray(tools)) return {api: true, names: null};
    return {api: true, names: tools.slice(0, 50).map(t => String((t && t.name) || ''))};
  } catch (e) { return {api: true, names: null}; }
}"#;

/// `_CONSENT_JS`: a visible cookie or consent notice and its buttons, each tagged data-eve-consent.
pub const CONSENT_JS: &str = r#"(pattern) => {
  const re = new RegExp(pattern, 'i');
  const vis = (e) => { const r = e.getBoundingClientRect(); if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(e); return cs.visibility !== 'hidden' && cs.display !== 'none' && cs.opacity !== '0'; };
  const boxes = Array.from(document.querySelectorAll('dialog[open],[role=dialog],[role=alertdialog],[aria-modal=true],' +
    '[id*=cookie i],[class*=cookie i],[id*=consent i],[class*=consent i]')).filter(vis);
  for (const b of boxes) {
    const text = (b.innerText || '').trim();
    if (!re.test(text)) continue;
    const btns = Array.from(b.querySelectorAll('button,[role=button],input[type=button],input[type=submit]')).filter(vis);
    for (const e of document.querySelectorAll('[data-eve-consent]')) e.removeAttribute('data-eve-consent');
    btns.forEach((e, i) => e.setAttribute('data-eve-consent', String(i)));
    return {text: text.replace(/\s+/g, ' ').slice(0, 200), buttons: btns.map((e, i) => ({id: String(i),
      name: (e.getAttribute('aria-label') || e.innerText || e.value || e.title || '').trim().replace(/\s+/g, ' ').slice(0, 80)}))};
  }
  return null; }"#;

/// `_FILL_FIELDS_JS`: the fields a vault item fills, tagged data-eve-fill, as [id, part, tag] rows.
pub const FILL_FIELDS_JS: &str = r#"(kind) => {
  for (const e of document.querySelectorAll('[data-eve-fill]')) e.removeAttribute('data-eve-fill');
  const vis = (e) => { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e);
    return r.width >= 2 && r.height >= 2 && cs.visibility !== 'hidden' && cs.display !== 'none' && !e.disabled && !e.readOnly; };
  const tokens = (e) => (e.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/);
  const has = (e, t) => tokens(e).includes(t);
  const words = (e) => { const parts = [e.getAttribute('aria-label'), e.getAttribute('name'), e.id, e.getAttribute('placeholder'), e.title];
    if (e.labels) for (const l of e.labels) parts.push(l.innerText);
    return parts.filter(Boolean).join(' ').toLowerCase(); };
  const skip = ['hidden', 'submit', 'button', 'reset', 'checkbox', 'radio', 'file', 'image', 'range', 'color', 'search'];
  const fields = Array.from(document.querySelectorAll('input,select,textarea')).filter(e => !skip.includes((e.type || '').toLowerCase()) && vis(e));
  const textish = (e) => e.tagName === 'INPUT' && ['text', 'email', 'tel', 'number', 'url'].includes((e.type || 'text').toLowerCase());
  const out = [];
  const mark = (e, part) => { if (!e || out.some(o => o[1] === part) || e.hasAttribute('data-eve-fill')) return;
    e.setAttribute('data-eve-fill', String(out.length)); out.push([String(out.length), part, e.tagName.toLowerCase()]); };
  if (kind === 'login') {
    const pwds = fields.filter(e => (e.type || '').toLowerCase() === 'password' || has(e, 'current-password'));
    const pw = pwds.find(e => has(e, 'current-password')) || pwds.find(e => !has(e, 'new-password')) || pwds[0];
    const inScope = (e) => !pw || !pw.form || e.form === pw.form;
    const named = (e) => has(e, 'username') || has(e, 'email') || (e.type || '').toLowerCase() === 'email';
    let user = fields.find(e => e !== pw && textish(e) && inScope(e) && named(e));
    if (!user && pw) { const before = fields.filter(e => e !== pw && textish(e) && inScope(e) && (e.compareDocumentPosition(pw) & Node.DOCUMENT_POSITION_FOLLOWING));
      user = before[before.length - 1]; }
    if (!user && !pw) { const looks = fields.filter(e => textish(e) && (named(e) || /user|e-?mail|login|account|phone/.test(words(e))));
      user = looks.length === 1 ? looks[0] : (fields.filter(textish).length === 1 ? fields.filter(textish)[0] : null); }
    mark(user, 'username'); mark(pw, 'password');
  } else if (kind === 'card') {
    const hinted = (e) => tokens(e).some(a => a.startsWith('cc-'));
    const by = (tok, rx) => fields.find(e => has(e, tok)) || fields.find(e => !hinted(e) && rx.test(words(e)));
    mark(by('cc-number', /card.?num|cc.?num|card no|number on card|\bpan\b/), 'number');
    const month = fields.find(e => has(e, 'cc-exp-month')) || fields.find(e => !hinted(e) && /exp.*(month|\bmm\b)/.test(words(e)));
    const year = fields.find(e => has(e, 'cc-exp-year')) || fields.find(e => !hinted(e) && /exp.*(year|\byy)/.test(words(e)));
    if (month && year && month !== year) { mark(month, 'exp_month'); mark(year, 'exp_year'); }
    else mark(fields.find(e => has(e, 'cc-exp')) || fields.find(e => !hinted(e) && e.tagName === 'INPUT' &&
      /expir|exp.?date|mm.?\/.?yy|valid.?thru/.test(words(e))), 'exp');
    mark(by('cc-csc', /cvc|cvv|csc|security.?code|card.?code|verification.?(code|number)/), 'cvc');
    mark(by('cc-name', /name.?on.?card|card.?holder|cardholder/), 'name');
  } else if (kind === 'totp') {
    const otp = fields.find(e => has(e, 'one-time-code') && !(e.maxLength === 1)) ||
      fields.find(e => textish(e) && e.maxLength !== 1 && /otp|one.?time|2fa|two.?factor|verification|auth.*code|security.?code|passcode|\bcode\b|token/.test(words(e)));
    const singles = fields.filter(e => textish(e) && e.maxLength === 1);
    if (otp) mark(otp, 'code');
    else if (singles.length >= 4 && singles.length <= 8) singles.forEach((e, i) => { e.setAttribute('data-eve-fill', String(out.length)); out.push([String(out.length), 'code#' + i, 'input']); });
    else { const a = document.activeElement; const t = fields.filter(textish);
      mark(a && t.includes(a) ? a : (t.length === 1 ? t[0] : null), 'code'); }
  }
  return out; }"#;

/// `_IDENTITY_JS`: roleOf, nameOf, hrefOf, domIdOf and the secret-field rule, shared by both scripts.
pub const IDENTITY_JS: &str = identity_js!();
