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

    if (url.pathname === "/" && request.method === "GET") {
      return json({ service: "axelrod", status: "ok" });
    }

    if (url.pathname !== "/ingest") {
      return json({ error: "not found" }, 405);
    }

    if (request.method !== "POST") {
      return json({ error: "method not allowed" }, 405);
    }

    const auth = request.headers.get("Authorization") ?? "";
    if (!auth.startsWith("Bearer ") || auth.slice(7) !== env.INGEST_KEY) {
      return json({ error: "unauthorized" }, 401);
    }

    let body: string;
    try {
      body = await request.text();
    } catch {
      return json({ error: "could not read body" }, 400);
    }

    const lines = body.split("\n").filter((l) => l.trim() !== "");

    if (lines.length === 0) {
      return json({ error: "empty body" }, 400);
    }

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
    } catch (err) {
      return json({ error: "storage error" }, 500);
    }

    return json({ ok: true, lines: records.length });
  },
};

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
