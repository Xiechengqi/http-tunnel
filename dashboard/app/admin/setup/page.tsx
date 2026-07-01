"use client";

import { FormEvent, type ReactNode, useEffect, useState } from "react";
import { Eye, EyeOff, Loader2, SlidersHorizontal } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { publicApi } from "@/lib/api";
import type { SetupStatus } from "@/lib/types";

export default function SetupPage() {
  const [password, setPassword] = useState("");
  const [visible, setVisible] = useState(false);
  const [domain, setDomain] = useState("");
  const [scheme, setScheme] = useState("https");
  const [addr, setAddr] = useState("0.0.0.0:8080");
  const [databaseUrl, setDatabaseUrl] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    publicApi<SetupStatus>("/api/admin/setup/status")
      .then((status) => {
        if (status.database_url) setDatabaseUrl(status.database_url);
      })
      .catch(() => setError("Unable to load setup defaults. Refresh and try again."));
  }, []);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setError("");
    if (password.length < 8) {
      setError("Admin password must be at least 8 characters.");
      return;
    }
    if (!domain.trim()) {
      setError("Domain is required.");
      return;
    }
    setBusy(true);
    try {
      const response = await fetch("/api/admin/setup/init", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          admin_password: password,
          confirm_password: password,
          domain,
          public_scheme: scheme,
          addr,
          database_url: databaseUrl,
        }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok || !body?.ok) {
        throw new Error(body?.error?.message || "Setup failed.");
      }
      window.location.href = "/admin/login";
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="ops-shell grid min-h-screen place-items-center px-4 py-8">
      <Card className="w-full max-w-xl">
        <CardHeader>
          <CardTitle>http-tunnel setup</CardTitle>
        </CardHeader>
        <CardContent>
          <form className="grid gap-4" onSubmit={submit}>
            <Field label="Admin password" help="Required. At least 8 characters.">
              <span className="grid grid-cols-[1fr_2.25rem] gap-2">
                <Input type={visible ? "text" : "password"} autoComplete="new-password" value={password} onChange={(event) => setPassword(event.target.value)} />
                <Button type="button" variant="outline" size="icon" onClick={() => setVisible((value) => !value)} aria-label={visible ? "Hide password" : "Show password"}>
                  {visible ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                </Button>
              </span>
            </Field>
            <Field label="Domain" help="Required. Base domain for public tunnel hosts, for example example.com.">
              <Input value={domain} onChange={(event) => setDomain(event.target.value)} placeholder="example.com" />
            </Field>
            <Field label="Public scheme" help="Required. Use https for public TLS/proxy deployments; use http for local plain HTTP.">
              <Select value={scheme} onChange={(event) => setScheme(event.target.value)}>
                <option value="https">https</option>
                <option value="http">http</option>
              </Select>
            </Field>

            <details className="rounded-lg border border-border bg-secondary/30 p-3">
              <summary className="flex cursor-pointer items-center gap-2 text-sm font-medium">
                <SlidersHorizontal className="h-4 w-4" />
                Advanced optional settings
              </summary>
              <div className="mt-4 grid gap-4">
                <Field label="Listen address" help="Optional. Keep 0.0.0.0:8080 unless you need a different port.">
                  <Input value={addr} onChange={(event) => setAddr(event.target.value)} />
                </Field>
                <Field label="Database URL" help="Optional override. Defaults to $HOME/.http-tunnel/http-tunnel.sqlite3.">
                  <Input value={databaseUrl} onChange={(event) => setDatabaseUrl(event.target.value)} />
                </Field>
              </div>
            </details>
            {error ? <p className="rounded-md border border-red-500/40 bg-red-500/10 p-2 text-sm text-red-700 dark:text-red-200">{error}</p> : null}
            <Button type="submit" disabled={busy}>
              {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
              Initialize
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}

function Field({ label, help, children }: { label: string; help: string; children: ReactNode }) {
  return (
    <label className="grid gap-2 text-sm">
      <span className="font-medium">{label}</span>
      <span className="text-xs text-muted-foreground">{help}</span>
      {children}
    </label>
  );
}
