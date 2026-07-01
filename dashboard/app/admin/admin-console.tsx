"use client";

import type * as React from "react";
import { useEffect, useMemo, useState } from "react";
import {
  Download,
  Eye,
  FileClock,
  Filter,
  KeyRound,
  Loader2,
  PauseCircle,
  PlayCircle,
  RefreshCw,
  RotateCcw,
  Save,
  Shield,
  Trash2,
  Unplug,
} from "lucide-react";
import { DatabasePlugConnectedRegular } from "@fluentui/react-icons";
import { ProgressBar } from "@tremor/react";
import { AdminShell } from "@/components/layout/admin-shell";
import { MetricCard } from "@/components/metric-card";
import { EmptyState, ErrorState, LoadingState } from "@/components/state-block";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { adminApi, adminPage, adminText, downloadBlob, qs } from "@/lib/api";
import type {
  AdminSession,
  AdminStatus,
  Alert,
  AuditLog,
  ConfigFieldSchema,
  EventLog,
  MaintenanceStatus,
  PageResult,
  RequestLog,
  ServerConfig,
  Tunnel,
  UpgradeStatus,
} from "@/lib/types";
import { formatBytes, formatDuration, formatNumber } from "@/lib/utils";

type Snapshot = {
  status: AdminStatus;
  tunnels: PageResult<Tunnel>;
  requests: PageResult<RequestLog>;
  events: PageResult<EventLog>;
  audit: PageResult<AuditLog>;
  sessions: AdminSession[];
  alerts: Alert[];
  config: ServerConfig;
  schema: ConfigFieldSchema[];
  maintenance: MaintenanceStatus;
  upgrade: UpgradeStatus;
  version: unknown;
  metrics: string;
};

const pageSize = 50;
const tabValues = ["overview", "tunnels", "activity", "security", "config", "maintenance", "version"] as const;
type AdminTab = (typeof tabValues)[number];

const configPages = [
  { value: "core", label: "Core", categories: ["Core"] },
  { value: "storage", label: "Storage", categories: ["Storage"] },
  { value: "tunnel", label: "Tunnel", categories: ["Tunnel Limits"] },
  { value: "security", label: "Security", categories: ["Security"] },
  { value: "observability", label: "Observability", categories: ["Observability"] },
  { value: "retention", label: "Retention", categories: ["Retention"] },
  { value: "inspector", label: "Inspector", categories: ["Inspector"] },
  { value: "upgrade", label: "Upgrade", categories: ["Upgrade"] },
  { value: "raw", label: "Raw", categories: [] },
] as const;
type ConfigPage = (typeof configPages)[number]["value"];

function normalizeTab(value: string | null): AdminTab {
  return tabValues.includes(value as AdminTab) ? (value as AdminTab) : "overview";
}

function normalizeConfigPage(value: string | null): ConfigPage {
  return configPages.some((page) => page.value === value) ? (value as ConfigPage) : "core";
}

