// The Perses embedding boundary: everything a Perses panel needs to render
// inside our app, built once. Per perses/docs/embedding-panels.md, with the
// datasource pointed at the Perses server's own proxy on this origin, so the
// viewer's login cookie is the credential - no token in the page.
import React, { ReactNode, useMemo } from 'react';
import { ThemeProvider } from '@mui/material';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ChartsProvider, generateChartsTheme, getTheme, SnackbarProvider } from '@perses-dev/components';
import {
  DataQueriesProvider,
  PluginModuleResource,
  PluginRegistry,
  TimeRangeProvider,
} from '@perses-dev/plugin-system';
import { DatasourceStoreProvider, Panel, VariableProvider } from '@perses-dev/dashboards';
type DatasourceApi = React.ComponentProps<typeof DatasourceStoreProvider>['datasourceApi'];
import type { DashboardResource, DatasourceResource, GlobalDatasourceResource, TimeRangeValue } from '@perses-dev/core';
import * as prometheusPlugin from '@perses-dev/prometheus-plugin';
import * as timeseriesChartPlugin from '@perses-dev/timeseries-chart-plugin';
import * as barChartPlugin from '@perses-dev/bar-chart-plugin';
import * as statChartPlugin from '@perses-dev/stat-chart-plugin';

/** Prometheus through Perses' datasource proxy for the everquest project. */
const prometheus: GlobalDatasourceResource = {
  kind: 'GlobalDatasource',
  metadata: { name: 'prometheus' },
  spec: {
    default: true,
    plugin: {
      kind: 'PrometheusDatasource',
      spec: { directUrl: '/perses/proxy/projects/everquest/datasources/prometheus' },
    },
  },
};

const datasourceApi: DatasourceApi = {
  getDatasource: () => Promise.resolve(undefined as DatasourceResource | undefined),
  getGlobalDatasource: () => Promise.resolve(prometheus),
  listDatasources: () => Promise.resolve([] as DatasourceResource[]),
  listGlobalDatasources: () => Promise.resolve([prometheus]),
  buildProxyUrl: () => '/perses/proxy/projects/everquest/datasources/prometheus',
} as DatasourceApi;
const queryClient = new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } });

// The registry resolves a plugin by `kind:name:registry:version` and then
// reads *that key* off the loaded module. The npm packages export plugins by
// plain name (`TimeSeriesChart`), and their getPluginModule() reads a
// package.json the bundler does not inline - so the doc's loader registers
// nothing. This loader declares each package's plugins (from its package.json
// `perses` block, versions pinned to the server's) and exposes every plugin
// under the compound key the registry will ask for.
type PluginDecl = { kind: string; name: string };
type Package = { name: string; version: string; plugins: PluginDecl[]; module: Record<string, unknown> };

const PACKAGES: Package[] = [
  { name: '@perses-dev/timeseries-chart-plugin', version: '0.13.0', plugins: [{ kind: 'Panel', name: 'TimeSeriesChart' }], module: timeseriesChartPlugin as unknown as Record<string, unknown> },
  { name: '@perses-dev/bar-chart-plugin', version: '0.13.0', plugins: [{ kind: 'Panel', name: 'BarChart' }], module: barChartPlugin as unknown as Record<string, unknown> },
  { name: '@perses-dev/stat-chart-plugin', version: '0.13.0', plugins: [{ kind: 'Panel', name: 'StatChart' }], module: statChartPlugin as unknown as Record<string, unknown> },
  {
    name: '@perses-dev/prometheus-plugin', version: '0.58.0',
    plugins: [
      { kind: 'Datasource', name: 'PrometheusDatasource' },
      { kind: 'TimeSeriesQuery', name: 'PrometheusTimeSeriesQuery' },
      { kind: 'Variable', name: 'PrometheusLabelValuesVariable' },
      { kind: 'Variable', name: 'PrometheusLabelNamesVariable' },
      { kind: 'Variable', name: 'PrometheusPromQLVariable' },
    ],
    module: prometheusPlugin as unknown as Record<string, unknown>,
  },
];

const compoundKey = (kind: string, name: string, version: string) => `${kind}:${name}::${version}`;

const resources: PluginModuleResource[] = PACKAGES.map((p) => ({
  kind: 'PluginModule',
  metadata: { name: p.name, version: p.version },
  spec: { plugins: p.plugins.map((d) => ({ kind: d.kind, spec: { name: d.name, display: { name: d.name } } })) },
} as unknown as PluginModuleResource));

const pluginLoader = {
  getInstalledPlugins: () => Promise.resolve(resources),
  importPluginModule: (resource: PluginModuleResource) => {
    const pkg = PACKAGES.find((p) => p.name === resource.metadata.name);
    if (!pkg) return Promise.reject(new Error(`unknown plugin package ${resource.metadata.name}`));
    const keyed: Record<string, unknown> = {};
    for (const d of pkg.plugins) keyed[compoundKey(d.kind, d.name, pkg.version)] = pkg.module[d.name];
    return Promise.resolve(keyed);
  },
};

const emptyDashboard: DashboardResource = {
  kind: 'Dashboard',
  metadata: { name: 'nocturnal-site', project: 'everquest', createdAt: '', updatedAt: '', version: 0 },
  spec: { duration: '1h', variables: [], panels: {}, layouts: [] },
};

export function PersesProviders({ children, timeRange, dark }: { children: ReactNode; timeRange: TimeRangeValue; dark: boolean }) {
  const muiTheme = useMemo(() => getTheme(dark ? 'dark' : 'light'), [dark]);
  const chartsTheme = useMemo(() => generateChartsTheme(muiTheme, {}), [muiTheme]);
  return (
    <ThemeProvider theme={muiTheme}>
      <ChartsProvider chartsTheme={chartsTheme}>
        <SnackbarProvider anchorOrigin={{ vertical: 'bottom', horizontal: 'right' }}>
          <QueryClientProvider client={queryClient}>
            <PluginRegistry pluginLoader={pluginLoader}>
              <TimeRangeProvider timeRange={timeRange} refreshInterval="0s" setTimeRange={() => undefined} setRefreshInterval={() => undefined}>
                <VariableProvider initialVariableDefinitions={[]}>
                  <DatasourceStoreProvider dashboardResource={emptyDashboard} datasourceApi={datasourceApi}>
                    {children}
                  </DatasourceStoreProvider>
                </VariableProvider>
              </TimeRangeProvider>
            </PluginRegistry>
          </QueryClientProvider>
        </SnackbarProvider>
      </ChartsProvider>
    </ThemeProvider>
  );
}

/** One Prometheus time-series panel, the way a Perses dashboard would draw it. */
export function PromPanel({ title, kind, query, seriesNameFormat, spec, height = 260 }: {
  title: string;
  kind: 'TimeSeriesChart' | 'BarChart' | 'StatChart';
  query: string;
  seriesNameFormat?: string;
  spec?: Record<string, unknown>;
  height?: number;
}) {
  const definition = useMemo(() => ({
    kind: 'Panel' as const,
    spec: {
      display: { name: title },
      plugin: { kind, spec: spec ?? {} },
    },
  }), [title, kind, spec]);
  const queries = useMemo(() => [{
    kind: 'TimeSeriesQuery',
    spec: { plugin: { kind: 'PrometheusTimeSeriesQuery', spec: { query, seriesNameFormat } } },
  }], [query, seriesNameFormat]);
  return (
    <div style={{ height }}>
      <DataQueriesProvider definitions={queries}>
        <Panel definition={definition} panelOptions={{ hideHeader: false }} />
      </DataQueriesProvider>
    </div>
  );
}
