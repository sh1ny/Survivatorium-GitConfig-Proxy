use axum::{
    extract::{ConnectInfo, Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::time::sleep;
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
    /// Number of LFS background downloads currently in flight.
    pending_lfs: Arc<AtomicUsize>,
    /// Cumulative count of LFS downloads that failed (never resets during a run).
    failed_lfs: Arc<AtomicUsize>,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let client = Client::builder()
        .user_agent("Survivatorium-GitConfig-Proxy/0.2")
        .timeout(Duration::from_secs(args.timeout))
        .build()
        .expect("Failed to create HTTP client");

    let profile_path = args.profile_path.as_deref().map(PathBuf::from);
    let mission_path = args.mission_path.as_deref().map(PathBuf::from);

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
        pending_lfs: Arc::new(AtomicUsize::new(0)),
        failed_lfs: Arc::new(AtomicUsize::new(0)),
    };

    // /health is outside the IP allowlist so Docker probes and monitoring can reach it
    let guarded = Router::new()
        .route("/tree", get(handle_tree))
        .route("/raw", get(handle_raw))
        .route("/write", get(handle_write))
        .route("/lfs-status", get(handle_lfs_status))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ip_allowlist,
        ))
        .with_state(state);

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .merge(guarded);

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
    // Block Windows drive letters (e.g. "C:/...") — on Windows, PathBuf::join
    // treats paths with drive letters as absolute and replaces the base entirely
    if path.contains(':') {
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

    let owner = params.get("owner").map_or("", String::as_str);
    let repo = params.get("repo").map_or("", String::as_str);
    let branch = params.get("branch").map_or("main", String::as_str);

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

    let owner = params.get("owner").map_or("", String::as_str);
    let repo = params.get("repo").map_or("", String::as_str);
    let branch = params.get("branch").map_or("main", String::as_str);
    let path = params.get("path").map_or("", String::as_str);

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
// Git LFS support — pointer detection, batch API, streaming helper
// ============================================================================

/// Parsed Git LFS pointer.
struct LfsPointer {
    oid: String, // e.g. "sha256:4dee5767f70b..."
    size: u64,   // e.g. 100663320
}

/// Check if `data` is a Git LFS pointer. Returns parsed fields if so.
fn parse_lfs_pointer(data: &[u8]) -> Option<LfsPointer> {
    // LFS pointers are always small (< 512 bytes) and valid UTF-8
    if data.len() > 512 {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    if !text.starts_with("version https://git-lfs.github.com/spec/v1") {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(val) = line.strip_prefix("oid ") {
            // Strip "sha256:" prefix — LFS Batch API expects bare hex hash
            let hash = val.strip_prefix("sha256:").unwrap_or(val);
            oid = Some(hash.to_string());
        } else if let Some(val) = line.strip_prefix("size ") {
            size = val.parse::<u64>().ok();
        }
    }
    Some(LfsPointer {
        oid: oid?,
        size: size?,
    })
}

// ── LFS Batch API types ─────────────────────────────────────────────────────

#[derive(Serialize)]
struct LfsBatchRequest {
    operation: String,
    transfers: Vec<String>,
    objects: Vec<LfsBatchObject>,
}

#[derive(Serialize, Deserialize)]
struct LfsBatchObject {
    oid: String,
    size: u64,
}

#[derive(Deserialize)]
struct LfsBatchResponse {
    objects: Vec<LfsBatchResponseObject>,
}

#[derive(Deserialize)]
struct LfsBatchResponseObject {
    #[allow(dead_code)]
    oid: String,
    #[allow(dead_code)]
    size: u64,
    actions: Option<LfsBatchActions>,
    error: Option<LfsBatchError>,
}

#[derive(Deserialize)]
struct LfsBatchActions {
    download: LfsBatchAction,
}

#[derive(Deserialize)]
struct LfsBatchAction {
    href: String,
    header: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct LfsBatchError {
    code: u16,
    message: String,
}

/// Resolve an LFS pointer to a download URL via the GitHub LFS Batch API.
/// Returns (download_url, optional_headers).
async fn resolve_lfs(
    client: &Client,
    token: &str,
    owner: &str,
    repo: &str,
    pointer: &LfsPointer,
) -> Result<(String, HashMap<String, String>), (StatusCode, String)> {
    let batch_url = format!(
        "https://github.com/{}/{}.git/info/lfs/objects/batch",
        owner, repo
    );

    let body = LfsBatchRequest {
        operation: "download".to_string(),
        transfers: vec!["basic".to_string()],
        objects: vec![LfsBatchObject {
            oid: pointer.oid.clone(),
            size: pointer.size,
        }],
    };

    info!(
        "LFS batch API: requesting download URL for oid={}",
        pointer.oid
    );

    let resp = client
        .post(&batch_url)
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Content-Type", "application/vnd.git-lfs+json")
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            error!("LFS batch API request failed: {}", e);
            (StatusCode::BAD_GATEWAY, format!("LFS batch failed: {}", e))
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!(
            "LFS batch API returned {}: {}",
            status,
            &text[..text.len().min(200)]
        );
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "LFS batch {}: {}",
                status,
                &text[..text.len().min(200)]
            ),
        ));
    }

    let batch_resp: LfsBatchResponse = resp.json().await.map_err(|e| {
        error!("LFS batch response parse failed: {}", e);
        (
            StatusCode::BAD_GATEWAY,
            format!("LFS batch parse error: {}", e),
        )
    })?;

    let obj = batch_resp.objects.into_iter().next().ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "LFS batch returned no objects".to_string(),
        )
    })?;

    if let Some(err) = obj.error {
        error!("LFS object error {}: {}", err.code, err.message);
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("LFS error {}: {}", err.code, err.message),
        ));
    }

    let actions = obj.actions.ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "LFS object has no download action".to_string(),
        )
    })?;

    let headers = actions.download.header.unwrap_or_default();
    Ok((actions.download.href, headers))
}

