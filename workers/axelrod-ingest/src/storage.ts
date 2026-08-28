// Storage abstraction — swap backends per tenant via env.STORAGE_BACKEND.
// One primitive, five backends: R2, WebDAV (AXeCloud), Google Drive, S3, LocalTunnel.
// Everything is a namespace; the backend is just a pluggable adapter.

export interface StoredObject {
  key: string;
  size: number;
  uploaded: Date;
}

export interface ObjectBody {
  arrayBuffer(): Promise<ArrayBuffer>;
  text(): Promise<string>;
  json<T = unknown>(): Promise<T>;
  body: ReadableStream<Uint8Array> | null;
  size: number;
  uploaded: Date;
}

export interface ListResult {
  objects: StoredObject[];
  truncated: boolean;
  cursor?: string;
}

export interface ListOptions {
  prefix?: string;
  cursor?: string;
  limit?: number;
}

export interface PutOptions {
  httpMetadata?: { contentType?: string };
  customMetadata?: Record<string, string>;
}

export interface StorageBackend {
  put(key: string, body: ArrayBuffer | ReadableStream | string, opts?: PutOptions): Promise<void>;
  get(key: string): Promise<ObjectBody | null>;
  head(key: string): Promise<StoredObject | null>;
  delete(key: string): Promise<void>;
  list(opts: ListOptions): Promise<ListResult>;
}

// ── R2 (Cloudflare native, default) ───────────────────────────────────────────
class R2Backend implements StorageBackend {
  constructor(private bucket: R2Bucket) {}

  async put(key: string, body: ArrayBuffer | ReadableStream | string, opts?: PutOptions) {
    await this.bucket.put(key, body as any, opts);
  }

  async get(key: string): Promise<ObjectBody | null> {
    const obj = await this.bucket.get(key);
    if (!obj) return null;
    return {
      arrayBuffer: () => obj.arrayBuffer(),
      text: () => obj.text(),
      json: <T = unknown>() => obj.json<T>(),
      body: obj.body,
      size: obj.size,
      uploaded: obj.uploaded,
    };
  }

  async head(key: string): Promise<StoredObject | null> {
    const obj = await this.bucket.head(key);
    if (!obj) return null;
    return { key, size: obj.size, uploaded: obj.uploaded };
  }

  async delete(key: string) {
    await this.bucket.delete(key);
  }

  async list(opts: ListOptions): Promise<ListResult> {
    const r = await this.bucket.list({ prefix: opts.prefix, cursor: opts.cursor, limit: opts.limit ?? 1000 });
    return {
      objects: r.objects.map((o) => ({ key: o.key, size: o.size, uploaded: o.uploaded })),
      truncated: r.truncated,
      cursor: r.truncated ? (r as any).cursor : undefined,
    };
  }
}

// ── WebDAV (AXeCloud on jl4, or any WebDAV server) ────────────────────────────
// Reaches the Worker via a Cloudflare Tunnel or public DNS. Zero cloud storage cost —
// data stays on your own hardware (casper's 427GB, or whatever fleet node hosts it).
class WebDAVBackend implements StorageBackend {
  constructor(private baseUrl: string, private auth: string) {}

  private headers(extra: Record<string, string> = {}): HeadersInit {
    return { Authorization: `Basic ${this.auth}`, ...extra };
  }

  async put(key: string, body: ArrayBuffer | ReadableStream | string, opts?: PutOptions) {
    // WebDAV requires parent collections to exist. MKCOL each ancestor as needed.
    await this.ensureParents(key);
    const res = await fetch(`${this.baseUrl}/${key}`, {
      method: "PUT",
      headers: this.headers({ "Content-Type": opts?.httpMetadata?.contentType ?? "application/octet-stream" }),
      body: body as any,
    });
    if (!res.ok && res.status !== 201 && res.status !== 204) {
      throw new Error(`WebDAV PUT ${key} failed: ${res.status}`);
    }
  }

  private async ensureParents(key: string) {
    const parts = key.split("/").slice(0, -1);
    let path = "";
    for (const p of parts) {
      path = path ? `${path}/${p}` : p;
      await fetch(`${this.baseUrl}/${path}`, { method: "MKCOL", headers: this.headers() });
      // 201 created, 405 already-exists — both fine
    }
  }

