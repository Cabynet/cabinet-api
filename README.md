# Cabinet API (Rust + Axum Modular Monolith)

High-performance, resource-efficient backend for the **Cabinet** bookmark, document, and note management platform. Designed specifically to run alongside PostgreSQL 17 and Redis 7 within a **2 GB RAM VPS**, while serving Web, Mobile (Expo), Desktop (Tauri), and Browser Extension clients concurrently.

---

## Key Architectural Principles (PRD Compliance)

1. **Modular Monolith in Rust + Axum:**
   - Single lightweight binary handling HTTP API routes, background workers, and telemetry.
   - Production RSS memory footprint: **~11 MB RAM** (extremely safe within 2 GB VPS limits).
2. **Dual Auth Strategy:**
   - Supports both `HttpOnly, Secure, SameSite=Lax` Cookie (for Web App) and `Authorization: Bearer <session_token>` (for Browser Extension, Mobile, and Desktop).
   - Password hashing with **Argon2id**.
   - Session tokens hashed with **SHA-256** prior to database persistence.
3. **Save-First, Enrich-Later (Write Path):**
   - Bookmarks return `201 Created` in `< 25ms` immediately with `metadata_status = 'PENDING'`.
   - Idempotent retries using `client_request_id` (UUID) prevents duplicate items during offline sync.
   - Background worker enriches title, description, favicon, and thumbnail using **"Fill-Missing, Never Overwrite User Intent"** logic.
4. **PostgreSQL 17 Database & Indexing:**
   - Primary Keys use time-ordered **UUIDv7**.
   - **Keyset (Cursor) Pagination** `(is_pinned, created_at, id)` eliminating slow `OFFSET` scans.
   - Zero `note_document` overfetching on list feeds.
   - URL normalization and SHA-256 `url_hash` for $O(1)$ duplicate lookup.
5. **PostgreSQL Full-Text Search + Trigram Typo Tolerance:**
   - Weighted `search_vector` (`A`: title, `B`: domain/url, `C`: description/notes) automatically populated via database trigger.
   - Combined with `pg_trgm` `word_similarity` for typo tolerance.
6. **Hardened Background Worker:**
   - Dual-queue in Redis (`queue:interactive` vs `queue:bulk`) with fairness per user.
   - **SSRF Protection:** DNS validation blocking private (RFC 1918), loopback, link-local, and cloud metadata (`169.254.169.254`) IP addresses on every redirect hop.
   - Streaming HTML parsing with **`lol_html`** with a hard limit of 1 MB that aborts immediately upon encountering `</head>`.
7. **Cloudflare R2 Direct Uploads & WebP Thumbnails:**
   - Presigned PUT URLs for direct client-to-R2 upload (zero binary pass-through on the API).
   - Storage quota validation per user.
   - Background image downscaling to 640px WebP strictly bounded by a Tokio Semaphore.
8. **Public Folder Sharing (`/share/:token`):**
   - Read-only public collections.
   - Strictly excludes notes, trash, and archived items for complete privacy.

---

## Getting Started

### Prerequisites
- Rust 1.80+ (`rustc --version`)
- PostgreSQL 17 with `pg_trgm` extension
- Redis 7

### Configuration
Copy `.env.example` to `.env`:
```sh
cp .env.example .env
```

### Running Locally
```sh
cargo run
```

### Running the Test Suite
```sh
cargo test
```
The suite includes full integration and E2E tests:
- `auth_integration_test`: Cookie & Bearer Dual Auth validation.
- `bookmarks_crud_test`: Idempotency, folder tree, tags, and Keyset Cursor pagination.
- `notes_conflict_test`: Optimistic concurrency control and HTTP 409 conflict detection.
- `worker_enrichment_test`: SSRF guard, streaming parser, and queue priority.
- `media_and_thumbnail_test`: Quota validation, presign, confirm, and WebP compression.
- `search_and_share_test`: FTS, trigram typo matching, and public folder share privacy.
- `e2e_smoke_test`: Complete end-to-end lifecycle.

### Exporting OpenAPI Specification
```sh
cargo run --bin cabinet-api -- --export-openapi
```
Generates `openapi.json` for client SDK generation.

### Production Build & Docker
```sh
docker build -t cabinet-api:latest .
```
Multi-stage build outputs a minimal runtime container (~45 MB) running as a non-root user.
