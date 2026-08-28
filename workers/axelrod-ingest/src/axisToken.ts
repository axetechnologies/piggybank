// HS256 verification for axis-minted CLI/API tokens.
// Matches the mint shape emitted by axis /device/token (see axis src/device.ts).
// Shared TOKEN_SIGNING_SECRET must equal the one axis uses.

const AXIS_ISSUER = "https://axis.axe.onl";

export interface AxisClaims {
  iss: string;
  sub: string;
  aud?: string;
  workspace?: string;
  hostname?: string;
  scope?: string;
  jti?: string;
  iat: number;
  exp: number;
}

export async function verifyAxisToken(
  token: string,
  secret: string
): Promise<AxisClaims | null> {
  const parts = token.split(".");
  if (parts.length !== 3) return null;
  const [h, p, s] = parts;

  const header = safeJson<{ alg?: string; typ?: string }>(h);
  if (!header || header.alg !== "HS256") return null;

  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"]
  );
  const sigBytes = b64urlDecode(s);
  const ok = await crypto.subtle.verify(
    "HMAC",
    key,
    sigBytes,
    new TextEncoder().encode(`${h}.${p}`)
  );
  if (!ok) return null;

  const claims = safeJson<AxisClaims>(p);
  if (!claims) return null;
  if (claims.iss !== AXIS_ISSUER) return null;
  if (typeof claims.sub !== "string" || !claims.sub) return null;
  if (typeof claims.exp !== "number" || claims.exp < Math.floor(Date.now() / 1000)) return null;

  return claims;
}

function safeJson<T>(seg: string): T | null {
  try {
    return JSON.parse(new TextDecoder().decode(b64urlDecode(seg))) as T;
  } catch {
    return null;
  }
}

function b64urlDecode(s: string): Uint8Array {
  const pad = s.length % 4 === 0 ? "" : "=".repeat(4 - (s.length % 4));
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + pad);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
