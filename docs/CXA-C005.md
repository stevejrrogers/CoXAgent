FOLDER: Engineering

# OpenAPI Specification Generation

**Keywords:** OpenAPI 3, openapi.json, spec generation, ROUTES table, drift guard, SDK discovery, bearerAuth security scheme, operationId, autotag, autosummary

## Overview

CXA-C005 enriches the hub self-describing API contract. GET /api/openapi.json serves an OpenAPI 3.x document in which every service route carries a unique operationId, a feature-area tags group, a human-readable summary, path parameters derived from colon-segments, and a security requirement matched to what auth actually enforces at runtime. It exists so MCP clients and SDK generators can discover endpoints and wire authenticated callers without reading Rust source (built on CXA-F023 / CXA-B051). It is for anyone adding or renaming an api service route under crates/presentation/src/server/mod.rs.

## How it works
Axum 0.7 exposes no practical runtime route introspection, so this module keeps ONE structured table named ROUTES in crates/presentation/src/server/openapi.rs, mirroring exactly what serve_full chains onto its Router new. The function build_document is a pure function over that table; nothing here depends on request context.

1. serve_full registers the route "/api/openapi.json" with the get(openapi_ep) handler in crates/presentation/src/server/mod.rs.
2. openapi_ep returns Json(build_document()).
3. build_document walks ROUTES: one Path Item per distinct path (keys sorted via a BTreeMap) and one Operation per supported HTTP verb.
4. Each Operation is built by operation(spec, method): an id from verb plus tail_word, tag and summary derived by autotag and autosummary, path parameters emitted by parameters, and a security tier from security_for.
5. Per-operation security comes from enum SecurityLevel with variants Public, Authenticated and Admin. Public operations declare no requirement (empty array); Authenticated and Admin require a bearer token via the method requirement(). The document declares one bearer scheme under components.securitySchemes.bearerAuth.
6. The drift guard in crates/app/tests/openapi_routes_gate.rs fails CI if any registered service route diverges from ROUTES, in either direction.

The runtime auth ground truth lives in auth_mw (crates/presentation/src/server/auth.rs): public paths pass through with no session; manage surfaces need an Admin or lead role; everything else needs any valid session plus per-project membership where a project id appears in the path.

## Usage
Fetch and inspect the generated contract:

```
curl -s http://127.0.0.1:4000/api/openapi.json | jq '.info.title'
# "CoXAgent Hub API"

curl -s http://127.0.0.1:4000/api/openapi.json | \
  jq '.paths["/api/projects/:pid/tickets"].post | {operationId,tags}'
```

The endpoint is public (same trust tier as health), so no session is needed to read the spec.

Authenticated operations expect a personal access token issued under /api/auth/my/tokens, presented as an HTTP bearer token:

```
curl -s http://127.0.0.1:4000/api/projects \
  -H 'Authorization: Bearer <token>'
```

## Interface
The feature serves exactly one endpoint.

GET /api/openapi.json
- handler openapi_ep returning Json(build_document())
- top-level "openapi": "3.0.3"
- info.title: "CoXAgent Hub API"; info.version: env!("CARGO_PKG_VERSION")
- components.securitySchemes.bearerAuth: http scheme with scheme bearer; description states the token is issued under /api/auth/my/tokens
- paths: a BTreeMap keyed by route path, serialised with sorted keys for stable output

Per Operation object fields (built by operation):
- operationId: snake-case id from HTTP verb plus all static path segments via tail_word, e.g. post_api_projects_pid_tickets; unique per verb so multi-method routes never collide
- tags: single-element array from autotag, the first segment after "/api/" capitalised and with hyphens removed (e.g. Projects)
- summary: autosummary of the last static segment, capitalised (e.g. Tickets); may be overridden per RouteSpec entry
- parameters: array of required string path parameters derived from each colon-named segment in path order via parameters
- security: [] for Public, else [ { "bearerAuth": [] } ], via SecurityLevel requirement()
- responses["200"].description: the summary

Internal types:
- struct RouteSpec in openapi.rs with fields path, methods, tag (Option), summary (Option); built by const fn route(path, methods)
- enum SecurityLevel { Public, Authenticated, Admin }
- pub(super) const ROUTES listing every registered service route

## Configuration
There is no configuration surface for this feature. All behaviour derives from code constants in openapi.rs:

- ROUTES entry overrides: optional tag and summary fields on RouteSpec (none currently set)
- Security tiers hard-coded in security_for: Public = "/api/health", "/api/openapi.json", "/api/auth/login"; Admin = any path starting with "/api/auth/users" or "/api/auth/tokens"; everything else Authenticated

Nothing here reads environment variables or config files. If configurability is added later, this section must be updated.

## Edge cases and limits

The document deliberately does NOT model request bodies or typed response schemas; responses only carry an informative description equal to the summary. Path parameter types are always string because hub routes treat ids as strings and no type inference is attempted.

Limits to know:
- The generated spec describes route shape and security topology but not payload shapes; SDK generators should treat bodies as unmodelled.
- operationId uniqueness is enforced by a unit test (operation_ids_are_unique_across_the_whole_document), so two routes whose tails collapse after stripping colon params would fail CI rather than silently collide.
- Tags and summaries derive only from path segment conventions via autotag and autosummary; entries needing intent beyond naming must set explicit tag/summary on their RouteSpec.
- The document is rebuilt from ROUTES on every request; fine at hub scale (about 180 entries) since it stays well under one kilobyte of work per call.

Failure modes:
- If a live route in mod.rs is forgotten from ROUTES (or an orphan entry remains for a removed route), the drift guard openapi_routes_gate fails CI loudly with guidance to add the missing route or drop the orphan.
- If auth enforcement drifts away from what security_for encodes, the spec would document a wrong tier; keep security_for aligned with auth_mw as ground truth.

## Code map

This documents where each piece lives on branch feat/CXA-C005 (commit 76bf8f8). Note: the current main HEAD still carries the pre-enrichment baseline of openapi.rs without SecurityLevel, tags, or summaries; this feature becomes active once feat/CXA-C005 merges.

- crates/presentation/src/server/openapi.rs - the whole feature: ROUTES table, RouteSpec, SecurityLevel enum with requirement(), build_document(), operation(), security_for(), autotag(), autosummary(), tail_word(), parameters(), endpoint handler openapi_ep(), and unit tests
- crates/presentation/src/server/mod.rs - registers the "/api/openapi.json" route onto serve_full's router chain
- crates/app/tests/openapi_routes_gate.rs - CI drift guard pinning ROUTES equal to live registration in both directions
- crates/app/tests/openapi_endpoint.rs - end-to-end test that a booted hub answers GET /api/openapi.json with 200, an OpenAPI 3.x version marker, title "CoXAgent Hub API", and non-empty paths
- crates/presentation/src/server/auth.rs - auth_mw where public/manage enforcement defines the SecurityLevel tiers security_for mirrors

## Related

Built on CXA-F023 (self-describing API discovery) and CXA-B051 (OpenAPI endpoint end-to-end plus drift guard). This page describes CXA-C005, which refactors the OpenAPI generation in openapi.rs into its richer form with security tiers, tags, and summaries. The workspace repo map lists coxagent codegraph tools for locating these symbols interactively.