export function AdminConsole() {
  const [tab, setTab] = useState<AdminTab>("overview");
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [message, setMessage] = useState("");
  const [query, setQuery] = useState({ tunnel: "", request: "", log: "", audit: "" });
  const [offsets, setOffsets] = useState({ tunnels: 0, requests: 0, events: 0, audit: 0 });
  const [configDraft, setConfigDraft] = useState<ServerConfig>({});
  const [configText, setConfigText] = useState("");
  const [detail, setDetail] = useState<unknown>(null);

  useEffect(() => {
    const syncTab = () => {
      const params = new URLSearchParams(window.location.search);
      setTab(normalizeTab(params.get("tab")));
    };
    syncTab();
    window.addEventListener("popstate", syncTab);
    return () => window.removeEventListener("popstate", syncTab);
  }, []);

  function selectTab(value: string) {
    const next = normalizeTab(value);
    setTab(next);
    const url = new URL(window.location.href);
    if (next === "overview") {
      url.searchParams.delete("tab");
    } else {
      url.searchParams.set("tab", next);
    }
    window.history.pushState({}, "", `${url.pathname}${url.search}${url.hash}`);
  }

  useEffect(() => {
    load().catch((err) => setError(err instanceof Error ? err.message : String(err)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [offsets]);

  async function load() {
    setError("");
    const [status, tunnels, config, schema, version, upgrade, requests, events, audit, sessions, maintenance, alerts, metrics] =
      await Promise.all([
        adminApi<AdminStatus>("/api/admin/status"),
        adminPage<Tunnel>("/api/admin/tunnels" + qs({ limit: pageSize, offset: offsets.tunnels, q: query.tunnel })),
        adminApi<ServerConfig>("/api/admin/config"),
        adminApi<ConfigFieldSchema[]>("/api/admin/config/schema"),
        adminApi<unknown>("/api/admin/version/full"),
        adminApi<UpgradeStatus>("/api/admin/upgrade/status"),
        adminPage<RequestLog>("/api/admin/requests" + qs({ limit: pageSize, offset: offsets.requests, q: query.request })),
        adminPage<EventLog>("/api/admin/events" + qs({ limit: pageSize, offset: offsets.events, q: query.log })),
        adminPage<AuditLog>("/api/admin/audit" + qs({ limit: pageSize, offset: offsets.audit, q: query.audit })),
        adminApi<AdminSession[]>("/api/admin/sessions"),
        adminApi<MaintenanceStatus>("/api/admin/maintenance"),
        adminApi<Alert[]>("/api/admin/alerts"),
        adminText("/metrics").catch(() => ""),
      ]);
    setSnapshot({ status, tunnels, config, schema, version, upgrade, requests, events, audit, sessions, maintenance, alerts, metrics });
    setConfigDraft(config);
    setConfigText(JSON.stringify(config, null, 2));
  }

  async function run(label: string, action: () => Promise<void>) {
    setBusy(label);
    setError("");
    setMessage("");
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy("");
    }
  }

  async function mutate(label: string, action: () => Promise<void>, refresh = true) {
    await run(label, async () => {
      await action();
      if (refresh) await load();
      setMessage(`${label} completed.`);
    });
  }

  async function logout() {
    try {
      await adminApi<void>("/api/admin/logout", { method: "POST" });
    } finally {
      window.location.href = "/admin/login";
    }
  }

  const errorRate = useMemo(() => {
    const status = snapshot?.status;
    if (!status || status.request_count === 0) return 0;
    return Math.min(100, Math.round((status.error_count / status.request_count) * 100));
  }, [snapshot]);

  return (
    <AdminShell onLogout={logout} activeTab={tab} onTabChange={selectTab}>
      <div className="grid gap-4">
        {error ? <ErrorState message={error} /> : null}
        {message ? <Card className="border-emerald-500/40 bg-emerald-500/5"><CardContent className="p-3 text-sm text-emerald-700 dark:text-emerald-200">{message}</CardContent></Card> : null}
        {!snapshot ? <LoadingState label="Loading admin console" /> : null}
        {snapshot ? (
          <Tabs value={tab} onValueChange={selectTab}>
            <TabsContent value="overview">
              <Overview snapshot={snapshot} errorRate={errorRate} reload={() => mutate("Refresh", load, false)} busy={busy} />
            </TabsContent>
            <TabsContent value="tunnels">
              <Tunnels
                data={snapshot.tunnels}
                query={query.tunnel}
                setQuery={(value) => setQuery((current) => ({ ...current, tunnel: value }))}
                page={(direction) => setOffsets((current) => ({ ...current, tunnels: Math.max(0, current.tunnels + direction * pageSize) }))}
                filter={() => {
                  setOffsets((current) => ({ ...current, tunnels: 0 }));
                }}
                busy={busy}
                action={(label, action) => mutate(label, action)}
                setDetail={setDetail}
              />
              <DetailPanel detail={detail} />
            </TabsContent>
            <TabsContent value="activity">
              <ActivityTables
                snapshot={snapshot}
                query={query}
                setQuery={setQuery}
                page={(name, direction) => setOffsets((current) => ({ ...current, [name]: Math.max(0, current[name] + direction * pageSize) }))}
                action={(label, action) => mutate(label, action, false)}
                setDetail={setDetail}
              />
              <DetailPanel detail={detail} />
            </TabsContent>
            <TabsContent value="security">
              <Security sessions={snapshot.sessions} action={(label, action) => mutate(label, action)} busy={busy} />
            </TabsContent>
            <TabsContent value="config">
              <ConfigEditor
                config={configDraft}
                configText={configText}
                schema={snapshot.schema}
                setConfig={setConfigDraft}
                setConfigText={setConfigText}
                action={(label, action) => mutate(label, action)}
                busy={busy}
              />
            </TabsContent>
            <TabsContent value="maintenance">
              <Maintenance data={snapshot.maintenance} action={(label, action) => mutate(label, action)} busy={busy} />
            </TabsContent>
            <TabsContent value="version">
              <VersionPanel version={snapshot.version} upgrade={snapshot.upgrade} action={(label, action) => mutate(label, action, false)} busy={busy} />
            </TabsContent>
          </Tabs>
        ) : null}
      </div>
    </AdminShell>
  );
}

function Overview({ snapshot, errorRate, reload, busy }: { snapshot: Snapshot; errorRate: number; reload: () => void; busy: string }) {
  return (
    <div className="grid gap-4">
      <section className="grid gap-3 md:grid-cols-2 xl:grid-cols-5">
        <MetricCard label="active sessions" value={formatNumber(snapshot.status.active_sessions)} tone="green" />
        <MetricCard label="requests" value={formatNumber(snapshot.status.request_count)} tone="blue" />
        <MetricCard label="errors" value={formatNumber(snapshot.status.error_count)} tone={snapshot.status.error_count ? "red" : "muted"} />
        <MetricCard label="uptime" value={formatDuration(snapshot.status.uptime_seconds)} tone="blue" />
        <MetricCard label="pending restart" value={snapshot.status.pending_restart ? "yes" : "no"} tone={snapshot.status.pending_restart ? "amber" : "muted"} />
      </section>
      <section className="grid gap-4 lg:grid-cols-[1fr_.8fr]">
        <Card>
          <CardHeader>
            <CardTitle>Runtime pressure</CardTitle>
            <Button variant="outline" size="sm" onClick={reload} disabled={busy === "Refresh"}>
              {busy === "Refresh" ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
              Refresh
            </Button>
          </CardHeader>
          <CardContent className="grid gap-4">
            <div>
              <div className="mb-2 flex justify-between text-sm">
                <span className="text-muted-foreground">Request error rate</span>
                <span>{errorRate}%</span>
              </div>
              <ProgressBar value={errorRate} color={errorRate > 10 ? "red" : errorRate > 0 ? "amber" : "emerald"} />
            </div>
            <MetricRows
              rows={[
                ["tunnels", snapshot.tunnels.meta.total],
                ["admin sessions", snapshot.sessions.filter((session) => session.active).length],
                ["database", snapshot.maintenance.database_path || ""],
                ["database bytes", formatBytes(snapshot.maintenance.database_size_bytes)],
                ["wal bytes", formatBytes(snapshot.maintenance.wal_size_bytes)],
              ]}
            />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Alerts</CardTitle>
          </CardHeader>
          <CardContent>
            {snapshot.alerts.length ? (
              <Table>
                <TableBody>
                  {snapshot.alerts.map((alert) => (
                    <TableRow key={alert.code}>
                      <TableCell><Badge variant={alert.severity === "critical" ? "danger" : "warning"}>{alert.severity}</Badge></TableCell>
                      <TableCell>{alert.code}</TableCell>
                      <TableCell className="text-muted-foreground">{alert.message}</TableCell>
                      <TableCell>{alert.count ?? ""}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            ) : (
              <EmptyState label="No active alerts" />
            )}
          </CardContent>
        </Card>
      </section>
    </div>
  );
}

function Tunnels({
  data,
  query,
  setQuery,
  filter,
  page,
  action,
  busy,
  setDetail,
}: {
  data: PageResult<Tunnel>;
  query: string;
  setQuery: (value: string) => void;
  filter: () => void;
  page: (direction: number) => void;
  action: (label: string, action: () => Promise<void>) => void;
  busy: string;
  setDetail: (detail: unknown) => void;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Tunnels</CardTitle>
        <Toolbar query={query} setQuery={setQuery} filter={filter} page={page} meta={data.meta} />
      </CardHeader>
      <CardContent>
        {data.data.length ? (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Subdomain</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Access</TableHead>
                <TableHead>Expires</TableHead>
                <TableHead className="text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.data.map((tunnel) => (
                <TableRow key={tunnel.id}>
                  <TableCell className="font-medium">{tunnel.subdomain}</TableCell>
                  <TableCell><StatusBadge status={tunnel.enabled ? tunnel.status : "disabled"} /></TableCell>
                  <TableCell className="text-muted-foreground">{tunnel.access_policy || "public"}</TableCell>
                  <TableCell className="text-muted-foreground">{tunnel.expires_at}</TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-1">
                      <IconAction label="Detail" icon={<Eye />} onClick={() => action("Load detail", async () => setDetail(await adminApi(`/api/admin/tunnels/${tunnel.id}/detail`)))} />
                      <IconAction label="Inspector" icon={<Shield />} onClick={() => action("Toggle inspector", () => adminApi(`/api/admin/tunnels/${tunnel.id}`, { method: "PATCH", body: JSON.stringify({ inspector_enabled: !tunnel.inspector_enabled }) }))} />
                      <IconAction label="Expire" icon={<PauseCircle />} danger onClick={() => confirmAction("Expire this tunnel now?", () => action("Expire tunnel", () => adminApi(`/api/admin/tunnels/${tunnel.id}`, { method: "PATCH", body: JSON.stringify({ expire_now: true }) })))} />
                      <IconAction label="Disconnect" icon={<Unplug />} onClick={() => confirmAction("Disconnect this tunnel?", () => action("Disconnect tunnel", () => adminApi(`/api/admin/tunnels/${tunnel.id}/disconnect`, { method: "POST" })))} />
                      <IconAction label="Disable" icon={<PauseCircle />} onClick={() => confirmAction("Disable this tunnel?", () => action("Disable tunnel", () => adminApi(`/api/admin/tunnels/${tunnel.id}/disable`, { method: "POST" })))} />
                      <IconAction label="Enable" icon={<PlayCircle />} onClick={() => action("Enable tunnel", () => adminApi(`/api/admin/tunnels/${tunnel.id}/enable`, { method: "POST" }))} />
                      <IconAction label="Rotate token" icon={<RotateCcw />} onClick={() => confirmAction("Rotate token and disconnect active client?", () => action("Rotate tunnel token", async () => setDetail(await adminApi(`/api/admin/tunnels/${tunnel.id}/token/rotate`, { method: "POST" }))))} />
                      <IconAction label="Delete" icon={<Trash2 />} danger onClick={() => confirmAction("Delete this tunnel?", () => action("Delete tunnel", () => adminApi(`/api/admin/tunnels/${tunnel.id}`, { method: "DELETE" })))} />
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        ) : (
          <EmptyState label="No tunnels match the current filter" />
        )}
        {busy ? <p className="mt-3 text-xs text-muted-foreground">{busy}...</p> : null}
      </CardContent>
    </Card>
  );
}

function ActivityTables({
  snapshot,
  query,
  setQuery,
  page,
  action,
  setDetail,
}: {
  snapshot: Snapshot;
  query: { tunnel: string; request: string; log: string; audit: string };
  setQuery: React.Dispatch<React.SetStateAction<{ tunnel: string; request: string; log: string; audit: string }>>;
  page: (name: "requests" | "events" | "audit", direction: number) => void;
  action: (label: string, action: () => Promise<void>) => void;
  setDetail: (detail: unknown) => void;
}) {
  return (
    <div className="grid gap-4">
      <LogTable
        title="Requests"
        query={query.request}
        setQuery={(value) => setQuery((current) => ({ ...current, request: value }))}
        meta={snapshot.requests.meta}
        page={(direction) => page("requests", direction)}
      >
        <div className="mb-3 flex flex-wrap gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              action("Export request CSV page", async () =>
                saveResponse(
                  await downloadBlob(
                    "/api/admin/requests/export" +
                      qs({ limit: snapshot.requests.meta.limit, offset: snapshot.requests.meta.offset, q: query.request }),
                  ),
                  "http-tunnel-requests.csv",
                ),
              )
            }
          >
            <Download className="h-4 w-4" />
            CSV page
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              action("Export filtered requests", async () =>
                saveResponse(
                  await downloadBlob("/api/admin/requests/export" + qs({ all: true, q: query.request })),
                  "http-tunnel-requests-filtered.csv",
                ),
              )
            }
          >
            <Download className="h-4 w-4" />
            CSV filtered
          </Button>
        </div>
        <Table>
          <TableHeader><TableRow><TableHead>Type</TableHead><TableHead>Method</TableHead><TableHead>Path</TableHead><TableHead>Status</TableHead><TableHead>Error</TableHead><TableHead className="text-right">Actions</TableHead></TableRow></TableHeader>
          <TableBody>
            {snapshot.requests.data.map((row) => (
              <TableRow key={row.id}>
                <TableCell>{row.request_type || "http"}</TableCell>
                <TableCell>{row.method || ""}</TableCell>
                <TableCell className="max-w-md truncate text-muted-foreground">{row.path || ""}</TableCell>
                <TableCell>{row.status ?? ""}</TableCell>
                <TableCell className="text-red-700 dark:text-red-200">{row.error || row.ws_close_reason || ""}</TableCell>
                <TableCell><div className="flex justify-end gap-1"><IconAction label="Detail" icon={<Eye />} onClick={() => action("Request detail", async () => setDetail(await adminApi(`/api/admin/requests/${row.id}`)))} /><IconAction label="Replay" icon={<RotateCcw />} onClick={() => action("Replay request", async () => setDetail(await adminApi(`/api/admin/requests/${row.id}/replay`, { method: "POST" })))} /></div></TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </LogTable>
      <div className="grid gap-4 lg:grid-cols-2">
        <LogTable title="Events" query={query.log} setQuery={(value) => setQuery((current) => ({ ...current, log: value }))} meta={snapshot.events.meta} page={(direction) => page("events", direction)}>
          <SimpleRows rows={snapshot.events.data.map((event) => [event.kind, event.message || "", event.created_at])} />
        </LogTable>
        <LogTable title="Audit" query={query.audit} setQuery={(value) => setQuery((current) => ({ ...current, audit: value }))} meta={snapshot.audit.meta} page={(direction) => page("audit", direction)}>
          <div className="mb-3 flex flex-wrap gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                action("Export audit CSV page", async () =>
                  saveResponse(
                    await downloadBlob(
                      "/api/admin/audit/export" +
                        qs({ limit: snapshot.audit.meta.limit, offset: snapshot.audit.meta.offset, q: query.audit }),
                    ),
                    "http-tunnel-audit.csv",
                  ),
                )
              }
            >
              <Download className="h-4 w-4" />
              CSV page
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                action("Export filtered audit", async () =>
                  saveResponse(
                    await downloadBlob("/api/admin/audit/export" + qs({ all: true, q: query.audit })),
                    "http-tunnel-audit-filtered.csv",
                  ),
                )
              }
            >
              <Download className="h-4 w-4" />
              CSV filtered
            </Button>
          </div>
          <SimpleRows rows={snapshot.audit.data.map((audit) => [audit.action, audit.result, audit.detail || audit.created_at])} />
        </LogTable>
      </div>
    </div>
  );
}

function Security({ sessions, action, busy }: { sessions: AdminSession[]; action: (label: string, action: () => Promise<void>) => void; busy: string }) {
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader><CardTitle>Admin sessions</CardTitle><Button variant="destructive" size="sm" onClick={() => confirmAction("Revoke all other active sessions?", () => action("Revoke other sessions", () => adminApi("/api/admin/sessions/revoke-all", { method: "POST" })))}>Revoke other sessions</Button></CardHeader>
        <CardContent>
          <Table>
            <TableHeader><TableRow><TableHead>ID</TableHead><TableHead>IP</TableHead><TableHead>User agent</TableHead><TableHead>Last seen</TableHead><TableHead>Status</TableHead><TableHead /></TableRow></TableHeader>
            <TableBody>
              {sessions.map((session) => (
                <TableRow key={session.id}>
                  <TableCell className="font-mono text-xs">{session.id}</TableCell>
                  <TableCell>{session.remote_ip || ""}</TableCell>
                  <TableCell className="max-w-sm truncate text-muted-foreground">{session.user_agent || ""}</TableCell>
                  <TableCell>{session.last_seen_at}</TableCell>
                  <TableCell><Badge variant={session.active ? "healthy" : "muted"}>{session.current ? "current" : session.active ? "active" : "inactive"}</Badge></TableCell>
                  <TableCell className="text-right"><Button variant="outline" size="sm" onClick={() => confirmAction("Revoke this admin session?", () => action("Revoke session", () => adminApi(`/api/admin/sessions/${session.id}/revoke`, { method: "POST" })))}>Revoke</Button></TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>Password</CardTitle></CardHeader>
        <CardContent className="grid gap-3 md:grid-cols-[1fr_1fr_1fr_auto]">
          <Input type="password" placeholder="Current password" value={currentPassword} onChange={(event) => setCurrentPassword(event.target.value)} />
          <Input type="password" placeholder="New password" value={newPassword} onChange={(event) => setNewPassword(event.target.value)} />
          <Input type="password" placeholder="Confirm password" value={confirmPassword} onChange={(event) => setConfirmPassword(event.target.value)} />
          <Button disabled={busy === "Change password"} onClick={() => action("Change password", async () => {
            await adminApi("/api/admin/password", { method: "POST", body: JSON.stringify({ current_password: currentPassword, new_password: newPassword, confirm_password: confirmPassword }) });
            setCurrentPassword(""); setNewPassword(""); setConfirmPassword("");
          })}><KeyRound className="h-4 w-4" />Change</Button>
        </CardContent>
      </Card>
    </div>
  );
}

function ConfigEditor({
  config,
  configText,
  schema,
  setConfig,
  setConfigText,
  action,
  busy,
}: {
  config: ServerConfig;
  configText: string;
  schema: ConfigFieldSchema[];
  setConfig: (config: ServerConfig) => void;
  setConfigText: (value: string) => void;
  action: (label: string, action: () => Promise<void>) => void;
  busy: string;
}) {
  const [configPage, setConfigPage] = useState<ConfigPage>("core");
  useEffect(() => {
    const syncConfigPage = () => {
      const params = new URLSearchParams(window.location.search);
      setConfigPage(normalizeConfigPage(params.get("config")));
    };
    syncConfigPage();
    window.addEventListener("popstate", syncConfigPage);
    return () => window.removeEventListener("popstate", syncConfigPage);
  }, []);

  function update(key: string, value: unknown) {
    const next = { ...config, [key]: value };
    setConfig(next);
    setConfigText(JSON.stringify(next, null, 2));
  }

  function selectConfigPage(value: string) {
    const next = normalizeConfigPage(value);
    setConfigPage(next);
    const url = new URL(window.location.href);
    url.searchParams.set("tab", "config");
    if (next === "core") {
      url.searchParams.delete("config");
    } else {
      url.searchParams.set("config", next);
    }
    window.history.pushState({}, "", `${url.pathname}${url.search}${url.hash}`);
  }

  const groupedFields = useMemo(() => {
    const groups = new Map<string, ConfigFieldSchema[]>();
    for (const field of schema) {
      const fields = groups.get(field.category) ?? [];
      fields.push(field);
      groups.set(field.category, fields);
    }
    return groups;
  }, [schema]);
  const currentPage = configPages.find((page) => page.value === configPage) ?? configPages[0];
  const currentFields = currentPage.categories.flatMap((category) => groupedFields.get(category) ?? []);

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader className="flex-wrap items-start">
          <div className="grid gap-1">
            <CardTitle>Runtime config</CardTitle>
            <div className="text-xs text-muted-foreground">{currentPage.label}</div>
          </div>
          <div className="flex shrink-0 gap-2">
            <Button variant="outline" size="sm" onClick={() => action("Validate config", () => adminApi("/api/admin/config/validate", { method: "POST", body: JSON.stringify(config) }))}>Validate</Button>
            <Button size="sm" disabled={busy === "Save config"} onClick={() => action("Save config", () => adminApi("/api/admin/config", { method: "PUT", body: JSON.stringify(config) }))}><Save className="h-4 w-4" />Save</Button>
          </div>
        </CardHeader>
        <CardContent className="grid gap-4">
          <Tabs value={configPage} onValueChange={selectConfigPage}>
            <TabsList className="w-full flex-nowrap justify-start overflow-x-auto rounded-md">
              {configPages.map((page) => {
                const count = page.value === "raw" ? schema.length : page.categories.reduce((sum, category) => sum + (groupedFields.get(category)?.length ?? 0), 0);
                return (
                  <TabsTrigger key={page.value} value={page.value} className="shrink-0 gap-2">
                    <span>{page.label}</span>
                    <span className="rounded-md border border-border bg-secondary px-1.5 py-0.5 text-[11px] text-muted-foreground">{count}</span>
                  </TabsTrigger>
                );
              })}
            </TabsList>
            {configPages.map((page) => {
              const fields = page.categories.flatMap((category) => groupedFields.get(category) ?? []);
              return (
                <TabsContent key={page.value} value={page.value}>
                  {page.value === "raw" ? (
                    <Textarea
                      className="min-h-96 font-mono"
                      value={configText}
                      onChange={(event) => {
                        setConfigText(event.target.value);
                        try { setConfig(JSON.parse(event.target.value)); } catch (_) {}
                      }}
                    />
                  ) : (
                    <ConfigFieldGrid fields={fields} config={config} update={update} />
                  )}
                </TabsContent>
              );
            })}
          </Tabs>
          {configPage !== "raw" && !currentFields.length ? <EmptyState label="No config fields in this category" /> : null}
        </CardContent>
      </Card>
    </div>
  );
}

function ConfigFieldGrid({
  fields,
  config,
  update,
}: {
  fields: ConfigFieldSchema[];
  config: ServerConfig;
  update: (key: string, value: unknown) => void;
}) {
  return (
    <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
      {fields.map((field) => (
        <label key={field.key} className="grid gap-1 text-sm">
          <span className="flex items-center justify-between gap-2">
            <span>{field.key}</span>
            {field.restart_required ? <Badge variant="warning">restart</Badge> : null}
          </span>
          {field.allowed_values?.length ? (
            <Select value={String(config[field.key] ?? "")} onChange={(event) => update(field.key, parseConfigValue(field, event.target.value))}>
              {field.allowed_values.map((value) => <option key={value} value={value}>{value}</option>)}
            </Select>
          ) : isTextArea(field) ? (
            <Textarea value={fieldValue(config[field.key])} onChange={(event) => update(field.key, parseConfigValue(field, event.target.value))} />
          ) : (
            <Input value={fieldValue(config[field.key])} onChange={(event) => update(field.key, parseConfigValue(field, event.target.value))} />
          )}
          <span className="text-xs text-muted-foreground">{field.description}</span>
        </label>
      ))}
    </div>
  );
}

function Maintenance({ data, action, busy }: { data: MaintenanceStatus; action: (label: string, action: () => Promise<void>) => void; busy: string }) {
  return (
    <div className="grid gap-4">
      <section className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
        <MetricCard label="database" value={formatBytes(data.database_size_bytes)} detail={data.database_path || ""} />
        <MetricCard label="wal" value={formatBytes(data.wal_size_bytes)} />
        <MetricCard label="requests" value={formatNumber(data.request_log_count)} />
        <MetricCard label="runtime sessions" value={formatNumber(data.active_runtime_sessions)} tone="green" />
      </section>
      <Card>
        <CardHeader><CardTitle className="flex items-center gap-2"><DatabasePlugConnectedRegular className="h-5 w-5" />Maintenance actions</CardTitle></CardHeader>
        <CardContent className="flex flex-wrap gap-2">
          <Button variant="outline" disabled={!!busy} onClick={() => confirmAction("Run cleanup now?", () => action("Run cleanup", () => adminApi("/api/admin/cleanup", { method: "POST" })))}>Run cleanup</Button>
          <Button variant="outline" disabled={!!busy} onClick={() => action("WAL checkpoint", () => adminApi("/api/admin/maintenance/wal-checkpoint", { method: "POST" }))}>WAL checkpoint</Button>
          <Button variant="outline" disabled={!!busy} onClick={() => action("Analyze", () => adminApi("/api/admin/maintenance/analyze", { method: "POST" }))}>Analyze</Button>
          <Button variant="outline" disabled={!!busy} onClick={() => action("Vacuum", () => adminApi("/api/admin/maintenance/vacuum", { method: "POST" }))}>Vacuum</Button>
          <Button variant="outline" disabled={!!busy} onClick={() => action("Download backup", async () => saveResponse(await downloadBlob("/api/admin/backup", { method: "POST" }), "http-tunnel-backup.zip"))}><Download className="h-4 w-4" />Backup</Button>
          <Button variant="outline" disabled={!!busy} onClick={() => action("Diagnostics", async () => saveResponse(await downloadBlob("/api/admin/diagnostics/export"), "http-tunnel-diagnostics.json"))}><Download className="h-4 w-4" />Diagnostics</Button>
        </CardContent>
      </Card>
    </div>
  );
}

function VersionPanel({ version, upgrade, action, busy }: { version: unknown; upgrade: UpgradeStatus; action: (label: string, action: () => Promise<void>) => void; busy: string }) {
  return (
    <Card>
      <CardHeader><CardTitle className="flex items-center gap-2"><FileClock className="h-4 w-4" />Version and upgrade</CardTitle></CardHeader>
      <CardContent className="grid gap-4">
        <pre className="overflow-auto rounded-md border border-border bg-background p-3 text-xs text-muted-foreground">{JSON.stringify(version, null, 2)}</pre>
        <MetricRows
          rows={[
            ["auto upgrade", upgrade.auto_upgrade_enabled ? "enabled" : "disabled"],
            ["release repo", upgrade.effective_release_repo],
            ["release tag", upgrade.release_tag],
            ["latest tag", upgrade.latest_tag || "not checked"],
            ["update", upgrade.update_available == null ? "unknown" : upgrade.update_available ? "available" : "none"],
            ["last check", upgrade.last_checked_at || "never"],
            ["last result", upgrade.last_result || "none"],
            ["idle window", `${upgrade.idle_window_seconds}s`],
            ["restart methods", upgrade.restart_methods.join(", ") || "external"],
          ]}
        />
        {upgrade.last_message ? <p className="text-sm text-muted-foreground">{upgrade.last_message}</p> : null}
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" disabled={!!busy || upgrade.upgrade_in_progress} onClick={() => confirmAction("Upgrade the server binary now?", () => action("Upgrade", async () => alert(JSON.stringify(await adminApi("/api/admin/upgrade", { method: "POST" }), null, 2))))}>Upgrade now</Button>
          <Button variant="destructive" disabled={!!busy} onClick={() => confirmAction("Restart the server?", () => action("Restart", async () => alert(JSON.stringify(await adminApi("/api/admin/restart", { method: "POST" }), null, 2))))}>Restart</Button>
        </div>
      </CardContent>
    </Card>
  );
}

function Toolbar({ query, setQuery, filter, page, meta }: { query: string; setQuery: (value: string) => void; filter: () => void; page: (direction: number) => void; meta: { offset: number; limit: number; total: number; hasMore: boolean } }) {
  return (
    <div className="flex flex-wrap items-center justify-end gap-2">
      <Input className="h-8 w-48" placeholder="Search" value={query} onChange={(event) => setQuery(event.target.value)} />
      <Button variant="outline" size="sm" onClick={filter}><Filter className="h-4 w-4" />Filter</Button>
      <Button variant="outline" size="sm" disabled={meta.offset <= 0} onClick={() => page(-1)}>Prev</Button>
      <Button variant="outline" size="sm" disabled={!meta.hasMore} onClick={() => page(1)}>Next</Button>
      <span className="text-xs text-muted-foreground">{meta.offset + 1}-{Math.min(meta.offset + meta.limit, meta.total)} / {meta.total}</span>
    </div>
  );
}

function LogTable({ title, query, setQuery, meta, page, children }: { title: string; query: string; setQuery: (value: string) => void; meta: PageResult<unknown>["meta"]; page: (direction: number) => void; children: React.ReactNode }) {
  return (
    <Card>
      <CardHeader><CardTitle>{title}</CardTitle><Toolbar query={query} setQuery={setQuery} filter={() => page(0)} page={page} meta={meta} /></CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

function SimpleRows({ rows }: { rows: Array<Array<string | number>> }) {
  if (!rows.length) return <EmptyState label="No records" />;
  return (
    <Table>
      <TableBody>
        {rows.map((row, index) => (
          <TableRow key={index}>{row.map((cell, cellIndex) => <TableCell key={cellIndex} className={cellIndex === 2 ? "text-muted-foreground" : ""}>{cell}</TableCell>)}</TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function MetricRows({ rows }: { rows: Array<[string, React.ReactNode]> }) {
  return (
    <Table>
      <TableBody>
        {rows.map(([label, value]) => <TableRow key={label}><TableCell className="w-48 text-muted-foreground">{label}</TableCell><TableCell>{value}</TableCell></TableRow>)}
      </TableBody>
    </Table>
  );
}

function DetailPanel({ detail }: { detail: unknown }) {
  if (!detail) return null;
  return (
    <Card className="mt-4">
      <CardHeader><CardTitle>Detail</CardTitle></CardHeader>
      <CardContent><pre className="max-h-[520px] overflow-auto rounded-md border border-border bg-background p-3 text-xs text-muted-foreground">{JSON.stringify(detail, null, 2)}</pre></CardContent>
    </Card>
  );
}

function StatusBadge({ status }: { status: string }) {
  const value = status.toLowerCase();
  const variant = value === "connected" || value === "active" ? "healthy" : value === "disabled" || value === "expired" ? "warning" : value === "deleted" ? "danger" : "muted";
  return <Badge variant={variant}>{status}</Badge>;
}

function IconAction({ label, icon, onClick, danger }: { label: string; icon: React.ReactElement; onClick: () => void; danger?: boolean }) {
  return (
    <Button title={label} aria-label={label} variant={danger ? "destructive" : "outline"} size="icon" onClick={onClick}>
      {icon}
    </Button>
  );
}

function confirmAction(message: string, action: () => void) {
  if (window.confirm(message)) action();
}

function fieldValue(value: unknown) {
  if (Array.isArray(value)) return value.join("\n");
  if (value === null || value === undefined) return "";
  return String(value);
}

function isTextArea(field: ConfigFieldSchema) {
  return field.value_type.includes("list") || field.value_type.includes("array") || field.key.includes("cidrs") || field.key.includes("subdomains");
}

function parseConfigValue(field: ConfigFieldSchema, value: string) {
  if (field.value_type.includes("bool")) return value === "true";
  if (field.value_type.includes("number") || field.value_type.includes("integer") || field.key.endsWith("_seconds") || field.key.endsWith("_bytes") || field.key.includes("limit")) {
    return Number(value || 0);
  }
  if (isTextArea(field)) {
    return value.split(/\n|,/).map((item) => item.trim()).filter(Boolean);
  }
  if (field.key === "domain" && !value.trim()) return null;
  if (field.key === "systemd_unit" && !value.trim()) return null;
  return value;
}

async function saveResponse(response: Response, filename: string) {
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}
