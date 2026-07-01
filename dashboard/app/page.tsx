"use client";

import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  BookOpen,
  ExternalLink,
  Globe2,
  MapPinned,
  Network,
  Search,
  Waves,
  X,
} from "lucide-react";
import { TopBar } from "@/components/layout/top-bar";
import { MetricCard } from "@/components/metric-card";
import { EmptyState, ErrorState, LoadingState } from "@/components/state-block";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { publicApi } from "@/lib/api";
import type { DashboardSummary, PublicTunnel, PublicTunnelMapPoint } from "@/lib/types";
import { cn, formatBytes, formatNumber } from "@/lib/utils";

type StatusFilter = "all" | "online" | "offline";

export default function PublicDashboardPage() {
  const [summary, setSummary] = useState<DashboardSummary | null>(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [lastLoadedAt, setLastLoadedAt] = useState<Date | null>(null);
  const [docsOpen, setDocsOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const data = await publicApi<DashboardSummary>("/api/v1/dashboard");
        if (!cancelled) {
          setSummary(data);
          setLastLoadedAt(new Date());
          setError("");
        }
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      }
    }
    load();
    const timer = window.setInterval(load, 5000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const filteredTunnels = useMemo(() => {
    const text = query.trim().toLowerCase();
    return (summary?.tunnels || []).filter((tunnel) => {
      const matchesStatus =
        statusFilter === "all" ||
        (statusFilter === "online" && tunnel.connected) ||
        (statusFilter === "offline" && !tunnel.connected);
      const matchesText =
        !text ||
        tunnel.subdomain.toLowerCase().includes(text) ||
        tunnel.url.toLowerCase().includes(text) ||
        tunnel.source.label.toLowerCase().includes(text);
      return matchesStatus && matchesText;
    });
  }, [query, statusFilter, summary?.tunnels]);

  const visibleMapPoints = useMemo(() => {
    if (!summary) return [];
    const visible = new Set(filteredTunnels.map((tunnel) => tunnel.subdomain));
    return summary.map_points.filter((point) => visible.has(point.subdomain));
  }, [filteredTunnels, summary]);

  const status = error
    ? "Unable to load dashboard"
    : summary?.setup_required
      ? "Setup required"
      : summary?.ready === "not_ready"
        ? "Database unavailable"
        : undefined;
  const statusTone: "warning" | "danger" = error || (summary?.ready === "not_ready" && !summary.setup_required) ? "danger" : "warning";

  return (
    <div className="ops-shell">
      <TopBar
        title="http-tunnel"
        subtitle="Tunnel overview"
        status={status}
        statusTone={statusTone}
      />
      <main className="mx-auto grid max-w-7xl gap-5 px-4 py-5">
        {error ? <ErrorState message={error} /> : null}
        {!summary && !error ? <LoadingState label="Loading tunnels" /> : null}
        {summary ? (
          <>
            <section className="grid gap-3 md:grid-cols-2 xl:grid-cols-6">
              <MetricCard label="tunnels" value={formatNumber(summary.stats.total_tunnels)} tone="blue" />
              <MetricCard label="online" value={formatNumber(summary.stats.online_tunnels)} tone="green" />
              <MetricCard label="offline" value={formatNumber(summary.stats.offline_tunnels)} tone={summary.stats.offline_tunnels ? "amber" : "muted"} />
              <MetricCard label="sessions" value={formatNumber(summary.stats.active_sessions)} tone="green" />
              <MetricCard label="requests" value={formatNumber(summary.stats.request_count)} tone="blue" />
              <MetricCard label="errors" value={formatNumber(summary.stats.error_count)} tone={summary.stats.error_count ? "red" : "muted"} />
            </section>

            <Card className="overflow-hidden">
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <MapPinned className="h-4 w-4 text-primary" />
                  Source map
                </CardTitle>
                <Badge variant="muted">{formatNumber(visibleMapPoints.length)} located</Badge>
              </CardHeader>
              <CardContent className="p-0">
                <TunnelSourceMap points={visibleMapPoints} total={filteredTunnels.length} />
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="items-start gap-3 md:flex-row md:items-center">
                <div>
                  <CardTitle className="flex items-center gap-2">
                    <Network className="h-4 w-4 text-primary" />
                    Tunnels
                  </CardTitle>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {formatNumber(filteredTunnels.length)} shown
                    {lastLoadedAt ? ` · refreshed ${formatClock(lastLoadedAt)}` : ""}
                  </p>
                </div>
                <div className="flex w-full flex-col gap-2 md:w-auto md:flex-row md:items-center">
                  <Button type="button" variant="outline" size="sm" onClick={() => setDocsOpen(true)}>
                    <BookOpen className="h-4 w-4" />
                    Docs
                  </Button>
                  <div className="relative min-w-64">
                    <Search className="pointer-events-none absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
                    <Input
                      value={query}
                      onChange={(event) => setQuery(event.target.value)}
                      placeholder="Search tunnels"
                      className="pl-8"
                    />
                  </div>
                  <div className="inline-flex h-9 overflow-hidden rounded-md border border-border bg-background">
                    {(["all", "online", "offline"] as StatusFilter[]).map((item) => (
                      <button
                        key={item}
                        type="button"
                        onClick={() => setStatusFilter(item)}
                        className={cn(
                          "px-3 text-sm text-muted-foreground transition hover:bg-secondary hover:text-foreground",
                          statusFilter === item && "bg-secondary text-foreground",
                        )}
                      >
                        {item}
                      </button>
                    ))}
                  </div>
                </div>
              </CardHeader>
              <CardContent className="p-0">
                <TunnelTable tunnels={filteredTunnels} />
              </CardContent>
            </Card>
          </>
        ) : null}
      </main>
      {docsOpen ? <ClientDocsModal serverUrl={summary?.server_url} onClose={() => setDocsOpen(false)} /> : null}
    </div>
  );
}

function ClientDocsModal({ serverUrl, onClose }: { serverUrl?: string | null; onClose: () => void }) {
  const resolvedServerUrl = serverUrl || currentOrigin();
  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/45 p-4" role="dialog" aria-modal="true" aria-labelledby="client-docs-title" onClick={onClose}>
      <div className="max-h-[90vh] w-full max-w-3xl overflow-auto rounded-lg border border-border bg-card text-card-foreground shadow-xl" onClick={(event) => event.stopPropagation()}>
        <div className="sticky top-0 flex items-center justify-between gap-3 border-b border-border bg-card px-5 py-4">
          <div>
            <h2 id="client-docs-title" className="text-base font-semibold">Client binary</h2>
            <p className="mt-1 text-xs text-muted-foreground">Download the client binary from GitHub Releases and connect directly to your local service.</p>
          </div>
          <Button type="button" variant="ghost" size="icon" onClick={onClose} aria-label="Close">
            <X className="h-4 w-4" />
          </Button>
        </div>
        <div className="grid gap-4 p-5">
          <CommandBlock
            title="Linux amd64"
            command={clientCommand(
              "https://github.com/Xiechengqi/http-tunnel/releases/download/latest/http-tunnel-client-linux-amd64",
              resolvedServerUrl,
            )}
          />
          <CommandBlock
            title="Linux arm64"
            command={clientCommand(
              "https://github.com/Xiechengqi/http-tunnel/releases/download/latest/http-tunnel-client-linux-arm64",
              resolvedServerUrl,
            )}
          />
        </div>
      </div>
    </div>
  );
}

