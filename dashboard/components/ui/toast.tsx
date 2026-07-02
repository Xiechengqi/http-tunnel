"use client";

import * as React from "react";
import { X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export type ToastTone = "default" | "success" | "warning" | "danger";

export type ToastItem = {
  id: string;
  title: string;
  description?: string;
  tone?: ToastTone;
  actionLabel?: string;
  cancelLabel?: string;
  onAction?: () => void;
};

type ToastViewportProps = {
  toasts: ToastItem[];
  onDismiss: (id: string) => void;
  onAction: (toast: ToastItem) => void;
};

export function ToastViewport({ toasts, onDismiss, onAction }: ToastViewportProps) {
  if (!toasts.length) return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 grid w-[min(calc(100vw-2rem),24rem)] gap-2">
      {toasts.map((toast) => (
        <section
          key={toast.id}
          role="status"
          aria-live="polite"
          className={cn(
            "rounded-md border bg-card p-3 text-card-foreground shadow-lg",
            toast.tone === "success" && "border-emerald-500/40 bg-emerald-500/10",
            toast.tone === "warning" && "border-amber-500/50 bg-amber-500/10",
            toast.tone === "danger" && "border-destructive/50 bg-destructive/10",
          )}
        >
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <h2 className="text-sm font-medium">{toast.title}</h2>
              {toast.description ? (
                <p className="mt-1 whitespace-pre-wrap break-words text-xs leading-5 text-muted-foreground">
                  {toast.description}
                </p>
              ) : null}
            </div>
            <button
              type="button"
              className="rounded-md p-1 text-muted-foreground transition hover:bg-secondary hover:text-foreground"
              onClick={() => onDismiss(toast.id)}
              aria-label="Dismiss"
              title="Dismiss"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
          {toast.onAction ? (
            <div className="mt-3 flex justify-end gap-2">
              <Button type="button" variant="ghost" size="sm" onClick={() => onDismiss(toast.id)}>
                {toast.cancelLabel || "Cancel"}
              </Button>
              <Button
                type="button"
                variant={toast.tone === "danger" ? "destructive" : "default"}
                size="sm"
                onClick={() => onAction(toast)}
              >
                {toast.actionLabel || "Confirm"}
              </Button>
            </div>
          ) : null}
        </section>
      ))}
    </div>
  );
}
