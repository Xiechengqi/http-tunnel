"use client";

import { useEffect, useMemo, useRef, useState, type PointerEvent, type ReactNode } from "react";
import {
  ArrowDown,
  ArrowUp,
  BookOpen,
  ExternalLink,
  Globe2,
  MapPinned,
  Network,
  Search,
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
import type { DashboardPresence, DashboardSummary, PublicTunnel, PublicTunnelCountrySource } from "@/lib/types";
import { cn, formatBytes, formatBytesPerSecond, formatNumber } from "@/lib/utils";

type StatusFilter = "all" | "online" | "offline";
type TrafficRate = {
  inBytesPerSecond: number;
  outBytesPerSecond: number;
};
type TrafficSnapshot = {
  bytesIn: number;
  bytesOut: number;
  capturedAt: number;
};

const COUNTRY_NAMES = new Map(regions.map((region) => [region.code.toUpperCase(), region.name]));
const DEFAULT_MAP_OFFSET_RATIO = 0.58;
const MAP_OFFSET_STORAGE_KEY = "http-tunnel.dashboard.mapOffsetRatio.v1";
const DISCONNECTED_EXPIRE_MS = 10 * 60 * 1000;
const EXPIRED_DELETE_MS = 60 * 60 * 1000;
const EMPTY_TRAFFIC_RATE: TrafficRate = { inBytesPerSecond: 0, outBytesPerSecond: 0 };

export default function PublicDashboardPage() {
  const [summary, setSummary] = useState<DashboardSummary | null>(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [lastLoadedAt, setLastLoadedAt] = useState<Date | null>(null);
  const [docsOpen, setDocsOpen] = useState(false);
  const [trafficRates, setTrafficRates] = useState<Record<string, TrafficRate>>({});
  const trafficSnapshotsRef = useRef<Record<string, TrafficSnapshot>>({});

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const data = await publicApi<DashboardSummary>("/api/v1/dashboard");
        if (!cancelled) {
          const loadedAt = Date.now();
          const traffic = calculateTrafficRates(data.tunnels, trafficSnapshotsRef.current, loadedAt);
          trafficSnapshotsRef.current = traffic.snapshots;
          setTrafficRates(traffic.rates);
          setSummary(data);
          setLastLoadedAt(new Date(loadedAt));
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
                <TunnelTable tunnels={filteredTunnels} trafficRates={trafficRates} />
              </CardContent>
            </Card>
          </>
        ) : null}
      </main>
      <PresenceFooter />
      {docsOpen ? <ClientDocsModal serverUrl={summary?.server_url} githubProxy={summary?.github_proxy} onClose={() => setDocsOpen(false)} /> : null}
    </div>
  );
}

function calculateTrafficRates(
  tunnels: PublicTunnel[],
  previous: Record<string, TrafficSnapshot>,
  capturedAt: number,
) {
  const rates: Record<string, TrafficRate> = {};
  const snapshots: Record<string, TrafficSnapshot> = {};

  for (const tunnel of tunnels) {
    const key = tunnelTrafficKey(tunnel);
    const bytesIn = safeTrafficBytes(tunnel.bytes_in);
    const bytesOut = safeTrafficBytes(tunnel.bytes_out);
    const last = previous[key];
    snapshots[key] = { bytesIn, bytesOut, capturedAt };

    if (!last) {
      rates[key] = { ...EMPTY_TRAFFIC_RATE };
      continue;
    }

    const elapsedSeconds = Math.max((capturedAt - last.capturedAt) / 1000, 1);
    rates[key] = {
      inBytesPerSecond: Math.max(0, bytesIn - last.bytesIn) / elapsedSeconds,
      outBytesPerSecond: Math.max(0, bytesOut - last.bytesOut) / elapsedSeconds,
    };
  }

  return { rates, snapshots };
}

function tunnelTrafficKey(tunnel: PublicTunnel) {
  return tunnel.subdomain;
}

function safeTrafficBytes(value: number | null | undefined) {
  return Number.isFinite(value) ? Math.max(0, value || 0) : 0;
}

