import { renderFrontend } from "./frontend";

export interface Env {
  HARVEST: R2Bucket;
  INGEST_KEY: string;
}

const VALID_EVENTS = new Set([
  "compress",
  "decompress",
  "tool_call",
  "subagent_spawn",
  "subagent_complete",
  "context_transfer",
  "session_end",
]);

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const { pathname } = url;

    const wantsHtml = (request.headers.get("Accept") ?? "").includes("text/html");

    // GET / — HTML frontend or JSON status
    if (pathname === "/" && request.method === "GET") {
      if (wantsHtml) return html();
      return json({ service: "axelrod", status: "ok" });
    }

    // POST /ingest — original route, no auth refactor needed
    if (pathname === "/ingest" && request.method === "POST") {
      return handleIngest(request, env);
    }

    // All /datasets/* routes require auth
    if (pathname === "/datasets" || pathname.startsWith("/datasets/")) {
      const authErr = checkAuth(request, env);
      if (authErr) return authErr;
      return handleDatasets(request, env, url);
    }

    // Catch-all: serve SPA for browser navigations to unknown paths
    if (request.method === "GET" && wantsHtml) {
      return html();
    }

    return json({ error: "not found" }, 404);
  },
};

// ── Auth ──────────────────────────────────────────────────────────────────────

function checkAuth(request: Request, env: Env): Response | null {
  const auth = request.headers.get("Authorization") ?? "";
  if (!auth.startsWith("Bearer ") || auth.slice(7) !== env.INGEST_KEY) {
    return json({ error: "unauthorized" }, 401);
  }
  return null;
}

// ── /ingest (existing) ────────────────────────────────────────────────────────

async function handleIngest(request: Request, env: Env): Promise<Response> {
  const authErr = checkAuth(request, env);
  if (authErr) return authErr;

  let body: string;
  try {
    body = await request.text();
  } catch {
    return json({ error: "could not read body" }, 400);
  }

  const lines = body.split("\n").filter((l) => l.trim() !== "");
  if (lines.length === 0) return json({ error: "empty body" }, 400);

  const records: object[] = [];
  for (const line of lines) {
    let obj: Record<string, unknown>;
    try {
      obj = JSON.parse(line);
    } catch {
      return json({ error: "invalid JSON line", line }, 400);
    }
    if (typeof obj.event !== "string" || !VALID_EVENTS.has(obj.event)) {
      return json({ error: "invalid or missing event field", line }, 400);
    }
    if (typeof obj.session_id !== "string" || obj.session_id === "") {
      return json({ error: "missing session_id field", line }, 400);
    }
    records.push(obj);
  }

  const firstRecord = records[0] as Record<string, unknown>;
  const sessionId = firstRecord.session_id as string;
  const now = new Date();
  const date = now.toISOString().slice(0, 10);
  const ts = now.getTime();
  const key = `harvest/${date}/${sessionId}/${ts}.jsonl`;
  const payload = records.map((r) => JSON.stringify(r)).join("\n") + "\n";

  try {
    await env.HARVEST.put(key, payload, {
      httpMetadata: { contentType: "application/x-ndjson" },
    });
  } catch {
    return json({ error: "storage error" }, 500);
  }

  return json({ ok: true, lines: records.length });
}

// ── /datasets router ──────────────────────────────────────────────────────────

