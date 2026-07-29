// Hash router with unsaved-changes leave guard.
// Views register routes via route(pattern, def); app.js calls start() once all
// view modules are loaded. def: { title, wide?, render(el, { params, query }) }
// where render may return a destroy() cleanup function.

const routes = []; // { pattern, parts, def }
let current = null; // { def, destroy }
let leaveGuard = null; // fn() -> true when it is OK to leave the current view
let suppressNext = false; // set when reverting a guarded navigation

export function route(pattern, def) {
  routes.push({ pattern, parts: pattern.split('/').filter(Boolean), def });
}

export function navigate(hash) {
  if (location.hash === hash) {
    render();
  } else {
    location.hash = hash;
  }
}

// Views with unsaved state register a guard; return true to allow leaving.
export function setLeaveGuard(fn) { leaveGuard = fn; }
export function clearLeaveGuard() { leaveGuard = null; }

function parseHash() {
  const raw = location.hash.replace(/^#/, '') || '/';
  const qIdx = raw.indexOf('?');
  const path = qIdx >= 0 ? raw.slice(0, qIdx) : raw;
  const query = new URLSearchParams(qIdx >= 0 ? raw.slice(qIdx + 1) : '');
  return { path, query };
}

function match(path) {
  const segs = path.split('/').filter(Boolean);
  for (const r of routes) {
    if (r.parts.length !== segs.length) continue;
    const params = {};
    let ok = true;
    for (let j = 0; j < r.parts.length; j++) {
      if (r.parts[j].charAt(0) === ':') {
        params[r.parts[j].slice(1)] = decodeURIComponent(segs[j]);
      } else if (r.parts[j] !== segs[j]) {
        ok = false;
        break;
      }
    }
    if (ok) return { def: r.def, params };
  }
  return null;
}

export function render() {
  if (suppressNext) {
    suppressNext = false;
    return;
  }
  const app = document.getElementById('app');
  const h = parseHash();

  // Unsaved-changes guard: ask the current view before leaving it.
  if (leaveGuard) {
    const guard = leaveGuard;
    let ok = false;
    try { ok = guard(); } catch (e) { ok = true; }
    if (!ok) {
      // Revert to the current view's hash without re-rendering.
      suppressNext = true;
      history.back();
      return;
    }
    leaveGuard = null;
  }
  const m = match(h.path) || match('/');
  if (!m) return;

  if (current && current.destroy) {
    try { current.destroy(); } catch (e) { /* ignore */ }
  }
  current = null;

  // Nav active state (class + aria-current for assistive tech)
  document.querySelectorAll('.nav-item[data-nav]').forEach((el) => {
    const nav = el.getAttribute('data-nav');
    const active = nav === '/'
      ? h.path === '/'
      : h.path === nav || h.path.indexOf(nav + '/') === 0 ||
        (nav === '/library' && h.path.indexOf('/paper') === 0);
    el.classList.toggle('active', active);
    if (active) {
      el.setAttribute('aria-current', 'page');
    } else {
      el.removeAttribute('aria-current');
    }
  });
  document.getElementById('sidebar').classList.remove('open');

  const title = typeof m.def.title === 'function' ? m.def.title(m.params) : m.def.title;
  document.getElementById('topbar-title').textContent = title || 'Papered';
  document.title = (title ? title + ' · ' : '') + 'Papered';

  app.classList.toggle('wide', !!m.def.wide);
  app.innerHTML = '';
  const destroy = m.def.render(app, { params: m.params, query: h.query });
  current = { def: m.def, destroy: typeof destroy === 'function' ? destroy : null };
}

export function start() {
  window.addEventListener('hashchange', render);
  render();
}