/// Stream an HTTP response body to a file, returning the total byte count.
async fn stream_response_to_file(
    resp: reqwest::Response,
    target: &Path,
    path_label: &str,
) -> Result<u64, (StatusCode, String)> {
    let mut file = tokio::fs::File::create(target).await.map_err(|e| {
        error!("Failed to create {}: {}", target.display(), e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Create file failed: {}", e),
        )
    })?;

    let mut byte_count: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            error!("Stream error downloading {}: {}", path_label, e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Download stream error: {}", e),
            )
        })?;
        file.write_all(&chunk).await.map_err(|e| {
            error!("Failed to write chunk to {}: {}", target.display(), e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Write failed: {}", e),
            )
        })?;
        byte_count += chunk.len() as u64;
    }

    file.flush().await.map_err(|e| {
        error!("Failed to flush {}: {}", target.display(), e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Flush failed: {}", e),
        )
    })?;

    Ok(byte_count)
}

// ============================================================================
// GET /write — Download from GitHub and write directly to local disk
// Used for large/binary files that would timeout or corrupt via DayZ RestContext
//
// NOTE: This is a state-changing endpoint using GET. DayZ's RestContext only
// exposes GET_now(), so POST/PUT is not an option. The IP allowlist mitigates
// the risk of accidental invocations from browsers or cache-warming proxies.
// ============================================================================

