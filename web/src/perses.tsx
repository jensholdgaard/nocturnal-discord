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
  dynamicImportPluginLoader,
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

const pluginLoader = dynamicImportPluginLoader([
  { resource: prometheusPlugin.getPluginModule() as PluginModuleResource, importPlugin: () => Promise.resolve(prometheusPlugin) },
  { resource: timeseriesChartPlugin.getPluginModule() as PluginModuleResource, importPlugin: () => Promise.resolve(timeseriesChartPlugin) },
  { resource: barChartPlugin.getPluginModule() as PluginModuleResource, importPlugin: () => Promise.resolve(barChartPlugin) },
  { resource: statChartPlugin.getPluginModule() as PluginModuleResource, importPlugin: () => Promise.resolve(statChartPlugin) },
]);

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
