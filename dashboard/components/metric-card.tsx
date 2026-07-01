import type { ReactNode } from "react";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";

type MetricCardProps = {
  label: string;
  value: ReactNode;
  detail?: ReactNode;
  tone?: "blue" | "green" | "red" | "amber" | "muted";
};

const tones = {
  blue: "border-l-primary",
  green: "border-l-emerald-500",
  red: "border-l-red-500",
  amber: "border-l-amber-500",
  muted: "border-l-border",
};

export function MetricCard({ label, value, detail, tone = "blue" }: MetricCardProps) {
  return (
    <Card className={cn("border-l-4", tones[tone])}>
      <CardContent className="p-4">
        <p className="text-xs text-muted-foreground">{label}</p>
        <div className="mt-1 text-2xl font-semibold">{value}</div>
        {detail ? <p className="mt-1 text-xs text-muted-foreground">{detail}</p> : null}
      </CardContent>
    </Card>
  );
}