async fn handle_write(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = resolve_token(&state, &params)?;

    let owner = params.get("owner").map_or("", String::as_str);
    let repo = params.get("repo").map_or("", String::as_str);
    let branch = params.get("branch").map_or("main", String::as_str);
    let path = params.get("path").map_or("", String::as_str);
    let dest = params.get("dest").map_or("", String::as_str);
    let localpath = params.get("localpath").map_or("", String::as_str);

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

    // Temp file for atomic write — stream here first, rename on success.
    let tmp_target = resolved_target.with_extension(format!(
        "{}.tmp",
        resolved_target
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
    ));

    // Check Content-Length to decide whether this could be an LFS pointer.
    // LFS pointers are always < 512 bytes; real binary files are much larger.
    let content_length = resp.content_length();
    let maybe_lfs = content_length.map_or(true, |len| len <= 512);

    let byte_count: u64 = if maybe_lfs {
        // Buffer the (small) body and check for LFS pointer
        let body = resp.bytes().await.map_err(|e| {
            error!("Failed to read response body for {}: {}", path, e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Read body failed: {}", e),
            )
        })?;

        if let Some(pointer) = parse_lfs_pointer(&body) {
            // ── LFS pointer detected ────────────────────────────────────
            info!(
                "LFS pointer detected for {} (oid={}, size={})",
                path, pointer.oid, pointer.size
            );

            // If the target file already exists with the expected size, skip download
            if let Ok(meta) = tokio::fs::metadata(&resolved_target).await {
                if meta.len() == pointer.size {
                    info!(
                        "LFS file already on disk with correct size, skipping: {} ({} bytes)",
                        path, pointer.size
                    );
                    return Ok::<_, (StatusCode, String)>((
                        StatusCode::OK,
                        format!("{{\"ok\":true,\"bytes\":{}}}", pointer.size),
                    ));
                }
            }

            // Resolve download URL synchronously (fast — just an API call)
            let (lfs_url, lfs_headers) =
                resolve_lfs(&state.client, &token, owner, repo, &pointer).await?;

            info!(
                "LFS download (async): {} ({} bytes expected)",
                &lfs_url[..lfs_url.len().min(120)],
                pointer.size
            );

            // Spawn background task — return immediately so the DayZ mod doesn't timeout.
            // Increment pending_lfs before spawning so /lfs-status reflects this download
            // immediately; each exit path in the task decrements it.
            state.pending_lfs.fetch_add(1, Ordering::SeqCst);
            let bg_client = state.client.clone();
            let bg_target = resolved_target.clone();
            let bg_tmp = tmp_target.clone();
            let bg_path = path.to_string();
            let bg_pending = Arc::clone(&state.pending_lfs);
            let bg_failed = Arc::clone(&state.failed_lfs);
            tokio::spawn(async move {
                let mut lfs_req = bg_client.get(&lfs_url);
                for (k, v) in &lfs_headers {
                    lfs_req = lfs_req.header(k, v);
                }

                let lfs_resp = match lfs_req.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("LFS background download failed: {}", e);
                        bg_failed.fetch_add(1, Ordering::SeqCst);
                        bg_pending.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }
                };

                if !lfs_resp.status().is_success() {
                    let status = lfs_resp.status();
                    let text = lfs_resp.text().await.unwrap_or_default();
                    error!(
                        "LFS background download returned {}: {}",
                        status,
                        &text[..text.len().min(200)]
                    );
                    bg_failed.fetch_add(1, Ordering::SeqCst);
                    bg_pending.fetch_sub(1, Ordering::SeqCst);
                    return;
                }

                match stream_response_to_file(lfs_resp, &bg_tmp, &bg_path).await {
                    Ok(count) => {
                        if let Err(e) = tokio::fs::rename(&bg_tmp, &bg_target).await {
                            error!("LFS rename failed {} -> {}: {}", bg_tmp.display(), bg_target.display(), e);
                            let _ = std::fs::remove_file(&bg_tmp);
                            bg_failed.fetch_add(1, Ordering::SeqCst);
                            bg_pending.fetch_sub(1, Ordering::SeqCst);
                            return;
                        }
                        info!("LFS download complete: {} ({} bytes)", bg_path, count);
                        bg_pending.fetch_sub(1, Ordering::SeqCst);
                    }
                    Err(_) => {
                        let _ = std::fs::remove_file(&bg_tmp);
                        bg_failed.fetch_add(1, Ordering::SeqCst);
                        bg_pending.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            });

            // Return immediately — the mod records the SHA, download continues in background
            return Ok::<_, (StatusCode, String)>((
                StatusCode::OK,
                format!("{{\"ok\":true,\"bytes\":{}}}", pointer.size),
            ));
        } else {
            // ── Not LFS — write the buffered body directly ──────────────
            let len = body.len() as u64;
            let mut file =
                tokio::fs::File::create(&tmp_target)
                    .await
                    .map_err(|e| {
                        error!("Failed to create {}: {}", tmp_target.display(), e);
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Create file failed: {}", e),
                        )
                    })?;
            file.write_all(&body).await.map_err(|e| {
                error!("Failed to write to {}: {}", tmp_target.display(), e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Write failed: {}", e),
                )
            })?;
            file.flush().await.map_err(|e| {
                error!("Failed to flush {}: {}", tmp_target.display(), e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Flush failed: {}", e),
                )
            })?;
            len
        }
    } else {
        // ── Large file, definitely not LFS — stream directly ────────────
        stream_response_to_file(resp, &tmp_target, path)
            .await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_target);
                e
            })?
    };

    // Atomic rename from .tmp to final path
    tokio::fs::rename(&tmp_target, &resolved_target)
        .await
        .map_err(|e| {
            error!(
                "Failed to rename {} -> {}: {}",
                tmp_target.display(),
                resolved_target.display(),
                e
            );
            let _ = std::fs::remove_file(&tmp_target);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Rename failed: {}", e),
            )
        })?;

    info!("Write OK: {} ({} bytes)", path, byte_count);

    Ok::<_, (StatusCode, String)>((
        StatusCode::OK,
        format!("{{\"ok\":true,\"bytes\":{}}}", byte_count),
    ))
}

// ============================================================================
// GET /lfs-status — Poll LFS background download progress
//
// Returns {"pending":N,"failed":M}.
// If any downloads are still in flight, the handler sleeps 1 second before
// responding so each DayZ GET_now() call naturally spaces out the poll loop
// without requiring Sleep() on the EnScript side.
// ============================================================================

async fn handle_lfs_status(State(state): State<AppState>) -> impl IntoResponse {
    let pending = state.pending_lfs.load(Ordering::SeqCst);
    if pending > 0 {
        sleep(Duration::from_secs(1)).await;
    }
    let pending = state.pending_lfs.load(Ordering::SeqCst);
    let failed = state.failed_lfs.load(Ordering::SeqCst);
    (
        StatusCode::OK,
        format!("{{\"pending\":{},\"failed\":{}}}", pending, failed),
    )
}
