// The Perses island. The bot renders every page server-side and leaves
// placeholders where a chart goes:
//
//   <div data-panel='{"kind":"TimeSeriesChart","title":"...","query":"...",
//                     "seriesNameFormat":"{{x}}","start":<ms>,"end":<ms>,"height":260}'></div>
//
// This bundle finds them and mounts a real Perses panel into each, querying
// Prometheus through the Perses proxy on this origin with the viewer's login —
// after one pre-flight that turns "no grant yet" into a `/dpstoken` hint
// instead of a permissions error in every chart.
import { createRoot } from 'react-dom/client';
import { PersesProviders, PromPanel } from './perses';

interface Placeholder {
  kind: 'TimeSeriesChart' | 'BarChart' | 'StatChart';
  title: string;
  query: string;
  seriesNameFormat?: string;
  start: number;
  end: number;
  height?: number;
  spec?: Record<string, unknown>;
}

function mount(el: HTMLElement) {
  let p: Placeholder;
  try {
    p = JSON.parse(el.dataset.panel ?? '') as Placeholder;
  } catch {
    el.textContent = 'This chart could not be described.';
    return;
  }
  const dark = document.documentElement.dataset.theme === 'dark'
    || (!document.documentElement.dataset.theme && window.matchMedia('(prefers-color-scheme: dark)').matches);
  createRoot(el).render(
    <PersesProviders timeRange={{ start: new Date(p.start), end: new Date(p.end) }} dark={dark}>
      <PromPanel title={p.title} kind={p.kind} query={p.query} seriesNameFormat={p.seriesNameFormat} spec={p.spec} height={p.height ?? 260} />
    </PersesProviders>,
  );
}

/// What the viewer will get from Perses before any panel asks. A member is
/// behind the login wall already, but the raid charts read Prometheus through
/// Perses, and Perses grants a logged-in member nothing until `/dpstoken` has
/// provisioned their role binding. Without this, that member sees a raw
/// "permissions error" in every chart (feedback channel, 2026-08-31) and has
/// no idea what to do about it.
async function preflight(): Promise<'ok' | 'no-grant' | 'signed-out' | 'unknown'> {
  try {
    const r = await fetch(
      '/perses/proxy/projects/everquest/datasources/prometheus/api/v1/query?query=up',
      { credentials: 'same-origin', cache: 'no-store' },
    );
    if (r.status === 403) return 'no-grant';
    if (r.status === 401) return 'signed-out';
    return 'ok';
  } catch {
    return 'unknown';
  }
}

function hint(el: HTMLElement, state: 'no-grant' | 'signed-out') {
  const box = document.createElement('div');
  box.className = 'pb';
  box.style.display = 'grid';
  box.style.placeItems = 'center';
  box.style.minHeight = el.style.minHeight || '120px';
  const p = document.createElement('p');
  p.className = 'empty';
  p.style.maxWidth = '34em';
  p.style.textAlign = 'center';
  if (state === 'no-grant') {
    p.innerHTML =
      'Live charts need your DPS-meter login. Run <b>/dpstoken</b> in Discord once, '
      + 'wait a minute for access to land, then reload this page.';
  } else {
    p.innerHTML = 'Your login has lapsed — <a href="/perses/api/v1/user/whoami">reload</a> to sign in again.';
  }
  box.appendChild(p);
  el.replaceWith(box);
}

(async () => {
  const placeholders = Array.from(document.querySelectorAll<HTMLElement>('[data-panel]'));
  if (placeholders.length === 0) return;
  const state = await preflight();
  if (state === 'no-grant' || state === 'signed-out') {
    placeholders.forEach((el) => hint(el, state));
    return;
  }
  placeholders.forEach(mount);
})();