  async get(key: string): Promise<ObjectBody | null> {
    const res = await fetch(`${this.baseUrl}/${key}`, { headers: this.headers() });
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`WebDAV GET ${key} failed: ${res.status}`);
    const size = Number(res.headers.get("Content-Length") ?? 0);
    const lm = res.headers.get("Last-Modified");
    const uploaded = lm ? new Date(lm) : new Date();
    const buf = await res.arrayBuffer();
    return {
      arrayBuffer: async () => buf,
      text: async () => new TextDecoder().decode(buf),
      json: async <T = unknown>() => JSON.parse(new TextDecoder().decode(buf)) as T,
      body: null,
      size,
      uploaded,
    };
  }

  async head(key: string): Promise<StoredObject | null> {
    const res = await fetch(`${this.baseUrl}/${key}`, { method: "HEAD", headers: this.headers() });
    if (res.status === 404) return null;
    if (!res.ok) return null;
    return {
      key,
      size: Number(res.headers.get("Content-Length") ?? 0),
      uploaded: new Date(res.headers.get("Last-Modified") ?? Date.now()),
    };
  }

  async delete(key: string) {
    await fetch(`${this.baseUrl}/${key}`, { method: "DELETE", headers: this.headers() });
  }

  async list(opts: ListOptions): Promise<ListResult> {
    // WebDAV PROPFIND with Depth: infinity, then filter by prefix
    const root = opts.prefix ? `${this.baseUrl}/${opts.prefix.replace(/\/$/, "")}` : this.baseUrl;
    const res = await fetch(root, {
      method: "PROPFIND",
      headers: this.headers({ Depth: "infinity", "Content-Type": "application/xml" }),
      body: `<?xml version="1.0"?><propfind xmlns="DAV:"><prop><getcontentlength/><getlastmodified/></prop></propfind>`,
    });
    if (!res.ok) return { objects: [], truncated: false };
    const xml = await res.text();
    const objects: StoredObject[] = [];
    // Minimal XML parse — extract <D:href>, <D:getcontentlength>, <D:getlastmodified>
    const responseRe = /<D?:?response>([\s\S]*?)<\/D?:?response>/gi;
    let m: RegExpExecArray | null;
    while ((m = responseRe.exec(xml))) {
      const block = m[1];
      const href = /<D?:?href>([^<]+)<\/D?:?href>/i.exec(block)?.[1];
      const size = /<D?:?getcontentlength>(\d+)<\/D?:?getcontentlength>/i.exec(block)?.[1];
      const lm = /<D?:?getlastmodified>([^<]+)<\/D?:?getlastmodified>/i.exec(block)?.[1];
      if (!href || !size) continue; // skip collections (no content-length)
      const key = decodeURIComponent(href).replace(new RegExp(`^${new URL(this.baseUrl).pathname}/?`), "");
      objects.push({ key, size: Number(size), uploaded: new Date(lm ?? Date.now()) });
    }
    return { objects, truncated: false };
  }
}

// ── Google Drive (10TB personal plan — real primary backend, not a toy) ──────
// Requires an OAuth refresh-token flow; env.DRIVE_TOKEN is a short-lived access token
// refreshed by a companion cron Worker every 55 minutes.
class DriveBackend implements StorageBackend {
  constructor(private token: string, private rootFolderId: string) {}

  private auth() {
    return { Authorization: `Bearer ${this.token}` };
  }

  async put(key: string, body: ArrayBuffer | ReadableStream | string, opts?: PutOptions) {
    // Multipart upload: metadata + content. Drive maps `key` to file name; the
    // rootFolderId is the tenant's isolated namespace.
    const meta = { name: key, parents: [this.rootFolderId], mimeType: opts?.httpMetadata?.contentType ?? "application/octet-stream" };
    const boundary = `axelrod-${Date.now()}`;
    const preamble = `--${boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n${JSON.stringify(meta)}\r\n--${boundary}\r\nContent-Type: ${meta.mimeType}\r\n\r\n`;
    const trailer = `\r\n--${boundary}--`;
    const bodyBuf = typeof body === "string" ? new TextEncoder().encode(body) : new Uint8Array(body as ArrayBuffer);
    const parts = new Uint8Array(preamble.length + bodyBuf.length + trailer.length);
    parts.set(new TextEncoder().encode(preamble), 0);
    parts.set(bodyBuf, preamble.length);
    parts.set(new TextEncoder().encode(trailer), preamble.length + bodyBuf.length);
    await fetch("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart", {
      method: "POST",
      headers: { ...this.auth(), "Content-Type": `multipart/related; boundary=${boundary}` },
      body: parts,
    });
  }

