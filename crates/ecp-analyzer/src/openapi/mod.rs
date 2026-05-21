//! OpenAPI 3.x / Swagger 2.0 schema-field detector — T4-6.
//!
//! Lifts `components.schemas.<Name>.properties.<field>` (OpenAPI 3.x) and
//! `definitions.<Name>.properties.<field>` (Swagger 2.0) into `SchemaField`
//! nodes.
//!
//! **Out of scope (v1):** inline schemas under `paths.*` (request bodies,
//! response content, parameter schemas).  A future `--include-inline` flag
//! should cover those.  TODO: implement `--include-inline` to scan
//! `paths.<path>.<method>.{requestBody,responses,parameters}` schema trees.

pub mod schema_scan;

pub use schema_scan::OpenApiProvider;
