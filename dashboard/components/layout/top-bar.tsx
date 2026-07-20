import Link from "next/link";
import { Activity, Github, LockKeyhole, Network } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ThemeToggle } from "@/components/theme-toggle";

type TopBarProps = {
  title?: string;
  subtitle?: string;
  status?: string;
  statusTone?: "healthy" | "warning" | "danger" | "muted";
  adminLink?: boolean;
};

export function TopBar({
  title = "http-tunnel",
  subtitle = "HTTP/WebSocket tunnel operations",
  status,
  statusTone = "muted",
  adminLink = true,
}: TopBarProps) {
  return (
    <header className="sticky top-0 z-20 border-b border-border bg-background/95 backdrop-blur">
      <div className="mx-auto flex min-h-14 max-w-7xl items-center justify-between gap-2 px-4 sm:gap-4">
        <div className="flex min-w-0 items-center gap-3">
          <div className="flex h-8 w-8 items-center justify-center rounded-md border border-primary/40 bg-primary/10 text-primary">
            <Network className="h-4 w-4" />
          </div>
          <div className="min-w-0">
            <h1 className="truncate text-base font-semibold">{title}</h1>
            <p className="hidden truncate text-xs text-muted-foreground sm:block">{subtitle}</p>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {status ? (
            <Badge variant={statusTone}>
              <Activity className="mr-1 h-3 w-3" />
              {status}
            </Badge>
          ) : null}
          <Button asChild variant="outline" size="icon">
            <a
              href="https://github.com/Xiechengqi/http-tunnel"
              target="_blank"
              rel="noreferrer"
              aria-label="GitHub"
              title="GitHub"
            >
              <Github className="h-4 w-4" />
            </a>
          </Button>
          <ThemeToggle />
          {adminLink ? (
            <Button asChild variant="outline" size="sm" className="px-2 sm:px-3">
              <Link href="/admin">
                <LockKeyhole className="h-4 w-4" />
                <span className="hidden sm:inline">Admin</span>
              </Link>
            </Button>
          ) : null}
        </div>
      </div>
    </header>
  );
}