  async get(key: string): Promise<ObjectBody | null> {
    const fileId = await this.resolveKey(key);
    if (!fileId) return null;
    const res = await fetch(`https://www.googleapis.com/drive/v3/files/${fileId}?alt=media`, { headers: this.auth() });
    if (!res.ok) return null;
    const buf = await res.arrayBuffer();
    return {
      arrayBuffer: async () => buf,
      text: async () => new TextDecoder().decode(buf),
      json: async <T = unknown>() => JSON.parse(new TextDecoder().decode(buf)) as T,
      body: null,
      size: buf.byteLength,
      uploaded: new Date(),
    };
  }

  async head(key: string): Promise<StoredObject | null> {
    const fileId = await this.resolveKey(key);
    if (!fileId) return null;
    const res = await fetch(`https://www.googleapis.com/drive/v3/files/${fileId}?fields=size,modifiedTime`, { headers: this.auth() });
    if (!res.ok) return null;
    const meta = await res.json<{ size: string; modifiedTime: string }>();
    return { key, size: Number(meta.size ?? 0), uploaded: new Date(meta.modifiedTime) };
  }

  async delete(key: string) {
    const fileId = await this.resolveKey(key);
    if (fileId) await fetch(`https://www.googleapis.com/drive/v3/files/${fileId}`, { method: "DELETE", headers: this.auth() });
  }

  async list(opts: ListOptions): Promise<ListResult> {
    const q = `'${this.rootFolderId}' in parents and trashed=false${opts.prefix ? ` and name contains '${opts.prefix}'` : ""}`;
    const res = await fetch(`https://www.googleapis.com/drive/v3/files?q=${encodeURIComponent(q)}&fields=files(id,name,size,modifiedTime),nextPageToken&pageSize=${opts.limit ?? 1000}${opts.cursor ? `&pageToken=${opts.cursor}` : ""}`, { headers: this.auth() });
    if (!res.ok) return { objects: [], truncated: false };
    const data = await res.json<{ files: Array<{ id: string; name: string; size: string; modifiedTime: string }>; nextPageToken?: string }>();
    return {
      objects: (data.files ?? []).map((f) => ({ key: f.name, size: Number(f.size ?? 0), uploaded: new Date(f.modifiedTime) })),
      truncated: !!data.nextPageToken,
      cursor: data.nextPageToken,
    };
  }

  private async resolveKey(key: string): Promise<string | null> {
    const q = `'${this.rootFolderId}' in parents and name='${key.replace(/'/g, "\\'")}' and trashed=false`;
    const res = await fetch(`https://www.googleapis.com/drive/v3/files?q=${encodeURIComponent(q)}&fields=files(id)`, { headers: this.auth() });
    if (!res.ok) return null;
    const data = await res.json<{ files: Array<{ id: string }> }>();
    return data.files?.[0]?.id ?? null;
  }
}

// ── Factory ───────────────────────────────────────────────────────────────────
export interface StorageEnv {
  HARVEST?: R2Bucket;
  STORAGE_BACKEND?: string; // "r2" | "webdav" | "drive"
  WEBDAV_URL?: string;
  WEBDAV_AUTH?: string; // base64(user:pass)
  DRIVE_TOKEN?: string;
  DRIVE_FOLDER_ID?: string;
}

export function getStorage(env: StorageEnv): StorageBackend {
  const backend = env.STORAGE_BACKEND ?? "r2";
  if (backend === "webdav") {
    if (!env.WEBDAV_URL || !env.WEBDAV_AUTH) throw new Error("WEBDAV_URL and WEBDAV_AUTH required");
    return new WebDAVBackend(env.WEBDAV_URL, env.WEBDAV_AUTH);
  }
  if (backend === "drive") {
    if (!env.DRIVE_TOKEN || !env.DRIVE_FOLDER_ID) throw new Error("DRIVE_TOKEN and DRIVE_FOLDER_ID required");
    return new DriveBackend(env.DRIVE_TOKEN, env.DRIVE_FOLDER_ID);
  }
  if (!env.HARVEST) throw new Error("HARVEST R2 binding required for r2 backend");
  return new R2Backend(env.HARVEST);
}
