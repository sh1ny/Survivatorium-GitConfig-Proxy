use axum::{
    extract::{ConnectInfo, Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Parser;
use reqwest::Client;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

// ============================================================================
// CLI arguments
// ============================================================================

#[derive(Parser)]
#[command(name = "svc-gitconfig-proxy")]
#[command(version)]
#[command(about = "Lightweight GitHub API proxy for DayZ Survivatorium-GitConfig")]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "8470")]
    port: u16,

    /// Bind address (use 0.0.0.0 to allow external connections)
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    /// GitHub Personal Access Token (overrides token from query params)
    #[arg(long, env = "GITHUB_TOKEN")]
    token: Option<String>,

    /// DayZ server profile path (enables /write for $profile: files)
    #[arg(long, env = "SVC_PROFILE_PATH")]
    profile_path: Option<String>,

    /// DayZ mission path (enables /write for $mission: files)
    #[arg(long, env = "SVC_MISSION_PATH")]
    mission_path: Option<String>,

    /// HTTP request timeout in seconds for GitHub downloads
    #[arg(long, default_value = "300")]
    timeout: u64,

    /// Allowed client IP addresses (comma-separated). Use 0.0.0.0 to allow all.
    #[arg(long, default_value = "127.0.0.1", value_delimiter = ',', env = "SVC_ALLOWED_IPS")]
    allowed_ips: Vec<String>,

    /// Path to TLS certificate file (PEM format). Enables HTTPS when set with --tls-key.
    #[arg(long, env = "SVC_TLS_CERT")]
    tls_cert: Option<String>,

    /// Path to TLS private key file (PEM format). Enables HTTPS when set with --tls-cert.
    #[arg(long, env = "SVC_TLS_KEY")]
    tls_key: Option<String>,
}

// ============================================================================
// Shared state
// ============================================================================

#[derive(Clone)]
struct AppState {
    client: Client,
    default_token: Option<String>,
    profile_path: Option<PathBuf>,
    mission_path: Option<PathBuf>,
    allowed_ips: Vec<IpAddr>,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let client = Client::builder()
        .user_agent("Survivatorium-GitConfig-Proxy/0.1")
        .timeout(Duration::from_secs(args.timeout))
        .build()
        .expect("Failed to create HTTP client");

    let profile_path = args.profile_path.as_ref().map(|p| PathBuf::from(p));
    let mission_path = args.mission_path.as_ref().map(|p| PathBuf::from(p));

    let allowed_ips: Vec<IpAddr> = args
        .allowed_ips
        .iter()
        .map(|s| {
            s.trim()
                .parse::<IpAddr>()
                .unwrap_or_else(|_| panic!("Invalid IP address: '{}'", s))
        })
        .collect();

    let state = AppState {
        client,
        default_token: args.token.clone(),
        profile_path: profile_path.clone(),
        mission_path: mission_path.clone(),
        allowed_ips: allowed_ips.clone(),
    };

    let app = Router::new()
        .route("/tree", get(handle_tree))
        .route("/raw", get(handle_raw))
        .route("/write", get(handle_write))
        .route("/health", get(|| async { "OK" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ip_allowlist,
        ))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .expect("Invalid bind address");

    info!("=== Survivatorium GitConfig Proxy v{} ===", env!("CARGO_PKG_VERSION"));

    let tls_enabled = match (&args.tls_cert, &args.tls_key) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        _ => panic!("Both --tls-cert and --tls-key must be provided together"),
    };

    let scheme = if tls_enabled { "https" } else { "http" };
    info!("Listening on {}://{}", scheme, addr);

    if args.token.is_some() {
        info!("Using GitHub token from --token / GITHUB_TOKEN env var");
    } else {
        warn!("No default token. Expecting token in query parameters from the DayZ mod.");
    }

    if let Some(ref p) = profile_path {
        info!("Profile path: {}", p.display());
    }
    if let Some(ref p) = mission_path {
        info!("Mission path: {}", p.display());
    }
    if profile_path.is_none() && mission_path.is_none() {
        warn!("/write endpoint disabled — no --profile-path or --mission-path configured.");
    }

    let allow_all = allowed_ips.iter().any(|ip| ip.is_unspecified());
    if allow_all {
        warn!("IP allowlist: OPEN — all client IPs are permitted (0.0.0.0 in list)");
    } else {
        info!("Allowed client IPs: {:?}", allowed_ips);
    }

    if tls_enabled {
        let cert_path = args.tls_cert.as_ref().unwrap();
        let key_path = args.tls_key.as_ref().unwrap();

        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
            .await
            .expect("Failed to load TLS certificate/key");

        info!("TLS enabled with cert: {}", cert_path);

        axum_server::bind_rustls(addr, config)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    } else {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind address");

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    }
}

// ============================================================================
// Middleware — IP allowlist
// ============================================================================

async fn ip_allowlist(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    let client_ip = addr.ip();

    // 0.0.0.0 or :: (unspecified) in the list means "allow all"
    let allow_all = state.allowed_ips.iter().any(|ip| ip.is_unspecified());

    if !allow_all && !state.allowed_ips.contains(&client_ip) {
        warn!("Blocked request from {} — not in allowlist", client_ip);
        return Err((
            StatusCode::FORBIDDEN,
            format!("IP {} not in allowlist", client_ip),
        ));
    }

    Ok(next.run(request).await)
}

// ============================================================================
// Token resolution: env/CLI token takes priority, then query param
// ============================================================================

fn resolve_token(
    state: &AppState,
    params: &HashMap<String, String>,
) -> Result<String, (StatusCode, String)> {
    if let Some(ref t) = state.default_token {
        if !t.is_empty() {
            return Ok(t.clone());
        }
    }
    if let Some(t) = params.get("token") {
        if !t.is_empty() {
            return Ok(t.clone());
        }
    }
    Err((
        StatusCode::BAD_REQUEST,
        "No GitHub token. Set GITHUB_TOKEN env var or pass ?token= parameter.".to_string(),
    ))
}

// ============================================================================
// Input validation — prevent SSRF / injection
// ============================================================================

fn validate_segment(value: &str, name: &str) -> Result<(), (StatusCode, String)> {
    if value.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Missing required parameter: {}", name),
        ));
    }
    if value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.contains('?')
        || value.contains('&')
        || value.contains('#')
        || value.contains(' ')
        || value.contains('@')
        || value.contains(':')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid characters in parameter: {}", name),
        ));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), (StatusCode, String)> {
    if path.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Missing required parameter: path".to_string(),
        ));
    }
    if path.contains("..") {
        return Err((
            StatusCode::BAD_REQUEST,
            "Path traversal not allowed".to_string(),
        ));
    }
    if path.contains('\\') || path.starts_with('/') {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid path format".to_string(),
        ));
    }
    Ok(())
}

