# Everything Is a Token

### A Unifying Thesis for the Axelrod Data Platform

---

## The Thesis

HuggingFace made **model** and **dataset** first-class nouns. Axelrod's opening is to make **token** the third — and to make every meaningful thing on the platform (a brand color, a permission grant, a dataset row, a compressed context chunk, a billable unit of work) an instance of the same primitive: **a content-addressed, signable, composable token**.

One primitive, five categories, compounding advantage.

---

## The Five Token Categories

| Category | What it is | Where it lives |
|---|---|---|
| **Design tokens** | CSS custom properties defining a tenant's visual identity | `theme.json` per tenant, injected at Worker cold-start |
| **Capability tokens** | Scoped, signed grants (read:dataset:foo, write:tenant:*, admin) | JWTs signed by tenant's `INGEST_KEY`; verified in Worker |
| **Provenance tokens** | Content-addressed hashes of every dataset row / chunk | Already emitted by Piggybank; surface as first-class IDs |
| **Compression tokens** | Piggybank's content-addressed compressed blobs | R2 objects keyed by hash; deduped across sessions |
| **Metering tokens** | Billable units (ingest bytes, egress bytes, seat-days) | Emitted per request; aggregated into invoices |

**The unifier:** all five are `{namespace, hash, sig?, ttl?}` records. Same encoder, same verifier, same audit trail.

---

## Design Token Schema (v0)

```json
{
  "$schema": "https://axelrod.network/schema/theme.v0.json",
  "name": "axelrod-dark",
  "tokens": {
    "color.bg":       "#0d1117",
    "color.card":     "#161b22",
    "color.border":   "#30363d",
    "color.text":     "#e6edf3",
    "color.muted":    "#8b949e",
    "color.accent":   "#58a6ff",
    "color.success":  "#3fb950",
    "color.danger":   "#f85149",
    "color.harvest":  "#d29922",
    "radius.sm":      "4px",
    "radius.md":      "8px",
    "radius.lg":      "12px",
    "space.1":        "4px",
    "space.2":        "8px",
    "space.3":        "16px",
    "space.4":        "24px",
    "space.5":        "32px",
    "font.body":      "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif",
    "font.mono":      "'SF Mono', Menlo, Consolas, monospace",
    "nav.height":     "56px",
    "brand.name":     "Axelrod",
    "brand.tagline":  "Private AI Data Platform"
  }
}
```

**Example alt-theme (IMI-brand, light):**

```json
{
  "name": "imi-light",
  "tokens": {
    "color.bg":      "#ffffff",
    "color.card":    "#f6f8fa",
    "color.border":  "#d0d7de",
    "color.text":    "#1f2328",
    "color.muted":   "#656d76",
    "color.accent":  "#0969da",
    "color.success": "#1a7f37",
    "color.danger":  "#cf222e",
    "color.harvest": "#9a6700",
    "brand.name":    "IMI Data",
    "brand.tagline": "Sovereign Research Infrastructure"
  }
}
```

Same Worker, same code, different token pack → different platform.

---

## Near-Term Landing Plan (this quarter)

**1. `workers/axelrod-ingest/src/frontend.ts` — extract inline CSS to a `themeToVars(theme)` helper.**
Replace the hardcoded `:root { --bg: #0d1117; ... }` block with `${themeToVars(theme)}`. The function walks the token JSON and emits `--color-bg: #0d1117;` etc. Roughly 20 lines of change.

**2. `workers/axelrod-ingest/src/index.ts` — thread `env.THEME` (a KV or R2 pointer) into `renderFrontend(theme)`.**
Default to `axelrod-dark` if unset. Fetch once at cold-start, cache in module scope.

**3. `workers/axelrod-ingest/wrangler.toml` — add `[vars] THEME_URL = "https://axelrod.network/themes/axelrod-dark.json"`.**
Per-tenant deploys override this to point at their own theme JSON in their R2 bucket.

**4. `workers/axelrod-ingest/src/themes/` — ship `axelrod-dark.json` + `imi-light.json` in-repo.**
Themes served as static assets from the Worker itself for zero-config bootstrapping.

**5. `crates/axelrod-cli/` — add `axelrod theme apply <file.json>` command.**
Operators can push a theme without a redeploy. Themes are just tokens; tokens are just R2 objects.

---

## Capability Tokens — The Second Wave

Once design tokens land, replace the single `INGEST_KEY` with **JWTs signed by a tenant root key**:

```
scope: read:dataset:sessions_2026_q3
sub:   ferrisbueller@imi.com
exp:   2026-09-01T00:00:00Z
```

- **Viewer role** = read-only capability + stripped UI (driven by the same design-token bundle omitting mutation controls).
- **Contributor** = read + write on specific datasets.
- **Admin** = admin:tenant, sees the theme editor and can rotate keys.

Team Access Control (roadmap Q4) ships as **~200 LOC in the Worker**, not a new service, because we already have the primitive.

---

## Provenance + Compression = The Moat

Piggybank already content-addresses every compressed context chunk. Every ingested NDJSON row can be hashed the same way. This means:

- Datasets have **stable, citable identifiers** (`ax://tenant/dataset/rowhash`) that survive re-ingest, re-partition, re-format.
- Dedup happens automatically — the same row uploaded twice occupies one R2 object.
- The **Model-Dataset Lineage Graph** (long-term roadmap) is a walk over the content hashes. It's not a separate database — it falls out of the storage layer.

---

## The Commercial Angle

Metering tokens turn Axelrod's pricing into a story every enterprise buyer already understands: **"You pay per token, not per seat."**

- Ingest: $X per million ingest-tokens
- Storage: $Y per GB-month
- Egress: **$0** (R2 advantage — pass through)
- Seat/theme customization: flat platform fee

Every category above (design, capability, provenance, compression) is *free* to the buyer — bundled. What's metered is *movement*. This aligns Axelrod's revenue with the exact thing that scales with client value (more agent sessions → more ingest → more training data → better private models → deeper lock-in).

---

## The Big Bet

**A token registry, per tenant, as the platform's system-of-record.**

One R2 prefix per tenant (`ax://tenant/tokens/`) holds *every* token type: themes, capabilities, dataset hashes, compression blobs, metering ledgers. Every Worker request reads and writes this one namespace. Every audit query is a walk of this one prefix. Every backup is one R2 sync. Every GDPR "delete-my-data" is one prefix drop.

**Why this compounds:** competitors will build five separate subsystems (theming, IAM, provenance, storage, billing). Axelrod ships one primitive that does all five. Ten engineers can maintain what would otherwise need a fifty-person platform team.

That's the moat. Not a feature — an **architectural insight** that makes every future feature cheaper to ship than the last.

---

*This is the token thesis. Everything downstream is compounding.*
