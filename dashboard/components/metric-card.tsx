import type { ReactNode } from "react";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";

type MetricCardProps = {
  label: string;
  value: ReactNode;
  detail?: ReactNode;
  icon?: ReactNode;
  progress?: number;
  tone?: "blue" | "green" | "red" | "amber" | "muted";
};

const tones = {
  blue: {
    border: "border-l-primary",
    icon: "bg-primary/10 text-primary",
    progress: "bg-primary",
  },
  green: {
    border: "border-l-emerald-500",
    icon: "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
    progress: "bg-emerald-500",
  },
  red: {
    border: "border-l-red-500",
    icon: "bg-red-500/10 text-red-700 dark:text-red-300",
    progress: "bg-red-500",
  },
  amber: {
    border: "border-l-amber-500",
    icon: "bg-amber-500/10 text-amber-700 dark:text-amber-300",
    progress: "bg-amber-500",
  },
  muted: {
    border: "border-l-border",
    icon: "bg-secondary text-muted-foreground",
    progress: "bg-muted-foreground",
  },
};

export function MetricCard({ label, value, detail, icon, progress, tone = "blue" }: MetricCardProps) {
  const styles = tones[tone];
  const normalizedProgress = progress === undefined ? undefined : Math.max(0, Math.min(100, progress));

  return (
    <Card className={cn("min-h-28 min-w-0 border-l-4", styles.border)}>
      <CardContent className="flex h-full flex-col justify-between gap-3 p-4">
        <div className="flex items-start justify-between gap-2">
          <p className="min-w-0 text-xs font-medium leading-4 text-muted-foreground">{label}</p>
          {icon ? (
            <span className={cn("flex h-8 w-8 shrink-0 items-center justify-center rounded-md", styles.icon)}>
              {icon}
            </span>
          ) : null}
        </div>
        <div className="whitespace-nowrap text-2xl font-semibold tabular-nums">{value}</div>
        <div>
          {normalizedProgress !== undefined ? (
            <div
              className="mb-2 h-1.5 overflow-hidden rounded-full bg-secondary"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(normalizedProgress)}
            >
              <div className={cn("h-full rounded-full", styles.progress)} style={{ width: `${normalizedProgress}%` }} />
            </div>
          ) : null}
          {detail ? <p className="text-xs leading-4 text-muted-foreground">{detail}</p> : null}
        </div>
      </CardContent>
    </Card>
  );
}
