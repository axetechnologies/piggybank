export interface AuthEnv {
  AUTHGATE_ISSUER: string;
  AUTHGATE_CLIENT_ID: string;
  AUTHGATE_CLIENT_SECRET: string;
  SESSION_SECRET: string;
}

const SESSION_COOKIE = "axelrod_session";
const TRANSIT_COOKIE = "axelrod_transit";
const SESSION_TTL_SEC = 60 * 60 * 8;
const TRANSIT_TTL_SEC = 600;

export interface Session {
  sub: string;
  email?: string;
  name?: string;
  exp: number;
}

export async function handleAuthRoute(
  request: Request,
  env: AuthEnv,
  url: URL
): Promise<Response | null> {
  if (url.pathname === "/auth/login" && request.method === "GET") {
    return startLogin(request, env, url);
  }
  if (url.pathname === "/auth/callback" && request.method === "GET") {
    return finishLogin(request, env, url);
  }
  if (url.pathname === "/auth/logout") {
    return logout(url);
  }
  if (url.pathname === "/auth/me" && request.method === "GET") {
    const s = await readSession(request, env);
    if (!s) return json({ authenticated: false }, 401);
    return json({ authenticated: true, sub: s.sub, email: s.email, name: s.name });
  }
  return null;
}

async function startLogin(request: Request, env: AuthEnv, url: URL): Promise<Response> {
  if (!env.AUTHGATE_CLIENT_ID) {
    return json({ error: "auth not configured — AUTHGATE_CLIENT_ID missing" }, 503);
  }
  const returnTo = sanitizeReturnTo(url.searchParams.get("return_to"));
  const verifier = randomString(64);
  const state = randomString(32);
  const challenge = await s256(verifier);

  const transit = await signJson({ v: verifier, s: state, r: returnTo }, env.SESSION_SECRET);

  const redirectUri = `${url.origin}/auth/callback`;
  const auth = new URL(`${env.AUTHGATE_ISSUER}/oauth/authorize`);
  auth.searchParams.set("response_type", "code");
  auth.searchParams.set("client_id", env.AUTHGATE_CLIENT_ID);
  auth.searchParams.set("redirect_uri", redirectUri);
  auth.searchParams.set("scope", "openid profile email");
  auth.searchParams.set("state", state);
  auth.searchParams.set("code_challenge", challenge);
  auth.searchParams.set("code_challenge_method", "S256");

  return new Response(null, {
    status: 302,
    headers: {
      Location: auth.toString(),
      "Set-Cookie": cookie(TRANSIT_COOKIE, transit, TRANSIT_TTL_SEC),
    },
  });
}

async function finishLogin(request: Request, env: AuthEnv, url: URL): Promise<Response> {
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  if (!code || !state) return json({ error: "missing code/state" }, 400);

  const transitVal = getCookie(request, TRANSIT_COOKIE);
  if (!transitVal) return json({ error: "no auth session" }, 400);
  const transit = await verifyJson<{ v: string; s: string; r: string }>(transitVal, env.SESSION_SECRET);
  if (!transit || transit.s !== state) return json({ error: "state mismatch" }, 400);

  const redirectUri = `${url.origin}/auth/callback`;
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code,
    redirect_uri: redirectUri,
    client_id: env.AUTHGATE_CLIENT_ID,
    code_verifier: transit.v,
  });
  const headers: Record<string, string> = { "Content-Type": "application/x-www-form-urlencoded" };
  if (env.AUTHGATE_CLIENT_SECRET) {
    const basic = btoa(`${env.AUTHGATE_CLIENT_ID}:${env.AUTHGATE_CLIENT_SECRET}`);
    headers.Authorization = `Basic ${basic}`;
  }
  const tokenResp = await fetch(`${env.AUTHGATE_ISSUER}/oauth/token`, {
    method: "POST",
    headers,
    body: body.toString(),
  });
  if (!tokenResp.ok) {
    const text = await tokenResp.text();
    return json({ error: "token exchange failed", detail: text }, 502);
  }
  const tokens = (await tokenResp.json()) as { access_token: string; id_token?: string };

  const userinfoResp = await fetch(`${env.AUTHGATE_ISSUER}/oauth/userinfo`, {
    headers: { Authorization: `Bearer ${tokens.access_token}` },
  });
  if (!userinfoResp.ok) {
    return json({ error: "userinfo failed" }, 502);
  }
  const info = (await userinfoResp.json()) as {
    sub: string;
    email?: string;
    name?: string;
  };

  const session: Session = {
    sub: info.sub,
    email: info.email,
    name: info.name,
    exp: Math.floor(Date.now() / 1000) + SESSION_TTL_SEC,
  };
  const sealed = await signJson(session, env.SESSION_SECRET);

  const returnTo = transit.r || "/";
  const clearedTransit = clearCookie(TRANSIT_COOKIE);
  const setSession = cookie(SESSION_COOKIE, sealed, SESSION_TTL_SEC);
  return new Response(null, {
    status: 302,
    headers: [
      ["Location", returnTo],
      ["Set-Cookie", setSession],
      ["Set-Cookie", clearedTransit],
    ],
  });
}

function logout(url: URL): Response {
  return new Response(null, {
    status: 302,
    headers: {
      Location: url.searchParams.get("return_to") || "/",
      "Set-Cookie": clearCookie(SESSION_COOKIE),
    },
  });
}

export async function readSession(request: Request, env: AuthEnv): Promise<Session | null> {
  const val = getCookie(request, SESSION_COOKIE);
  if (!val) return null;
  const s = await verifyJson<Session>(val, env.SESSION_SECRET);
  if (!s) return null;
  if (s.exp < Math.floor(Date.now() / 1000)) return null;
  return s;
}

function sanitizeReturnTo(v: string | null): string {
  if (!v) return "/";
  if (!v.startsWith("/") || v.startsWith("//")) return "/";
  return v;
}

function cookie(name: string, value: string, maxAge: number): string {
  return `${name}=${value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${maxAge}`;
}
function clearCookie(name: string): string {
  return `${name}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0`;
}
function getCookie(request: Request, name: string): string | null {
  const header = request.headers.get("Cookie") ?? "";
  for (const part of header.split(";")) {
    const [k, ...rest] = part.trim().split("=");
    if (k === name) return rest.join("=");
  }
  return null;
}

function randomString(bytes: number): string {
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return b64url(buf);
}
async function s256(input: string): Promise<string> {
  const enc = new TextEncoder().encode(input);
  const digest = await crypto.subtle.digest("SHA-256", enc);
  return b64url(new Uint8Array(digest));
}
async function hmac(secret: string, msg: string): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const sig = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(msg));
  return new Uint8Array(sig);
}
async function signJson(obj: unknown, secret: string): Promise<string> {
  const payload = b64url(new TextEncoder().encode(JSON.stringify(obj)));
  const sig = b64url(await hmac(secret, payload));
  return `${payload}.${sig}`;
}
async function verifyJson<T>(token: string, secret: string): Promise<T | null> {
  const [payload, sig] = token.split(".");
  if (!payload || !sig) return null;
  const expected = b64url(await hmac(secret, payload));
  if (!timingSafeEqual(sig, expected)) return null;
  try {
    return JSON.parse(new TextDecoder().decode(b64urlDecode(payload))) as T;
  } catch {
    return null;
  }
}
function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let out = 0;
  for (let i = 0; i < a.length; i++) out |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return out === 0;
}
function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function b64urlDecode(s: string): Uint8Array {
  const pad = s.length % 4 === 0 ? "" : "=".repeat(4 - (s.length % 4));
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + pad);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
