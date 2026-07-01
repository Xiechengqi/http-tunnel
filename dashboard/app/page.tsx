"use client";

import { useEffect, useMemo, useRef, useState } from "react";
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
import WorldMap, { regions, type CountryContext, type DataItem } from "react-svg-worldmap";
import { TopBar } from "@/components/layout/top-bar";
import { MetricCard } from "@/components/metric-card";
import { EmptyState, ErrorState, LoadingState } from "@/components/state-block";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { publicApi } from "@/lib/api";
import type { DashboardSummary, PublicTunnel, PublicTunnelCountrySource } from "@/lib/types";
import { cn, formatBytes, formatNumber } from "@/lib/utils";

type StatusFilter = "all" | "online" | "offline";

const COUNTRY_NAMES = new Map(regions.map((region) => [region.code.toUpperCase(), region.name]));

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

  const visibleCountrySources = useMemo(() => {
    if (!summary) return [];
    const filtered = filteredTunnels.length !== summary.tunnels.length;
    if (!filtered) return summary.country_sources || [];
    return countrySourcesFromTunnels(filteredTunnels);
  }, [filteredTunnels, summary]);
  const visibleClientCount = useMemo(
    () => filteredTunnels.reduce((total, tunnel) => total + tunnel.active_sessions, 0),
    [filteredTunnels],
  );
  const visibleUnknownClientCount = useMemo(() => {
    const known = visibleCountrySources.reduce((total, country) => total + country.client_count, 0);
    return Math.max(0, visibleClientCount - known);
  }, [visibleClientCount, visibleCountrySources]);

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
                <Badge variant="muted">{formatNumber(visibleCountrySources.length)} countries</Badge>
              </CardHeader>
              <CardContent className="p-0">
                <TunnelSourceMap
                  countries={visibleCountrySources}
                  totalClients={visibleClientCount}
                  unknownClients={visibleUnknownClientCount}
                />
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

function TunnelSourceMap({
  countries,
  totalClients,
  unknownClients,
}: {
  countries: PublicTunnelCountrySource[];
  totalClients: number;
  unknownClients: number;
}) {
  const [mapFrameRef, mapFrameWidth] = useElementWidth<HTMLDivElement>();
  const data = countries
    .filter((country) => country.country_code)
    .map((country) => ({
      country: country.country_code.toLowerCase(),
      value: country.client_count,
    })) as DataItem<number>[];
  const countryMeta = new Map(countries.map((country) => [country.country_code.toUpperCase(), country]));
  const maxClients = countries.reduce((max, country) => Math.max(max, country.client_count), 0);
  const mapSize = Math.max(640, Math.ceil(mapFrameWidth || 960));

  return (
    <div className="relative h-[clamp(320px,48vw,620px)] overflow-hidden border-t border-border bg-sky-50 dark:bg-[#07111f]">
      <div ref={mapFrameRef} className="absolute inset-x-0 bottom-9 top-0 flex -translate-y-3 items-center justify-center overflow-hidden px-2 md:bottom-10">
        <WorldMap
          data={data}
          color="#0A94F2"
          size={mapSize}
          frame={false}
          backgroundColor="transparent"
          borderColor="rgba(14, 116, 144, 0.22)"
          tooltipBgColor="hsl(var(--popover))"
          tooltipTextColor="hsl(var(--popover-foreground))"
          richInteraction
          containerClassName="worldmap__wrapper flex w-full justify-center [&_.worldmap__figure-container]:shrink-0"
          tooltipTextFunction={(context) => countryTooltip(context, countryMeta)}
          styleFunction={(context) => countryStyle(context, maxClients)}
        />
      </div>
      <div className="pointer-events-none absolute bottom-3 left-4 right-4 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
        <div className="flex items-center gap-2">
          <span className="h-2.5 w-10 rounded-sm bg-gradient-to-r from-sky-100 to-sky-500 ring-1 ring-border" />
          <span>{formatNumber(totalClients)} active clients</span>
        </div>
        {unknownClients > 0 ? <span>{formatNumber(unknownClients)} unknown</span> : null}
      </div>
      {countries.length === 0 ? (
        <div className="absolute inset-0 flex items-center justify-center px-4">
          <div className="rounded-md border border-border bg-background/80 px-4 py-3 text-center text-sm text-muted-foreground shadow-sm backdrop-blur">
            {totalClients > 0 ? "No country data for active clients" : "No active clients"}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function useElementWidth<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [width, setWidth] = useState(0);

  useEffect(() => {
    const node = ref.current;
    if (!node) return;

    let frame = 0;
    const update = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        setWidth(Math.round(node.getBoundingClientRect().width));
      });
    };

    update();

    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", update);
      return () => {
        window.cancelAnimationFrame(frame);
        window.removeEventListener("resize", update);
      };
    }

    const observer = new ResizeObserver(update);
    observer.observe(node);

    return () => {
      window.cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, []);

  return [ref, width] as const;
}

function countryTooltip(context: CountryContext<number>, countryMeta: Map<string, PublicTunnelCountrySource>) {
  const code = context.countryCode.toUpperCase();
  const source = countryMeta.get(code);
  const name = source?.country || COUNTRY_NAMES.get(code) || context.countryName || code;
  return `${name}
Clients: ${formatNumber(source?.client_count || 0)}
Tunnels: ${formatNumber(source?.tunnel_count || 0)}`;
}

function countryStyle(context: CountryContext<number>, maxClients: number) {
  const value = Number(context.countryValue || 0);
  if (!value || !maxClients) {
    return {
      fill: "rgba(186, 230, 253, 0.34)",
      stroke: "rgba(14, 116, 144, 0.22)",
      strokeWidth: 0.6,
      cursor: "default",
    };
  }
  const intensity = Math.max(0.22, Math.min(1, value / maxClients));
  return {
    fill: `rgba(10, 148, 242, ${0.28 + intensity * 0.62})`,
    stroke: "rgba(7, 89, 133, 0.42)",
    strokeWidth: 0.8,
    cursor: "pointer",
  };
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
                  <span className="max-w-[220px] truncate">{sourceLabel(tunnel)}</span>
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

function countrySourcesFromTunnels(tunnels: PublicTunnel[]): PublicTunnelCountrySource[] {
  const byCountry = new Map<string, PublicTunnelCountrySource>();
  for (const tunnel of tunnels) {
    const code = tunnel.source.country_code?.toUpperCase();
    if (!code || !tunnel.active_sessions) continue;
    const current = byCountry.get(code) || {
      country_code: code,
      country: tunnel.source.country || COUNTRY_NAMES.get(code) || code,
      client_count: 0,
      tunnel_count: 0,
    };
    current.client_count += tunnel.active_sessions;
    current.tunnel_count += 1;
    byCountry.set(code, current);
  }
  return Array.from(byCountry.values()).sort((a, b) => b.client_count - a.client_count || a.country_code.localeCompare(b.country_code));
}

function sourceLabel(tunnel: PublicTunnel) {
  const code = tunnel.source.country_code?.toUpperCase();
  if (!code) return tunnel.source.label;
  return tunnel.source.country || COUNTRY_NAMES.get(code) || code;
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
