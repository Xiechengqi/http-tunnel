import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-medium",
  {
    variants: {
      variant: {
        default: "border-primary/40 bg-primary/10 text-primary",
        healthy: "border-emerald-500/40 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
        warning: "border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300",
        danger: "border-red-500/40 bg-red-500/10 text-red-700 dark:text-red-300",
        muted: "border-border bg-secondary text-muted-foreground",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  },
);

export interface BadgeProps extends React.HTMLAttributes<HTMLDivElement>, VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, ...props }: BadgeProps) {
  return <div className={cn(badgeVariants({ variant }), className)} {...props} />;
}
