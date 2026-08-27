# Data Platform in a Box

### Private, Self-Hosted AI Dataset Infrastructure — Deployed in Minutes

---

## The Problem

Every AI team hits the same wall: training data is scattered across S3 buckets, shared drives, and Slack threads. HuggingFace Hub works for open models — but enterprise clients need **private, tenant-isolated dataset infrastructure** they actually control.

There's no lightweight, self-hosted alternative that gives you HuggingFace-style dataset operations (push, pull, preview, stats) on your own infrastructure, with zero vendor lock-in.

**Until now.**

---

## What Axelrod Is

Axelrod is a complete dataset platform that deploys as a single Cloudflare Worker backed by R2 object storage. One config file. One deploy command. Full dataset lifecycle management.

Think of it as **HuggingFace Hub, but private, per-tenant, and running on edge infrastructure you control.**

Each client gets their own isolated deployment — their own domain, their own storage bucket, their own API key. Same platform code, zero cross-contamination.

---

## Architecture

Axelrod is a cloud-native data collection and dataset management platform built on Cloudflare Workers and R2 object storage. It captures structured telemetry from AI agent sessions and stores it as timestamped NDJSON files, while also serving as a general-purpose dataset management API for any structured data.

### API (Cloudflare Worker)

| Route | Method | What it does |
|---|---|---|
| `/` | GET | Health check |
| `/ingest` | POST | Live telemetry ingestion — validates, partitions by date/session, stores to R2 |
| `/datasets` | GET | List all datasets with size, type, and last-updated metadata |
| `/datasets/{name}` | GET | Dataset metadata — row count, schema, chunk count, date range |
| `/datasets/{name}` | DELETE | Remove a dataset and all its backing objects |
| `/datasets/{name}/download` | GET | Stream full dataset as NDJSON (supports `?date=` filter) |
| `/datasets/{name}/sample` | GET | Preview N records without downloading everything |
| `/datasets/{name}/upload` | POST | Append NDJSON chunk, auto-infer schema, update metadata |
| `/datasets/{name}/stats` | GET | Aggregate stats — rows, bytes, sessions, event distribution, date range |

### CLI (`axelrod`)

| Command | What it does |
|---|---|
| `axelrod login` | Configure API endpoint and key (saved to `~/.axelrod/config.json`) |
| `axelrod datasets` | List all datasets in a formatted table |
| `axelrod pull <name>` | Download full dataset to local NDJSON file with progress |
| `axelrod push <name> <file>` | Upload local NDJSON as new dataset chunk |
| `axelrod preview <name>` | Pretty-print sample records for quick inspection |
| `axelrod stats [name]` | Detailed statistics for one or all datasets |
| `axelrod ingest <file>` | Post raw telemetry directly to the harvest store |

### How Data Flows

```
Agent Session
    ↓
Piggybank compresses context, emits structured events
    ↓
Harvester buffers in memory (batch of 10)
    ↓
POST → axelrod.network/ingest (Bearer auth)
    ↓
Worker validates → R2 storage (harvest/{date}/{session}/{ts}.jsonl)
    ↓
CLI pulls → local analysis / model training
```

---

## Per-Client Deployment Model

This is the key differentiator. Each enterprise client gets a fully isolated instance:

| Component | Per-Client |
|---|---|
| Domain | `data.clientname.com` or `axelrod.network` |
| R2 Bucket | Isolated — `client-harvest` |
| API Key | Unique `INGEST_KEY` per tenant |
| Worker Code | Shared (same codebase, different config) |
| wrangler.toml | Client-specific routes and bindings |

**Deploying a new client takes < 5 minutes:**
1. Create R2 bucket
2. Copy wrangler.toml, set domain route
3. Generate INGEST_KEY
4. `wrangler deploy`

No shared databases. No cross-tenant data leakage. Full GDPR/compliance isolation by architecture, not policy.

---

## What's Live Today