async function handleDatasets(
  request: Request,
  env: Env,
  url: URL
): Promise<Response> {
  const { pathname } = url;

  // GET /datasets
  if (pathname === "/datasets" && request.method === "GET") {
    return listDatasets(env);
  }

  // Extract /{name}[/action] from /datasets/{name}[/action]
  const rest = pathname.slice("/datasets/".length); // e.g. "foo/download"
  const slashIdx = rest.indexOf("/");
  const name = slashIdx === -1 ? rest : rest.slice(0, slashIdx);
  const action = slashIdx === -1 ? "" : rest.slice(slashIdx + 1);

  if (!name) return json({ error: "dataset name required" }, 400);

  if (action === "" || action === undefined) {
    if (request.method === "GET") return getDatasetMeta(env, name);
    if (request.method === "DELETE") return deleteDataset(env, name);
    return json({ error: "method not allowed" }, 405);
  }

  if (action === "download" && request.method === "GET") {
    return downloadDataset(env, name, url);
  }
  if (action === "sample" && request.method === "GET") {
    const n = Math.max(1, parseInt(url.searchParams.get("n") ?? "10", 10));
    return sampleDataset(env, name, n);
  }
  if (action === "upload" && request.method === "POST") {
    return uploadDataset(request, env, name);
  }
  if (action === "stats" && request.method === "GET") {
    return datasetStats(env, name);
  }

  return json({ error: "not found" }, 404);
}

// ── Route implementations ─────────────────────────────────────────────────────

// GET /datasets
async function listDatasets(env: Env): Promise<Response> {
  const results: { name: string; type: string; size_bytes: number; updated_at: string }[] = [];

  // Named datasets under datasets/
  const named = await listAllKeys(env, "datasets/");
  const datasetNames = new Set<string>();
  for (const key of named) {
    const parts = key.slice("datasets/".length).split("/");
    if (parts[0]) datasetNames.add(parts[0]);
  }

  for (const name of datasetNames) {
    const meta = await readMeta(env, name);
    if (meta) {
      results.push({
        name,
        type: "dataset",
        size_bytes: (meta.size_bytes as number) ?? 0,
        updated_at: (meta.updated_at as string) ?? "",
      });
    } else {
      // Synthesize from object listing
      const keys = await listAllKeys(env, `datasets/${name}/data/`);
      let size = 0;
      let updated = "";
      for (const k of keys) {
        const obj = await env.HARVEST.head(k);
        if (obj) {
          size += obj.size;
          const ua = obj.uploaded.toISOString();
          if (ua > updated) updated = ua;
        }
      }
      results.push({ name, type: "dataset", size_bytes: size, updated_at: updated });
    }
  }

  // Harvest pseudo-dataset
  const harvestKeys = await listAllKeys(env, "harvest/");
  if (harvestKeys.length > 0) {
    let size = 0;
    let updated = "";
    for (const k of harvestKeys) {
      const obj = await env.HARVEST.head(k);
      if (obj) {
        size += obj.size;
        const ua = obj.uploaded.toISOString();
        if (ua > updated) updated = ua;
      }
    }
    results.push({ name: "harvest", type: "harvest", size_bytes: size, updated_at: updated });
  }

  return json(results);
}

// GET /datasets/{name}
async function getDatasetMeta(env: Env, name: string): Promise<Response> {
  if (name === "harvest") {
    return synthesizeHarvestMeta(env);
  }
  const meta = await readMeta(env, name);
  if (!meta) return json({ error: "dataset not found" }, 404);
  return json(meta);
}

// DELETE /datasets/{name}
async function deleteDataset(env: Env, name: string): Promise<Response> {
  if (name === "harvest") return json({ error: "harvest dataset cannot be deleted" }, 400);

  const keys = await listAllKeys(env, `datasets/${name}/`);
  if (keys.length === 0) return json({ error: "dataset not found" }, 404);

  await Promise.all(keys.map((k) => env.HARVEST.delete(k)));
  return json({ ok: true, deleted: keys.length });
}