function clientCommand(downloadUrl: string, serverUrl: string) {
  return `curl -L -o http-tunnel-client ${downloadUrl}
chmod +x http-tunnel-client
./http-tunnel-client connect \\
  --server ${serverUrl} \\
  --subdomain [SUBDOMAIN] \\
  --target http://[IP]:[PORT]`;
}

function currentOrigin() {
  if (typeof window === "undefined") return "https://[SERVER-DOMAIN]";
  return window.location.origin;
}

function CommandBlock({ title, command }: { title: string; command: string }) {
  return (
    <section className="grid gap-2">
      <h3 className="text-sm font-medium">{title}</h3>
      <pre className="overflow-x-auto rounded-md border border-border bg-background p-3 text-xs leading-6 text-foreground">
        <code>{command}</code>
      </pre>
    </section>
  );
}

function TunnelSourceMap({ points, total }: { points: PublicTunnelMapPoint[]; total: number }) {
  return (
    <div className="relative h-[320px] overflow-hidden border-t border-border bg-sky-50 md:h-[380px] dark:bg-[#07111f]">
      <svg className="h-full w-full" viewBox="0 0 1000 440" role="img" aria-label="Tunnel source map">
        <defs>
          <radialGradient id="mapGlow" cx="50%" cy="42%" r="70%">
            <stop offset="0%" stopColor="#0A94F2" stopOpacity="0.18" />
            <stop offset="65%" stopColor="#0A94F2" stopOpacity="0.04" />
            <stop offset="100%" stopColor="#0A94F2" stopOpacity="0" />
          </radialGradient>
        </defs>
        <rect width="1000" height="440" fill="url(#mapGlow)" />
        <MapGrid />
        <WorldSilhouette />
        {points.map((point, index) => {
          const { x, y } = project(point.longitude, point.latitude);
          const offset = pointOffset(index);
          const tone = point.status === "connected" || point.active_sessions > 0 ? "online" : "offline";
          return (
            <g key={`${point.subdomain}-${index}`} transform={`translate(${x + offset.x} ${y + offset.y})`}>
              <title>{`${point.subdomain} · ${point.label}`}</title>
              <circle className={cn("animate-ping", tone === "online" ? "fill-emerald-400/30" : "fill-amber-300/25")} r="12" />
              <circle className={tone === "online" ? "fill-emerald-400" : "fill-amber-300"} r="4.5" />
              <circle className="fill-white" r="1.5" />
            </g>
          );
        })}
      </svg>
      {points.length === 0 ? (
        <div className="absolute inset-0 flex items-center justify-center px-4">
          <div className="rounded-md border border-border bg-background/80 px-4 py-3 text-center text-sm text-muted-foreground shadow-sm backdrop-blur">
            {total > 0 ? "No located tunnel sources" : "No tunnels"}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function MapGrid() {
  const vertical = [-120, -60, 0, 60, 120].map((lon) => project(lon, 0).x);
  const horizontal = [-45, 0, 45].map((lat) => project(0, lat).y);
  return (
    <g>
      {vertical.map((x) => (
        <line key={`v-${x}`} x1={x} x2={x} y1="34" y2="406" className="stroke-sky-200 dark:stroke-white/10" strokeWidth="1" />
      ))}
      {horizontal.map((y) => (
        <line key={`h-${y}`} x1="60" x2="940" y1={y} y2={y} className="stroke-sky-200 dark:stroke-white/10" strokeWidth="1" />
      ))}
    </g>
  );
}

function WorldSilhouette() {
  return (
    <g className="fill-sky-100 stroke-sky-300 dark:fill-slate-700/55 dark:stroke-slate-500/30" strokeWidth="1">
      <path d="M105 145 170 82 256 94 318 139 286 201 221 219 166 197 123 213 78 184Z" />
      <path d="M263 236 318 253 340 314 311 388 263 357 237 286Z" />
      <path d="M438 121 510 95 582 119 557 166 486 175 432 158Z" />
      <path d="M508 182 576 177 620 237 594 335 540 361 498 294 465 232Z" />
      <path d="M610 111 748 85 890 145 834 212 704 201 619 168Z" />
      <path d="M738 226 816 246 850 321 797 347 724 310Z" />
      <path d="M833 331 900 345 925 392 852 388Z" />
    </g>
  );
}

function TunnelTable({ tunnels }: { tunnels: PublicTunnel[] }) {
  if (tunnels.length === 0) {
    return <div className="p-4"><EmptyState label="No tunnels matched" /></div>;
  }
  return (
    <div className="overflow-x-auto">
      <Table className="min-w-[980px]">
        <TableHeader>
          <TableRow>
            <TableHead className="w-[260px]">Tunnel</TableHead>
            <TableHead>Source</TableHead>
            <TableHead className="w-24 text-right">Sessions</TableHead>
            <TableHead className="w-24 text-right">Streams</TableHead>
            <TableHead className="w-28 text-right">Requests</TableHead>
            <TableHead className="w-32 text-right">Traffic</TableHead>
            <TableHead className="w-36">Last seen</TableHead>
            <TableHead className="w-36">Expires</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {tunnels.map((tunnel) => (
            <TableRow key={tunnel.subdomain}>
              <TableCell>
                <div className="flex items-start gap-3">
                  <span className={cn("mt-1.5 h-2.5 w-2.5 rounded-full", tunnel.connected ? "bg-emerald-400" : "bg-slate-500")} />
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-medium">{tunnel.subdomain}</span>
                      <Badge variant={tunnel.connected ? "healthy" : "muted"}>{tunnel.status}</Badge>
                    </div>
                    <a
                      href={tunnel.url}
                      target="_blank"
                      rel="noreferrer"
                      className="mt-1 inline-flex max-w-[220px] items-center gap-1 truncate text-xs text-muted-foreground no-underline hover:text-primary"
                    >
                      {tunnel.url}
                      <ExternalLink className="h-3 w-3 shrink-0" />
                    </a>
                  </div>
                </div>
              </TableCell>
              <TableCell>
                <div className="flex items-center gap-2">
                  <Globe2 className={cn("h-4 w-4", tunnel.source.located ? "text-primary" : "text-muted-foreground")} />
                  <span className="max-w-[220px] truncate">{tunnel.source.label}</span>
                </div>
              </TableCell>
              <TableCell className="text-right">{formatNumber(tunnel.active_sessions)}</TableCell>
              <TableCell className="text-right">{formatNumber(tunnel.active_streams)}</TableCell>
              <TableCell className="text-right">
                <div>{formatNumber(tunnel.request_count)}</div>
                {tunnel.error_count ? <div className="text-xs text-red-700 dark:text-red-300">{formatNumber(tunnel.error_count)} errors</div> : null}
              </TableCell>
              <TableCell className="text-right">
                <div className="inline-flex items-center justify-end gap-1 text-xs text-muted-foreground">
                  <Activity className="h-3.5 w-3.5" />
                  {formatBytes(tunnel.bytes_in)}
                </div>
                <div className="inline-flex items-center justify-end gap-1 text-xs text-muted-foreground">
                  <Waves className="h-3.5 w-3.5" />
                  {formatBytes(tunnel.bytes_out)}
                </div>
              </TableCell>
              <TableCell>{formatDate(tunnel.last_seen_at)}</TableCell>
              <TableCell>{formatDate(tunnel.expires_at)}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function project(longitude: number, latitude: number) {
  return {
    x: ((longitude + 180) / 360) * 880 + 60,
    y: ((90 - latitude) / 180) * 360 + 40,
  };
}

function pointOffset(index: number) {
  const angle = (index % 12) * 0.92;
  const radius = Math.floor(index / 12) * 3;
  return {
    x: Math.cos(angle) * radius,
    y: Math.sin(angle) * radius,
  };
}

function formatDate(value?: string | null) {
  if (!value) return "never";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatClock(value: Date) {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(value);
}
