"use client";

import { FormEvent, useState } from "react";
import { Eye, EyeOff, KeyRound, Loader2, Network } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { ApiError } from "@/lib/api";

export default function LoginPage() {
  const [password, setPassword] = useState("");
  const [visible, setVisible] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      const response = await fetch("/api/admin/login", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ password }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok || !body?.ok) {
        throw new ApiError(body?.error?.message || "Login failed", response.status, body?.error?.code);
      }
      window.location.href = "/admin";
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="ops-shell grid min-h-screen place-items-center px-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Network className="h-4 w-4 text-primary" />
            http-tunnel login
          </CardTitle>
        </CardHeader>
        <CardContent>
          <form className="grid gap-4" onSubmit={submit}>
            <label className="grid gap-2 text-sm">
              <span className="text-muted-foreground">Admin password</span>
              <span className="grid grid-cols-[1fr_2.25rem] gap-2">
                <Input
                  autoFocus
                  autoComplete="current-password"
                  type={visible ? "text" : "password"}
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                />
                <Button type="button" variant="outline" size="icon" onClick={() => setVisible((value) => !value)} aria-label={visible ? "Hide password" : "Show password"}>
                  {visible ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                </Button>
              </span>
            </label>
            {error ? <p className="rounded-md border border-red-500/40 bg-red-500/10 p-2 text-sm text-red-700 dark:text-red-200">{error}</p> : null}
            <Button type="submit" disabled={busy}>
              {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <KeyRound className="h-4 w-4" />}
              Login
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
