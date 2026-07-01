"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import {
  Activity,
  Bell,
  Database,
  FileClock,
  Gauge,
  KeyRound,
  Network,
  Settings,
  Shield,
  TerminalSquare,
} from "lucide-react";
import { ThemeToggle } from "@/components/theme-toggle";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const navItems = [
  { value: "overview", label: "Overview", icon: Gauge },
  { value: "tunnels", label: "Tunnels", icon: Network },
  { value: "activity", label: "Activity", icon: Activity },
  { value: "security", label: "Security", icon: Shield },
  { value: "config", label: "Config", icon: Settings },
  { value: "maintenance", label: "Maintenance", icon: Database },
  { value: "version", label: "Version", icon: FileClock },
];

type AdminShellProps = {
  children: ReactNode;
  onLogout?: () => void;
  activeTab?: string;
  onTabChange?: (tab: string) => void;
};

export function AdminShell({ children, onLogout, activeTab = "overview", onTabChange }: AdminShellProps) {
  return (
    <div className="ops-shell">
      <header className="border-b border-border bg-background/95 backdrop-blur">
        <div className="mx-auto flex min-h-14 max-w-7xl items-center justify-between gap-4 px-4">
          <div className="flex items-center gap-3">
            <div className="flex h-8 w-8 items-center justify-center rounded-md border border-primary/40 bg-primary/10 text-primary">
              <TerminalSquare className="h-4 w-4" />
            </div>
            <div>
              <h1 className="text-base font-semibold">http-tunnel admin</h1>
              <p className="text-xs text-muted-foreground">Authenticated operations console</p>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <ThemeToggle />
            <Button asChild variant="ghost" size="sm">
              <Link href="/">
                <Bell className="h-4 w-4" />
                Public
              </Link>
            </Button>
            <Button id="logoutButton" variant="outline" size="sm" onClick={onLogout}>
              <KeyRound className="h-4 w-4" />
              Logout
            </Button>
          </div>
        </div>
      </header>
      <nav className="border-b border-border bg-card">
        <div className="mx-auto flex max-w-7xl gap-1 overflow-x-auto px-4 py-2">
          {navItems.map((item) => {
            const Icon = item.icon;
            const active = activeTab === item.value;
            return (
              <button
                key={item.label}
                type="button"
                aria-current={active ? "page" : undefined}
                onClick={() => onTabChange?.(item.value)}
                className={cn(
                  "inline-flex h-9 items-center gap-2 rounded-md px-3 text-sm text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground",
                  active && "bg-primary text-primary-foreground shadow-sm hover:bg-primary/90 hover:text-primary-foreground",
                )}
              >
                <Icon className="h-4 w-4" />
                {item.label}
              </button>
            );
          })}
        </div>
      </nav>
      <main className="mx-auto max-w-7xl px-4 py-5">{children}</main>
    </div>
  );
}