// GET /datasets/{name}/download
async function downloadDataset(env: Env, name: string, url: URL): Promise<Response> {
  const dateFilter = url.searchParams.get("date");
  let prefix: string;
  let keys: string[];

  if (name === "harvest") {
    prefix = dateFilter ? `harvest/${dateFilter}/` : "harvest/";
    keys = await listAllKeys(env, prefix);
  } else {
    keys = await listAllKeys(env, `datasets/${name}/data/`);
    if (keys.length === 0) return json({ error: "dataset not found" }, 404);
  }

  keys.sort();

  // Stream concatenated JSONL
  const { readable, writable } = new TransformStream();
  const writer = writable.getWriter();
  const encoder = new TextEncoder();

  (async () => {
    for (const key of keys) {
      const obj = await env.HARVEST.get(key);
      if (!obj) continue;
      const text = await obj.text();
      await writer.write(encoder.encode(text.endsWith("\n") ? text : text + "\n"));
    }
    await writer.close();
  })();

  return new Response(readable, {
    headers: { "Content-Type": "application/x-ndjson" },
  });
}

// GET /datasets/{name}/sample?n=10
async function sampleDataset(env: Env, name: string, n: number): Promise<Response> {
  let keys: string[];

  if (name === "harvest") {
    keys = await listAllKeys(env, "harvest/");
  } else {
    keys = await listAllKeys(env, `datasets/${name}/data/`);
    if (keys.length === 0) return json({ error: "dataset not found" }, 404);
  }

  keys.sort();

  const rows: unknown[] = [];
  for (const key of keys) {
    if (rows.length >= n) break;
    const obj = await env.HARVEST.get(key);
    if (!obj) continue;
    const text = await obj.text();
    for (const line of text.split("\n")) {
      if (rows.length >= n) break;
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        rows.push(JSON.parse(trimmed));
      } catch {
        // skip malformed
      }
    }
  }

  return json(rows);
}

// POST /datasets/{name}/upload
async function uploadDataset(request: Request, env: Env, name: string): Promise<Response> {
  if (name === "harvest") return json({ error: "cannot upload to harvest" }, 400);

  let body: string;
  try {
    body = await request.text();
  } catch {
    return json({ error: "could not read body" }, 400);
  }

  const lines = body.split("\n").filter((l) => l.trim() !== "");
  if (lines.length === 0) return json({ error: "empty body" }, 400);

  // Parse and validate all lines; infer schema from first record
  const records: Record<string, unknown>[] = [];
  let schema: Record<string, string> | null = null;

  for (const line of lines) {
    let obj: Record<string, unknown>;
    try {
      obj = JSON.parse(line);
    } catch {
      return json({ error: "invalid JSON line", line }, 400);
    }
    if (!schema) {
      schema = inferSchema(obj);
    }
    records.push(obj);
  }

  const now = new Date();
  const ts = now.getTime();
  const chunkKey = `datasets/${name}/data/${ts}.jsonl`;
  const payload = records.map((r) => JSON.stringify(r)).join("\n") + "\n";

  await env.HARVEST.put(chunkKey, payload, {
    httpMetadata: { contentType: "application/x-ndjson" },
  });

  // Update metadata
  const existingMeta = (await readMeta(env, name)) ?? {
    name,
    row_count: 0,
    size_bytes: 0,
    schema: {},
    chunks: 0,
    created_at: now.toISOString(),
    updated_at: "",
  };

  existingMeta.row_count = ((existingMeta.row_count as number) ?? 0) + records.length;
  existingMeta.size_bytes = ((existingMeta.size_bytes as number) ?? 0) + payload.length;
  existingMeta.schema = schema ?? existingMeta.schema;
  existingMeta.chunks = ((existingMeta.chunks as number) ?? 0) + 1;
  existingMeta.updated_at = now.toISOString();

  await env.HARVEST.put(
    `datasets/${name}/metadata.json`,
    JSON.stringify(existingMeta),
    { httpMetadata: { contentType: "application/json" } }
  );

  return json({ ok: true, lines: records.length, key: chunkKey });
}

