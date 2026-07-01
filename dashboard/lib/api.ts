import type { ApiResponse, PageMeta, PageResult } from "@/lib/types";

export class ApiError extends Error {
  status: number;
  code?: string;

  constructor(message: string, status: number, code?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

export function cookie(name: string) {
  if (typeof document === "undefined") return "";
  return (
    document.cookie
      .split(";")
      .map((value) => value.trim())
      .find((value) => value.startsWith(`${name}=`))
      ?.slice(name.length + 1) || ""
  );
}

export function qs(params: Record<string, string | number | boolean | null | undefined>) {
  const text = new URLSearchParams(
    Object.entries(params)
      .filter(([, value]) => value !== "" && value !== null && value !== undefined)
      .map(([key, value]) => [key, String(value)]),
  ).toString();
  return text ? `?${text}` : "";
}

export async function publicApi<T>(path: string, init?: RequestInit): Promise<T> {
  return readApi<T>(await fetch(path, { cache: "no-store", ...init }));
}

export async function adminApi<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("content-type", headers.get("content-type") || "application/json");
  headers.set("x-csrf-token", cookie("http_tunnel_csrf"));
  const response = await fetch(path, {
    cache: "no-store",
    ...init,
    headers,
  });
  if (response.status === 401 && typeof window !== "undefined") {
    window.location.href = "/admin/login";
  }
  return readApi<T>(response);
}

export async function adminText(path: string, init: RequestInit = {}) {
  const response = await fetch(path, { cache: "no-store", ...init });
  if (response.status === 401 && typeof window !== "undefined") {
    window.location.href = "/admin/login";
  }
  const text = await response.text();
  if (!response.ok) throw new ApiError(text || response.statusText, response.status);
  return text;
}

export async function adminPage<T>(path: string): Promise<PageResult<T>> {
  const response = await fetch(path, {
    cache: "no-store",
    headers: { "x-csrf-token": cookie("http_tunnel_csrf") },
  });
  if (response.status === 401 && typeof window !== "undefined") {
    window.location.href = "/admin/login";
  }
  const body = await response.json().catch(() => null);
  if (!response.ok || !body?.ok) {
    throw new ApiError(body?.error?.message || response.statusText, response.status, body?.error?.code);
  }
  return {
    data: body.data || [],
    meta: pageMeta(response.headers),
  };
}

export async function downloadBlob(path: string, init: RequestInit = {}) {
  const headers = new Headers(init.headers);
  headers.set("x-csrf-token", cookie("http_tunnel_csrf"));
  const response = await fetch(path, { cache: "no-store", ...init, headers });
  if (response.status === 401 && typeof window !== "undefined") {
    window.location.href = "/admin/login";
  }
  if (!response.ok) throw new ApiError(await response.text(), response.status);
  return response;
}

async function readApi<T>(response: Response): Promise<T> {
  const body = (await response.json().catch(() => null)) as ApiResponse<T> | null;
  if (!response.ok || !body?.ok) {
    throw new ApiError(body?.error?.message || response.statusText, response.status, body?.error?.code);
  }
  return body.data as T;
}

function pageMeta(headers: Headers): PageMeta {
  return {
    total: Number(headers.get("x-http-tunnel-total-count") || 0),
    limit: Number(headers.get("x-http-tunnel-limit") || 50),
    offset: Number(headers.get("x-http-tunnel-offset") || 0),
    hasMore: headers.get("x-http-tunnel-has-more") === "true",
  };
}