/// Percent-encode individual path segments while preserving `/`
fn encode_path_for_url(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            let mut encoded = String::with_capacity(segment.len());
            for ch in segment.chars() {
                match ch {
                    ' ' => encoded.push_str("%20"),
                    '(' => encoded.push_str("%28"),
                    ')' => encoded.push_str("%29"),
                    '[' => encoded.push_str("%5B"),
                    ']' => encoded.push_str("%5D"),
                    '{' => encoded.push_str("%7B"),
                    '}' => encoded.push_str("%7D"),
                    '#' => encoded.push_str("%23"),
                    '?' => encoded.push_str("%3F"),
                    '+' => encoded.push_str("%2B"),
                    '%' => encoded.push_str("%25"),
                    _ => encoded.push(ch),
                }
            }
            encoded
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ============================================================================
// GET /tree — Fetch GitHub repository file tree
// ============================================================================

async fn handle_tree(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = resolve_token(&state, &params)?;

    let owner = params.get("owner").map(|s| s.as_str()).unwrap_or("");
    let repo = params.get("repo").map(|s| s.as_str()).unwrap_or("");
    let branch = params.get("branch").map(|s| s.as_str()).unwrap_or("main");

    validate_segment(owner, "owner")?;
    validate_segment(repo, "repo")?;
    validate_segment(branch, "branch")?;

    let url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        owner, repo, branch
    );

    info!("Tree: {}/{} branch={}", owner, repo, branch);

    match state
        .client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                warn!("GitHub {} for tree: {}", status, &body[..body.len().min(200)]);
            } else {
                info!("Tree OK: {} bytes", body.len());
            }
            Ok::<_, (StatusCode, String)>((
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                body,
            ))
        }
        Err(e) => {
            error!("Failed to reach GitHub: {}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Proxy error: {}", e),
            ))
        }
    }
}

// ============================================================================
// GET /raw — Fetch raw file content from GitHub
// ============================================================================