// GET /datasets/{name}/stats (harvest-focused)
async function datasetStats(env: Env, name: string): Promise<Response> {
  const prefix = name === "harvest" ? "harvest/" : `datasets/${name}/data/`;
  const keys = await listAllKeys(env, prefix);

  if (keys.length === 0) return json({ error: "dataset not found" }, 404);

  const eventCounts: Record<string, number> = {};
  const sessions = new Set<string>();
  let totalBytes = 0;
  let totalRows = 0;
  let minDate = "";
  let maxDate = "";

  for (const key of keys) {
    const obj = await env.HARVEST.get(key);
    if (!obj) continue;
    const size = obj.size ?? 0;
    totalBytes += size;
    const text = await obj.text();
    for (const line of text.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        const record = JSON.parse(trimmed) as Record<string, unknown>;
        totalRows++;
        if (typeof record.event === "string") {
          eventCounts[record.event] = (eventCounts[record.event] ?? 0) + 1;
        }
        if (typeof record.session_id === "string") {
          sessions.add(record.session_id);
        }
        if (typeof record.ts === "string" || typeof record.timestamp === "string") {
          const d = (record.ts ?? record.timestamp) as string;
          const date = d.slice(0, 10);
          if (!minDate || date < minDate) minDate = date;
          if (!maxDate || date > maxDate) maxDate = date;
        }
      } catch {
        // skip
      }
    }
  }

  // Extract date range from keys if not found in records
  if (!minDate) {
    for (const key of keys) {
      const parts = key.split("/");
      const datePart = parts[1]; // harvest/{date}/...
      if (datePart && /^\d{4}-\d{2}-\d{2}$/.test(datePart)) {
        if (!minDate || datePart < minDate) minDate = datePart;
        if (!maxDate || datePart > maxDate) maxDate = datePart;
      }
    }
  }

  return json({
    name,
    total_rows: totalRows,
    total_bytes: totalBytes,
    session_count: sessions.size,
    event_distribution: eventCounts,
    date_range: { from: minDate || null, to: maxDate || null },
    chunk_count: keys.length,
  });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async function listAllKeys(env: Env, prefix: string): Promise<string[]> {
  const keys: string[] = [];
  let cursor: string | undefined;
  do {
    const result: R2Objects = await env.HARVEST.list({ prefix, cursor, limit: 1000 });
    for (const obj of result.objects) {
      keys.push(obj.key);
    }
    cursor = result.truncated ? result.cursor : undefined;
  } while (cursor);
  return keys;
}

async function readMeta(env: Env, name: string): Promise<Record<string, unknown> | null> {
  const obj = await env.HARVEST.get(`datasets/${name}/metadata.json`);
  if (!obj) return null;
  try {
    return await obj.json<Record<string, unknown>>();
  } catch {
    return null;
  }
}

async function synthesizeHarvestMeta(env: Env): Promise<Response> {
  const keys = await listAllKeys(env, "harvest/");
  const dates = new Set<string>();
  const sessions = new Set<string>();
  let size = 0;
  let updated = "";

  for (const k of keys) {
    const parts = k.split("/"); // harvest/date/session/ts.jsonl
    if (parts[1]) dates.add(parts[1]);
    if (parts[2]) sessions.add(parts[2]);
    const obj = await env.HARVEST.head(k);
    if (obj) {
      size += obj.size;
      const ua = obj.uploaded.toISOString();
      if (ua > updated) updated = ua;
    }
  }

  const sortedDates = [...dates].sort();
  return json({
    name: "harvest",
    type: "harvest",
    size_bytes: size,
    updated_at: updated,
    date_count: dates.size,
    session_count: sessions.size,
    date_range: {
      from: sortedDates[0] ?? null,
      to: sortedDates[sortedDates.length - 1] ?? null,
    },
  });
}

function inferSchema(obj: Record<string, unknown>): Record<string, string> {
  const schema: Record<string, string> = {};
  for (const [k, v] of Object.entries(obj)) {
    if (v === null) schema[k] = "null";
    else if (Array.isArray(v)) schema[k] = "array";
    else schema[k] = typeof v;
  }
  return schema;
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function html(): Response {
  return new Response(renderFrontend(), {
    headers: { "Content-Type": "text/html;charset=UTF-8" },
  });
}
