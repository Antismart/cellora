//! OpenAPI specification generation.
//!
//! Every handler is annotated with [`utoipa::path`] and every wire-format
//! type derives [`utoipa::ToSchema`]. The [`ApiDoc`] struct collects them
//! into a single [`utoipa::openapi::OpenApi`] value used for two purposes:
//!
//! 1. The `/v1/openapi.json` endpoint, which serves the spec to clients
//!    and tooling (Postman, Swagger UI, code generators).
//! 2. The committed `docs/openapi.json` file. A drift test in
//!    `tests/api_e2e.rs` compares the live spec byte-for-byte against that
//!    file; a diff fails the test with instructions to regenerate.

use utoipa::OpenApi;

use crate::error::{ErrorBody, ErrorEnvelope};
use crate::routes::blocks::BlockResponse;
use crate::routes::cells::{CellResponse, CellsPage, ConsumedByResponse, PageMeta, ScriptResponse};
use crate::routes::health::{HealthResponse, ReadyResponse};
use crate::routes::stats::StatsResponse;

/// Top-level OpenAPI document for the Cellora REST API.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Cellora REST API",
        description = "Read-only REST API over indexed CKB data.",
        license(name = "Apache-2.0"),
    ),
    paths(
        crate::routes::health::liveness,
        crate::routes::health::readiness,
        crate::routes::blocks::latest,
        crate::routes::blocks::by_number,
        crate::routes::cells::list,
        crate::routes::stats::stats,
    ),
    components(schemas(
        HealthResponse,
        ReadyResponse,
        BlockResponse,
        CellResponse,
        CellsPage,
        ConsumedByResponse,
        PageMeta,
        ScriptResponse,
        StatsResponse,
        ErrorEnvelope,
        ErrorBody,
    )),
    tags(
        (name = "health", description = "Liveness and readiness probes."),
        (name = "blocks", description = "Indexed block lookups."),
        (name = "cells", description = "Paginated cell queries by lock or type hash."),
        (name = "stats", description = "Indexer progress and lag."),
    ),
)]
pub struct ApiDoc;

/// Render the spec as a pretty-printed JSON string. Used both by the
/// `/v1/openapi.json` handler and by the drift test.
pub fn spec_json() -> String {
    // The utoipa builder only fails when schema registration is malformed,
    // which is a programming error and cannot happen at runtime.
    #[allow(clippy::expect_used)]
    ApiDoc::openapi()
        .to_pretty_json()
        .expect("openapi serialisation")
}
