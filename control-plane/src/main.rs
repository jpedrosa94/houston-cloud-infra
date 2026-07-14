//! Houston Cloud control plane.
//!
//! Authenticates users via Supabase JWT, provisions per-tenant engine
//! containers, and returns engine URL + token to the frontend.

mod auth;
mod idle_sweeper;
pub mod k8s;
mod routes;
mod session;
mod tenant;

use axum::{
    http::{header, HeaderValue, Method},
    routing, Router,
};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let supabase_url = std::env::var("SUPABASE_URL").expect("SUPABASE_URL must be set");
    let supabase_service_key =
        std::env::var("SUPABASE_SERVICE_ROLE_KEY").expect("SUPABASE_SERVICE_ROLE_KEY must be set");
    let jwt_secret = std::env::var("SUPABASE_JWT_SECRET").expect("SUPABASE_JWT_SECRET must be set");
    let engine_url_template = std::env::var("ENGINE_URL_TEMPLATE")
        .unwrap_or_else(|_| "http://localhost:7777".to_string());
    let dev_engine_token = std::env::var("DEV_ENGINE_TOKEN").ok();
    let engine_image = std::env::var("ENGINE_IMAGE").ok();
    let gateway_domain = std::env::var("GATEWAY_DOMAIN").ok();
    let gateway_scheme = gateway_scheme();
    let gvisor = std::env::var("GVISOR")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let storage_class = std::env::var("STORAGE_CLASS").unwrap_or_else(|_| "standard".to_string());
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:3001".to_string());

    let state = Arc::new(routes::AppState {
        tenant_store: tenant::TenantStore::new(&supabase_url, &supabase_service_key),
        jwt_verifier: auth::JwtVerifier::new(&supabase_url, &jwt_secret),
        engine_url_template,
        dev_engine_token,
        engine_image: engine_image.clone(),
        gateway_domain,
        gateway_scheme,
        gvisor,
        storage_class,
    });

    let app = Router::new()
        .route("/api/session", routing::post(routes::create_session))
        .route("/api/tenant/status", routing::get(routes::tenant_status))
        .route("/api/tenant/suspend", routing::post(routes::suspend_tenant))
        .route("/api/tenant", routing::delete(routes::delete_tenant))
        .route("/ext-authz", routing::get(routes::ext_authz))
        .layer(cors_layer())
        .with_state(state);

    // Spawn idle tenant sweeper (auto-suspend after 1h inactivity).
    idle_sweeper::spawn(
        tenant::TenantStore::new(&supabase_url, &supabase_service_key),
        engine_image.is_some(),
    );

    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
    tracing::info!("houston-cloud-api listening on {bind}");
    axum::serve(listener, app).await.unwrap();
}

fn gateway_scheme() -> String {
    match std::env::var("GATEWAY_SCHEME")
        .unwrap_or_else(|_| "https".to_string())
        .as_str()
    {
        "http" => "http".to_string(),
        "https" => "https".to_string(),
        other => {
            tracing::warn!("invalid GATEWAY_SCHEME={other}; defaulting to https");
            "https".to_string()
        }
    }
}

fn cors_layer() -> CorsLayer {
    let methods = [Method::GET, Method::POST, Method::DELETE, Method::OPTIONS];
    let headers = [header::AUTHORIZATION, header::CONTENT_TYPE];

    if let Ok(raw_origins) = std::env::var("CORS_ALLOWED_ORIGINS") {
        let origins: Vec<HeaderValue> = raw_origins
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .filter_map(|origin| match origin.parse::<HeaderValue>() {
                Ok(origin) => Some(origin),
                Err(e) => {
                    tracing::warn!("ignoring invalid CORS origin {origin:?}: {e}");
                    None
                }
            })
            .collect();

        if !origins.is_empty() {
            return CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(methods)
                .allow_headers(headers);
        }
    }

    tracing::warn!("CORS_ALLOWED_ORIGINS is not set; browser CORS access is disabled");
    CorsLayer::new()
        .allow_methods(methods)
        .allow_headers(headers)
}
