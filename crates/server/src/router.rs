use axum::body::Body;
use axum::error_handling::HandleErrorLayer;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{middleware, Router};
use rust_embed::Embed;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::handlers;
use crate::middleware::{auth_layer, require_permission, resolve_role_layer};
use crate::ws;
use crate::NodeState;
use prism_core::rbac::Permission;

#[derive(Embed)]
#[folder = "../../dashboard/dist"]
struct DashboardAssets;

/// Build the main Axum router for the PRISM node HTTP API.
pub fn build_router(state: Arc<NodeState>) -> Router {
    // ── Rate limits ───────────────────────────────────────────────
    // HandleErrorLayer maps buffer/rate-limit errors → 429 so axum's Infallible requirement is met.
    let session_rate = ServiceBuilder::new()
        .layer(HandleErrorLayer::new(|_: tower::BoxError| async {
            (StatusCode::TOO_MANY_REQUESTS, "Too many requests — try again shortly.")
        }))
        .buffer(64)
        .rate_limit(10, std::time::Duration::from_secs(1));
    let api_rate = ServiceBuilder::new()
        .layer(HandleErrorLayer::new(|_: tower::BoxError| async {
            (StatusCode::TOO_MANY_REQUESTS, "Too many requests — try again shortly.")
        }))
        .buffer(256)
        .rate_limit(100, std::time::Duration::from_secs(1));

    // ── Public routes (no auth) ─────────────────────────────────────
    let public = Router::new()
        .route("/api/health", get(handlers::node::health_check))
        .route("/healthz", get(handlers::node::health_check))
        .route(
            "/api/sessions",
            post(handlers::sessions::create_session).layer(session_rate),
        );

    // ── Auth layer stack (applied to all non-public API routes) ─────
    // Order: auth_layer extracts token → resolve_role_layer looks up RBAC DB
    let auth_stack = middleware::from_fn_with_state(state.clone(), auth_layer);
    let role_stack = middleware::from_fn_with_state(state.clone(), resolve_role_layer);

    // ── Read-only routes (auth required, ViewDashboard permission) ──
    let read_routes = Router::new()
        .route("/api/v1/node", get(handlers::node::get_node_info))
        .route("/api/data/sources", get(handlers::data::list_sources))
        .route("/api/mesh/nodes", get(handlers::mesh::list_nodes))
        .route(
            "/api/mesh/subscriptions",
            get(handlers::mesh::list_subscriptions),
        )
        .route("/api/tools", get(handlers::tools::list_tools))
        .layer(middleware::from_fn(require_permission(
            Permission::ViewDashboard,
        )));

    // ── Query routes (auth required, QueryData permission) ──────────
    let query_routes = Router::new()
        .route("/api/query", post(handlers::query::execute_query))
        .layer(middleware::from_fn(require_permission(
            Permission::QueryData,
        )));

    // ── Mesh write routes (auth required, IngestData — publishing is a form of data sharing) ──
    let mesh_write_routes = Router::new()
        .route("/api/mesh/publish", post(handlers::mesh::publish_dataset))
        .route(
            "/api/mesh/subscribe",
            post(handlers::mesh::subscribe_dataset),
        )
        .route(
            "/api/mesh/subscribe",
            delete(handlers::mesh::unsubscribe_dataset),
        )
        .layer(middleware::from_fn(require_permission(
            Permission::IngestData,
        )));

    // ── Data ingest (auth required, IngestData permission) ──────────
    let ingest_routes = Router::new()
        .route("/api/data/ingest", post(handlers::data::ingest))
        .layer(middleware::from_fn(require_permission(
            Permission::IngestData,
        )));

    // ── Tool execution (auth required, ExecuteTools permission) ─────
    let tool_exec_routes = Router::new()
        .route("/api/tools/{name}/run", post(handlers::tools::run_tool))
        .layer(middleware::from_fn(require_permission(
            Permission::ExecuteTools,
        )));

    // ── User management (auth required, ManageUsers permission) ─────
    let user_routes = Router::new()
        .route("/api/users", get(handlers::users::list_users))
        .route("/api/users", post(handlers::users::create_user))
        .layer(middleware::from_fn(require_permission(
            Permission::ManageUsers,
        )));

    // ── Audit log (auth required, ViewAudit permission) ─────────────
    let audit_routes = Router::new()
        .route("/api/audit", get(handlers::audit::list_audit_log))
        .layer(middleware::from_fn(require_permission(
            Permission::ViewAudit,
        )));

    // ── Session management (auth required, no permission gate) ──────
    let session_routes = Router::new()
        .route("/api/sessions", delete(handlers::sessions::destroy_session));

    // ── Authenticated API (all permission-gated routes) ─────────────
    let authenticated_api = Router::new()
        .merge(read_routes)
        .merge(query_routes)
        .merge(mesh_write_routes)
        .merge(ingest_routes)
        .merge(tool_exec_routes)
        .merge(user_routes)
        .merge(audit_routes)
        .merge(session_routes)
        .layer(api_rate)
        .layer(role_stack)
        .layer(auth_stack);

    // ── WebSocket (auth via query param — token required) ───────────
    let ws_route = Router::new().route("/ws", get(ws::ws_upgrade));

    // ── Dashboard SPA (embedded assets, no auth) ────────────────────
    let dashboard = Router::new().fallback(get(serve_dashboard));

    // ── CORS — restricted to localhost origins ──────────────────────
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:7327".parse::<HeaderValue>().unwrap(),
            "http://127.0.0.1:7327".parse::<HeaderValue>().unwrap(),
            "http://localhost:5173".parse::<HeaderValue>().unwrap(), // Vite dev server
        ])
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    // ── Assemble ────────────────────────────────────────────────────
    Router::new()
        .merge(public)
        .merge(authenticated_api)
        .merge(ws_route)
        .merge(dashboard)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}

async fn serve_dashboard(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try the exact path first, then fall back to index.html (SPA routing).
    let mut builder = if let Some(file) = DashboardAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mut b = Response::builder().header(header::CONTENT_TYPE, mime.as_ref());
        // Cache immutable hashed assets aggressively
        if path.starts_with("assets/") {
            b = b.header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
        }
        b.body(Body::from(file.data.to_vec())).unwrap()
    } else if let Some(index) = DashboardAssets::get("index.html") {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::from(index.data.to_vec()))
            .unwrap()
    } else {
        return (StatusCode::NOT_FOUND, "dashboard not found").into_response();
    };

    // Security headers on all dashboard responses
    let headers = builder.headers_mut();
    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("X-Frame-Options", "DENY".parse().unwrap());
    headers.insert(
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://localhost:7327"
            .parse()
            .unwrap(),
    );
    headers.insert("Referrer-Policy", "strict-origin-when-cross-origin".parse().unwrap());

    builder
}
