import { AlertTriangle, Loader2 } from "lucide-react";
import { Card, CardContent } from "@/components/ui/card";

export function LoadingState({ label = "Loading" }: { label?: string }) {
  return (
    <Card>
      <CardContent className="flex items-center gap-2 p-4 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        {label}
      </CardContent>
    </Card>
  );
}

export function ErrorState({ message }: { message: string }) {
  return (
    <Card className="border-red-500/40 bg-red-500/5">
      <CardContent className="flex items-center gap-2 p-4 text-sm text-red-700 dark:text-red-200">
        <AlertTriangle className="h-4 w-4" />
        {message}
      </CardContent>
    </Card>
  );
}

export function EmptyState({ label }: { label: string }) {
  return (
    <div className="rounded-md border border-dashed border-border p-6 text-center text-sm text-muted-foreground">
      {label}
    </div>
  );
}
