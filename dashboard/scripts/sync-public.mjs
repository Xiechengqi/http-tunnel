import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const out = resolve(root, "dashboard", "out");
const target = resolve(root, "crates", "http-tunnel-server", "public");

if (!existsSync(out)) {
  throw new Error("dashboard/out does not exist; run next build first");
}

rmSync(target, { recursive: true, force: true });
mkdirSync(target, { recursive: true });
cpSync(out, target, { recursive: true });

writeFileSync(
  resolve(target, "build-info.json"),
  JSON.stringify(
    {
      version: process.env.HTTP_TUNNEL_VERSION || "0.1.0",
      commit: process.env.HTTP_TUNNEL_COMMIT || "unknown",
      commitMessage: process.env.HTTP_TUNNEL_COMMIT_MESSAGE || "unknown",
      buildTime: process.env.HTTP_TUNNEL_BUILD_TIME || new Date().toISOString(),
    },
    null,
    2,
  ) + "\n",
);