function PresenceFooter() {
  const [presence, setPresence] = useState<DashboardPresence | null>(null);

  useEffect(() => {
    let cancelled = false;
    const sessionId = dashboardPresenceSessionId();

    async function tick() {
      try {
        const data = await publicApi<DashboardPresence>("/api/v1/dashboard/presence", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ session_id: sessionId }),
        });
        if (!cancelled) setPresence(data);
      } catch {
        // Presence is informational; dashboard data should remain usable if it fails.
      }
    }

    tick();
    const timer = window.setInterval(tick, 15000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  return (
    <footer className="mx-auto flex w-[calc(100%-2rem)] max-w-7xl flex-wrap items-center justify-center gap-2 py-6 font-mono text-[11px] uppercase tracking-[0.1em] text-muted-foreground">
      <span>
        Page Online <strong className="ml-1 text-foreground">{presence?.online_count ?? 0}</strong>
      </span>
      <span className="opacity-50">|</span>
      <a
        href="https://github.com/Xiechengqi/http-tunnel"
        target="_blank"
        rel="noreferrer"
        className="hover:text-primary"
      >
        GitHub
      </a>
    </footer>
  );
}

function dashboardPresenceSessionId() {
  const randomUUID = globalThis.crypto?.randomUUID?.bind(globalThis.crypto);
  return randomUUID ? randomUUID() : `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function ClientDocsModal({
  serverUrl,
  githubProxy,
  onClose,
}: {
  serverUrl?: string | null;
  githubProxy?: string | null;
  onClose: () => void;
}) {
  const resolvedServerUrl = serverUrl || currentOrigin();
  const linuxAmd64Url = proxiedGithubUrl(
    "https://github.com/Xiechengqi/http-tunnel/releases/download/latest/http-tunnel-client-linux-amd64",
    githubProxy,
  );
  const linuxArm64Url = proxiedGithubUrl(
    "https://github.com/Xiechengqi/http-tunnel/releases/download/latest/http-tunnel-client-linux-arm64",
    githubProxy,
  );
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
            command={clientCommand(linuxAmd64Url, resolvedServerUrl)}
          />
          <CommandBlock
            title="Linux arm64"
            command={clientCommand(linuxArm64Url, resolvedServerUrl)}
          />
        </div>
      </div>
    </div>
  );
}

function proxiedGithubUrl(url: string, githubProxy?: string | null) {
  const proxy = githubProxy?.trim().replace(/\/+$/, "");
  if (!proxy) return url;
  const prefix = `${proxy}/`;
  return url.startsWith(prefix) ? url : `${prefix}${url}`;
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
  const mapRootRef = useRef<HTMLDivElement | null>(null);
  const [mapFrameRef, mapFrameSize] = useElementSize<HTMLDivElement>();
  const dragRef = useRef<MapDragState | null>(null);
  const mapOffsetRatioRef = useRef(DEFAULT_MAP_OFFSET_RATIO);
  const [tooltip, setTooltip] = useState<MapTooltipState | null>(null);
  const [mapOffsetRatio, setMapOffsetRatioValue] = useState(DEFAULT_MAP_OFFSET_RATIO);
  const countryMeta = useMemo(
    () => new Map(countries.map((country) => [country.country_code.toUpperCase(), country])),
    [countries],
  );
  const data = useMemo(
    () =>
      regions.map((region) => ({
        country: region.code.toLowerCase(),
        value: countryMeta.get(region.code.toUpperCase())?.client_count || 0,
      })) as DataItem<number>[],
    [countryMeta],
  );
  const maxClients = countries.reduce((max, country) => Math.max(max, country.client_count), 0);
  const mapFrameWidth = mapFrameSize.width;
  const mapSize = Math.max(640, Math.ceil(mapFrameWidth || 960));
  const mapVisualHeight = mapSize * 0.75;
  const maxMapOffsetY = Math.max(0, (mapVisualHeight - mapFrameSize.height) / 2);
  const mapOffsetY = maxMapOffsetY > 0 ? mapOffsetRatio * maxMapOffsetY : 0;

  useEffect(() => {
    const storedRatio = readStoredMapOffsetRatio();
    if (storedRatio === null) return;
    mapOffsetRatioRef.current = storedRatio;
    setMapOffsetRatioValue(storedRatio);
  }, []);

  function setMapOffsetRatio(nextRatio: number) {
    const clampedRatio = clamp(nextRatio, -1, 1);
    mapOffsetRatioRef.current = clampedRatio;
    setMapOffsetRatioValue(clampedRatio);
  }

  function showTooltip(event: PointerEvent<HTMLDivElement>) {
    const root = mapRootRef.current;
    const target = event.target instanceof Element ? event.target.closest("path") : null;
    if (!root || !target || !root.contains(target)) {
      setTooltip(null);
      return;
    }

    const svg = target.ownerSVGElement;
    if (!svg) {
      setTooltip(null);
      return;
    }

    const regionIndex = Array.from(svg.querySelectorAll("path")).indexOf(target as SVGPathElement);
    const region = regions[regionIndex];
    if (!region) {
      setTooltip(null);
      return;
    }

    const code = region.code.toUpperCase();
    const source = countryMeta.get(code);
    const bounds = root.getBoundingClientRect();
    setTooltip({
      code,
      name: source?.country || COUNTRY_NAMES.get(code) || region.name || code,
      clientCount: source?.client_count || 0,
      tunnelCount: source?.tunnel_count || 0,
      x: event.clientX - bounds.left,
      y: event.clientY - bounds.top,
    });
  }

  function startMapDrag(event: PointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    dragRef.current = {
      pointerId: event.pointerId,
      startY: event.clientY,
      startOffsetY: mapOffsetY,
    };
    setTooltip(null);
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function moveMapPointer(event: PointerEvent<HTMLDivElement>) {
    const drag = dragRef.current;
    if (drag?.pointerId === event.pointerId) {
      const nextOffset = clamp(drag.startOffsetY + event.clientY - drag.startY, -maxMapOffsetY, maxMapOffsetY);
      setMapOffsetRatio(maxMapOffsetY > 0 ? nextOffset / maxMapOffsetY : 0);
      setTooltip(null);
      event.preventDefault();
      return;
    }
    showTooltip(event);
  }

  function stopMapDrag(event: PointerEvent<HTMLDivElement>) {
    if (dragRef.current?.pointerId !== event.pointerId) return;
    dragRef.current = null;
    writeStoredMapOffsetRatio(mapOffsetRatioRef.current);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  }

  return (
    <div
      ref={mapRootRef}
      className="relative h-[clamp(160px,24vw,310px)] touch-none overflow-hidden border-t border-border bg-sky-50 select-none dark:bg-[#07111f]"
      onPointerDown={startMapDrag}
      onPointerMove={moveMapPointer}
      onPointerUp={stopMapDrag}
      onPointerCancel={stopMapDrag}
      onLostPointerCapture={stopMapDrag}
      onPointerLeave={(event) => {
        if (dragRef.current?.pointerId !== event.pointerId) setTooltip(null);
      }}
    >
      <div ref={mapFrameRef} className="absolute inset-x-0 bottom-9 top-0 flex items-center justify-center overflow-hidden px-2 md:bottom-10">
        <div className="cursor-grab active:cursor-grabbing" style={{ transform: `translateY(${mapOffsetY}px)` }}>
          <WorldMap
            data={data}
            color="#0A94F2"
            size={mapSize}
            frame={false}
            backgroundColor="transparent"
            borderColor="rgba(14, 116, 144, 0.22)"
            containerClassName="worldmap__wrapper flex w-full justify-center [&_.worldmap__figure-container]:shrink-0"
            tooltipTextFunction={() => ""}
            styleFunction={(context) => countryStyle(context, maxClients)}
          />
        </div>
      </div>
      {tooltip ? (
        <div
          className="pointer-events-none absolute z-10 w-44 rounded-md border border-border bg-popover px-3 py-2 text-xs text-popover-foreground shadow-lg"
          style={{
            left: tooltip.x,
            top: tooltip.y,
            transform: tooltip.x > (mapFrameWidth || mapSize) - 220 ? "translate(-100%, 12px)" : "translate(12px, 12px)",
          }}
        >
          <div className="flex items-center justify-between gap-3">
            <span className="truncate font-medium">{tooltip.name}</span>
            <span className="shrink-0 text-muted-foreground">{tooltip.code}</span>
          </div>
          <div className="mt-1 grid gap-0.5 text-muted-foreground">
            <span>Clients: {formatNumber(tooltip.clientCount)}</span>
            <span>Tunnels: {formatNumber(tooltip.tunnelCount)}</span>
          </div>
        </div>
      ) : null}
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

type MapTooltipState = {
  code: string;
  name: string;
  clientCount: number;
  tunnelCount: number;
  x: number;
  y: number;
};

type MapDragState = {
  pointerId: number;
  startY: number;
  startOffsetY: number;
};

function useElementSize<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    const node = ref.current;
    if (!node) return;

    let frame = 0;
    const update = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        const rect = node.getBoundingClientRect();
        setSize({
          width: Math.round(rect.width),
          height: Math.round(rect.height),
        });
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

  return [ref, size] as const;
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function readStoredMapOffsetRatio() {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(MAP_OFFSET_STORAGE_KEY);
    if (!raw) return null;
    const value = Number(raw);
    return Number.isFinite(value) ? clamp(value, -1, 1) : null;
  } catch {
    return null;
  }
}

function writeStoredMapOffsetRatio(value: number) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(MAP_OFFSET_STORAGE_KEY, String(clamp(value, -1, 1)));
  } catch {
    // Private browsing or locked-down storage should not break the dashboard.
  }
}

function countryStyle(context: CountryContext<number>, maxClients: number) {
  const value = Number(context.countryValue || 0);
  if (!value || !maxClients) {
    return {
      fill: "rgba(186, 230, 253, 0.34)",
      stroke: "rgba(14, 116, 144, 0.22)",
      strokeWidth: 0.6,
      cursor: "grab",
    };
  }
  const intensity = Math.max(0.22, Math.min(1, value / maxClients));
  return {
    fill: `rgba(10, 148, 242, ${0.28 + intensity * 0.62})`,
    stroke: "rgba(7, 89, 133, 0.42)",
    strokeWidth: 0.8,
    cursor: "grab",
  };
}

function TunnelTable({
  tunnels,
  trafficRates,
}: {
  tunnels: PublicTunnel[];
  trafficRates: Record<string, TrafficRate>;
}) {
  if (tunnels.length === 0) {
    return <div className="p-4"><EmptyState label="No tunnels matched" /></div>;
  }
  return (
    <div className="overflow-x-auto">
      <Table className="min-w-[1160px]">
        <TableHeader>
          <TableRow>
            <TableHead className="w-[260px]">Tunnel</TableHead>
            <TableHead>Source</TableHead>
            <TableHead className="w-24 text-right">Sessions</TableHead>
            <TableHead className="w-24 text-right">Streams</TableHead>
            <TableHead className="w-28 text-right">Requests</TableHead>
            <TableHead className="w-40 text-right">In</TableHead>
            <TableHead className="w-40 text-right">Out</TableHead>
            <TableHead className="w-36">Last seen</TableHead>
            <TableHead className="w-40">Lifecycle</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {tunnels.map((tunnel) => {
            const rate = trafficRates[tunnelTrafficKey(tunnel)] || EMPTY_TRAFFIC_RATE;
            return (
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
                  <TrafficCell
                    icon={<ArrowDown className="h-3.5 w-3.5" />}
                    rate={rate.inBytesPerSecond}
                    total={tunnel.bytes_in}
                  />
                </TableCell>
                <TableCell className="text-right">
                  <TrafficCell
                    icon={<ArrowUp className="h-3.5 w-3.5" />}
                    rate={rate.outBytesPerSecond}
                    total={tunnel.bytes_out}
                  />
                </TableCell>
                <TableCell>{formatDate(tunnel.last_seen_at)}</TableCell>
                <TableCell><TunnelLifecycle tunnel={tunnel} /></TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}

function TrafficCell({
  icon,
  rate,
  total,
}: {
  icon: ReactNode;
  rate: number;
  total: number;
}) {
  return (
    <div className="inline-flex items-center justify-end gap-1 whitespace-nowrap text-xs text-muted-foreground">
      {icon}
      <span>{formatBytesPerSecond(rate)} / {formatBytes(total)}</span>
    </div>
  );
}

function TunnelLifecycle({ tunnel }: { tunnel: PublicTunnel }) {
  const lifecycle = tunnelLifecycle(tunnel);
  return (
    <div className="grid gap-0.5">
      <span className={cn("text-sm", lifecycle.tone === "muted" && "text-muted-foreground", lifecycle.tone === "warning" && "text-amber-700 dark:text-amber-300")}>
        {lifecycle.label}
      </span>
      {lifecycle.detail ? <span className="text-xs text-muted-foreground">{lifecycle.detail}</span> : null}
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
  const date = parseDate(value);
  if (!date) return value;
  return new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function tunnelLifecycle(tunnel: PublicTunnel) {
  const status = tunnel.status.toLowerCase();
  if (tunnel.connected || status === "connected") {
    if (tunnel.client_ttl_seconds) {
      const expiresAt = parseDate(tunnel.expires_at);
      return {
        label: expiresAt ? `Deletes ${formatRelativeTime(expiresAt)}` : "Limited lifetime",
        detail: expiresAt ? formatDateTime(expiresAt) : `${formatDuration(tunnel.client_ttl_seconds)} exposure limit`,
        tone: "warning" as const,
      };
    }
    return {
      label: "While connected",
      detail: "No forced expiry",
      tone: "muted" as const,
    };
  }

  if (status === "disconnected") {
    const expiresAt = addMilliseconds(tunnel.disconnected_at, DISCONNECTED_EXPIRE_MS);
    return {
      label: expiresAt ? `Expires ${formatRelativeTime(expiresAt)}` : "Expires after disconnect",
      detail: expiresAt ? formatDateTime(expiresAt) : "About 10 minutes after disconnect",
      tone: "warning" as const,
    };
  }

  if (status === "expired") {
    const deletesAt = parseDate(tunnel.claim_expires_at) || addMilliseconds(tunnel.disconnected_at, EXPIRED_DELETE_MS) || addMilliseconds(tunnel.expires_at, EXPIRED_DELETE_MS);
    return {
      label: deletesAt ? `Deletes ${formatRelativeTime(deletesAt)}` : "Pending delete",
      detail: deletesAt ? formatDateTime(deletesAt) : "About 1 hour after expiry",
      tone: "warning" as const,
    };
  }

  return {
    label: formatDate(tunnel.expires_at),
    detail: status === "reserved" ? "Reserved tunnel" : undefined,
    tone: "muted" as const,
  };
}

function parseDate(value?: string | null) {
  if (!value) return null;
  const normalized = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(value)
    ? `${value.replace(" ", "T")}Z`
    : value;
  const date = new Date(normalized);
  return Number.isNaN(date.getTime()) ? null : date;
}

function addMilliseconds(value: string | null | undefined, milliseconds: number) {
  const date = parseDate(value);
  return date ? new Date(date.getTime() + milliseconds) : null;
}

function formatRelativeTime(date: Date) {
  const diffMs = date.getTime() - Date.now();
  const absMs = Math.abs(diffMs);
  const minutes = Math.max(1, Math.round(absMs / 60_000));
  const formatter = new Intl.RelativeTimeFormat("en-US", { numeric: "auto" });
  if (minutes < 60) {
    return formatter.format(diffMs >= 0 ? minutes : -minutes, "minute");
  }
  const hours = Math.max(1, Math.round(minutes / 60));
  return formatter.format(diffMs >= 0 ? hours : -hours, "hour");
}

function formatDateTime(date: Date) {
  return new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatDuration(seconds: number) {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.round(hours / 24);
  return `${days}d`;
}

function formatClock(value: Date) {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(value);
}
