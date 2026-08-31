// The Perses island. The bot renders every page server-side and leaves
// placeholders where a chart goes:
//
//   <div data-panel='{"kind":"TimeSeriesChart","title":"...","query":"...",
//                     "seriesNameFormat":"{{x}}","start":<ms>,"end":<ms>,"height":260}'></div>
//
// This bundle finds them and mounts a real Perses panel into each, querying
// Prometheus through the Perses proxy on this origin with the viewer's login.
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

document.querySelectorAll<HTMLElement>('[data-panel]').forEach(mount);