async fn handle_raw(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = resolve_token(&state, &params)?;

    let owner = params.get("owner").map(|s| s.as_str()).unwrap_or("");
    let repo = params.get("repo").map(|s| s.as_str()).unwrap_or("");
    let branch = params.get("branch").map(|s| s.as_str()).unwrap_or("main");
    let path = params.get("path").map(|s| s.as_str()).unwrap_or("");

    validate_segment(owner, "owner")?;
    validate_segment(repo, "repo")?;
    validate_segment(branch, "branch")?;
    validate_path(path)?;

    let encoded_path = encode_path_for_url(path);
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        owner, repo, branch, encoded_path
    );

    info!("Raw: {}", path);

    match state
        .client
        .get(&url)
        .header("Authorization", format!("token {}", token))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                warn!("GitHub {} for raw {}: {}", status, path, &body[..body.len().min(100)]);
            } else {
                info!("Raw OK: {} ({} bytes)", path, body.len());
            }
            Ok::<_, (StatusCode, String)>((
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                body,
            ))
        }
        Err(e) => {
            error!("Failed to reach GitHub: {}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Proxy error: {}", e),
            ))
        }
    }
}

// ============================================================================
// GET /write — Download from GitHub and write directly to local disk
// Used for large/binary files that would timeout or corrupt via DayZ RestContext
// ============================================================================

async fn handle_write(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = resolve_token(&state, &params)?;

    let owner = params.get("owner").map(|s| s.as_str()).unwrap_or("");
    let repo = params.get("repo").map(|s| s.as_str()).unwrap_or("");
    let branch = params.get("branch").map(|s| s.as_str()).unwrap_or("main");
    let path = params.get("path").map(|s| s.as_str()).unwrap_or("");
    let dest = params.get("dest").map(|s| s.as_str()).unwrap_or("");
    let localpath = params.get("localpath").map(|s| s.as_str()).unwrap_or("");

    validate_segment(owner, "owner")?;
    validate_segment(repo, "repo")?;
    validate_segment(branch, "branch")?;
    validate_path(path)?;
    validate_path(localpath)?;

    // Resolve destination base path
    let base = match dest {
        "profile" => state.profile_path.as_ref(),
        "mission" => state.mission_path.as_ref(),
        _ => None,
    };
    let base = base.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!(
                "Unknown or unconfigured dest '{}'. Start proxy with --profile-path / --mission-path.",
                dest
            ),
        )
    })?;

    // Resolve target path and verify it stays under the base directory
    let target = base.join(localpath);
    let canonical_base = base.canonicalize().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Base path does not exist: {}", e),
        )
    })?;

    // Create parent dirs so we can canonicalize the parent
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create directories: {}", e),
            )
        })?;
    }

    // Canonicalize parent and check containment (target file may not exist yet)
    let parent_canonical = target
        .parent()
        .unwrap_or(&target)
        .canonicalize()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot resolve target path: {}", e),
            )
        })?;
    let file_name = target
        .file_name()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Invalid localpath".to_string()))?;
    let resolved_target = parent_canonical.join(file_name);

    if !resolved_target.starts_with(&canonical_base) {
        return Err((
            StatusCode::FORBIDDEN,
            "Resolved path escapes base directory".to_string(),
        ));
    }

    // Download raw bytes from GitHub
    let encoded_path = encode_path_for_url(path);
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        owner, repo, branch, encoded_path
    );

    info!("Write: {} -> {}", path, resolved_target.display());

    let resp = state
        .client
        .get(&url)
        .header("Authorization", format!("token {}", token))
        .send()
        .await
        .map_err(|e| {
            error!("GitHub download failed: {}", e);
            (StatusCode::BAD_GATEWAY, format!("Download failed: {}", e))
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            "GitHub {} for write {}: {}",
            status,
            path,
            &body[..body.len().min(200)]
        );
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("GitHub {}: {}", status, &body[..body.len().min(200)]),
        ));
    }

    // Stream response body directly to disk (constant memory usage)
    let mut file = tokio::fs::File::create(&resolved_target)
        .await
        .map_err(|e| {
            error!("Failed to create {}: {}", resolved_target.display(), e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Create file failed: {}", e),
            )
        })?;

    let mut byte_count: u64 = 0;
    let mut stream = resp.bytes_stream();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            error!("Stream error downloading {}: {}", path, e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Download stream error: {}", e),
            )
        })?;
        file.write_all(&chunk).await.map_err(|e| {
            error!("Failed to write chunk to {}: {}", resolved_target.display(), e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Write failed: {}", e),
            )
        })?;
        byte_count += chunk.len() as u64;
    }

    file.flush().await.map_err(|e| {
        error!("Failed to flush {}: {}", resolved_target.display(), e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Flush failed: {}", e),
        )
    })?;

    info!("Write OK: {} ({} bytes)", path, byte_count);

    Ok::<_, (StatusCode, String)>((
        StatusCode::OK,
        format!("{{\"ok\":true,\"bytes\":{}}}", byte_count),
    ))
}