- **9 authenticated API routes** covering the full dataset lifecycle
- **7 CLI commands** for operators and data scientists
- **Streaming downloads** — no buffering, handles datasets of any size
- **Auto-schema inference** on upload
- **Date-partitioned storage** for efficient time-range queries
- **Live telemetry pipeline** — Piggybank agent sessions auto-ingest training data
- **Bearer token auth** on every route
- **R2 cursor-paginated listing** — handles millions of objects

---

## Roadmap — What's Coming

### Near-Term (Shipping This Quarter)

**Dataset Versioning & Snapshots**
Immutable point-in-time snapshots of any dataset. Pin exact versions to training runs. Full audit trail of who changed what and when — critical for reproducibility and compliance.

**Data Flywheel Integration**
Auto-ingest model outputs and human feedback back into datasets. Every inference improves the training corpus. The platform gets smarter with every agent session, creating a compounding data advantage.

**Format-Agnostic Storage**
Native support for Parquet, Arrow, CSV, and binary formats alongside NDJSON. Upload in any format, query in any format. Zero conversion friction for data science teams.

**Team Access Control**
Role-based permissions per dataset — viewer, contributor, admin. SSO integration. Audit logs for every access and mutation. Enterprise compliance out of the box.

### Mid-Term (Next 2 Quarters)

**Privacy-Preserving Compute**
Differential privacy statistics, synthetic data generation, and never-egress guarantees. Run analysis on sensitive data without the data ever leaving the client's R2 bucket. PII detection and auto-redaction on ingest.

**Edge-Native Distribution**
Push dataset shards to Cloudflare's 300+ edge locations for low-latency access. Federated training across distributed fleets. Data lives at the edge, closest to where models consume it.

**Training-Loop Telemetry**
Correlate specific data slices with model performance metrics. Flag which training examples hurt or help. Close the loop between dataset curation and model quality — the missing piece in every ML pipeline.

**Dataset Search & Discovery**
Full-text search across metadata, schemas, and sample content. Tags, categories, and auto-generated descriptions. Find the right dataset in seconds across thousands of entries.

### Long-Term Vision

**Marketplace Mode**
Optional cross-tenant dataset sharing with granular licensing. Clients can publish curated datasets to a private marketplace within their organization — or across partner organizations with controlled access.

**Model-Dataset Lineage Graph**
Full provenance tracking from raw data → curated dataset → training run → deployed model. One click to see exactly which data produced which model version. Regulatory-grade reproducibility.

**Federated Learning Coordinator**
Coordinate model training across multiple client deployments without centralizing data. Each tenant's data stays in their bucket. The platform orchestrates gradient aggregation across the federation.

---

## Why This Matters for IMI

IMI's clients handle proprietary data that cannot touch third-party infrastructure. With Axelrod:

- **Each IMI client** gets their own `data.clientname.com` — fully isolated, fully branded
- **Training data accumulates automatically** from every AI agent session via Piggybank
- **Data scientists** use the CLI to pull, inspect, and curate datasets without cloud console access
- **Compliance teams** get audit trails, tenant isolation, and data residency guarantees by architecture
- **The data flywheel** means every client interaction improves their private models — a compounding moat their competitors can't replicate

This isn't a feature — it's a **platform product** that IMI can offer to every client as a managed service.

---

## The Stack

| Layer | Technology | Why |
|---|---|---|
| API | Cloudflare Workers | Zero cold starts, global edge, 0ms TTFB |
| Storage | Cloudflare R2 | S3-compatible, zero egress fees, per-tenant isolation |
| CLI | Rust | Single binary, no runtime dependencies, cross-platform |
| Ingest | Piggybank Harvester | Buffered background POST, never blocks agent sessions |
| Auth | Bearer token | Simple, stateless, per-tenant keys |
| Wire Format | NDJSON | Streamable, appendable, universally supported |

---

## One-Liner

> **Axelrod is HuggingFace Hub for enterprises that need to own their data pipeline — private, per-tenant, deployed on edge infrastructure in under 5 minutes.**

---

*Built by AXE Technologies. Ready for production deployment.*
