//! Web surface: family SPEC envelope (auth/capabilities/status/SSE) over the
//! thermal hub, plus the bring-up viewer page and the SPEC v1 thermal
//! extension endpoints.
//!
//! Core (SPEC §2/§3): GET /api/health, /api/auth/me, POST /api/auth/login,
//! POST /api/auth/logout, GET /api/status, GET /api/capabilities,
//! GET /api/events (SSE), static front-end assets.
//! Thermal extension (SPEC draft §thermal): GET /api/thermal/current[?grid=1],
//! GET /api/thermal/snapshot[?format=bmp|json].
//! Bring-up: GET /viewer (pseudo-color viewer with absolute °C),
//! GET /healthz, GET /api/debug/alarm-test.
//!
//! Auth dialect: single admin password from config; **empty password = open
//! bring-up mode** (every session-bearing endpoint answers as admin). With a
//! password set, /api/* requires the session cookie (SPEC session model,
//! 24 h). Session tokens draw entropy from /dev/urandom at boot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::hub::{FrameOrigin, ThermalHub, ThermalStatus};
use crate::mlx90640::{HEIGHT, WIDTH};
use crate::render::{render_rgb_range, rgb_to_bmp};

/// Optional GB28181 alarm notifier (present when gb28181 is enabled) —
/// powers the pre-integration debug trigger.
pub type AlarmNotifier = Arc<gb28181_rs::subscribe::DeviceNotifier>;

const SESSION_TTL: Duration = Duration::from_secs(24 * 3600);

pub struct WebCtx {
    pub hub: Arc<ThermalHub>,
    pub notifier: Option<AlarmNotifier>,
    /// Admin password; empty = open bring-up mode (no login required).
    pub password: String,
    /// Static bearer token for machine consumers (NVR adapter path,
    /// SPEC §thermal finalize point 3). Empty = bearer auth off. Session
    /// cookie OR `Authorization: Bearer <token>` — either passes the gate.
    pub api_token: String,
    /// Front-end asset directory (family webui bundle, `make sync-thermal`).
    pub static_dir: PathBuf,
    pub device_name: String,
    /// Where PUT /api/hardware/config persists the overlay (sibling
    /// `hardware.toml` of the config file); None = runtime-only.
    pub persist_path: Option<PathBuf>,
    sessions: Mutex<HashMap<String, Instant>>,
    boot: Instant,
}

impl WebCtx {
    pub fn new(
        hub: Arc<ThermalHub>,
        notifier: Option<AlarmNotifier>,
        password: String,
        api_token: String,
        static_dir: PathBuf,
        device_name: String,
        persist_path: Option<PathBuf>,
    ) -> Self {
        Self {
            hub,
            notifier,
            password,
            api_token,
            static_dir,
            device_name,
            persist_path,
            sessions: Mutex::new(HashMap::new()),
            boot: Instant::now(),
        }
    }

    fn open_mode(&self) -> bool {
        self.password.is_empty()
    }

    /// Session token minted from boot-time entropy + a monotonically
    /// advancing counter.
    fn mint_session(&self) -> String {
        use std::hash::{Hash, Hasher};
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        boot_entropy().hash(&mut h);
        n.hash(&mut h);
        let mix = h.finish() ^ n.rotate_left(32);
        format!("{mix:016x}{n:016x}")
    }

    fn bearer_ok(&self, auth_header: &str) -> bool {
        if self.api_token.is_empty() {
            return false;
        }
        auth_header
            .strip_prefix("Bearer ")
            .is_some_and(|t| t.trim() == self.api_token)
    }

    fn session_valid(&self, cookies: &str) -> bool {
        if self.open_mode() {
            return true;
        }
        let token = cookie_value(cookies, "session").unwrap_or_default();
        let mut sessions = self.sessions.lock().expect("sessions lock");
        match sessions.get(&token) {
            Some(exp) if *exp > Instant::now() => true,
            Some(_) => {
                sessions.remove(&token);
                false
            }
            None => false,
        }
    }
}

/// 16 bytes from /dev/urandom read once per process (falls back to a
/// time-derived value on read-only systems).
fn boot_entropy() -> [u8; 16] {
    static ENTROPY: std::sync::OnceLock<[u8; 16]> = std::sync::OnceLock::new();
    *ENTROPY.get_or_init(|| {
        let mut buf = [0u8; 16];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            if f.read_exact(&mut buf).is_ok() {
                return buf;
            }
        }
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seed = t.to_le_bytes();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = seed[i % seed.len()] ^ (t as u8).wrapping_mul(i as u8);
        }
        buf
    })
}

fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|c| {
        let c = c.trim();
        let (k, v) = c.split_once('=')?;
        (k.trim() == name).then(|| v.trim().to_string())
    })
}

/// Extract a top-level string field from a small JSON object (login body).
/// Handles plain values; escaped quotes inside the value are not supported
/// (documented: admin passwords are constrained ASCII).
fn json_string_field(body: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let i = body.find(&key)?;
    let rest = &body[i + key.len()..];
    let j = rest.find('"')?;
    let rest = &rest[j + 1..];
    let k = rest.find('"')?;
    Some(rest[..k].to_string())
}

pub async fn serve(ctx: Arc<WebCtx>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("http: listening on 0.0.0.0:{port}");
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            let _ = handle_conn(stream, ctx).await;
        });
    }
}

async fn handle_conn(mut stream: TcpStream, ctx: Arc<WebCtx>) -> std::io::Result<()> {
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let req_line = req.lines().next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    // Headers after the request line: find Cookie header.
    let cookies = req
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
        .map(|l| l[7..].to_string())
        .unwrap_or_default();
    let auth_header = req
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
        .map(|l| l[15..].to_string())
        .unwrap_or_default();
    let body_at = req.find("\r\n\r\n").map(|i| i + 4).unwrap_or(req.len());
    let body = req[body_at.min(req.len())..].to_string();

    if method.is_empty() {
        return Ok(());
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target.as_str(), ""),
    };
    let qp: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    let q = |key: &str| qp.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let authorized = ctx.session_valid(&cookies) || ctx.bearer_ok(auth_header.trim());

    // --- static front-end (family webui bundle); /viewer stays the
    // bring-up page ---
    if method == "GET" && !path.starts_with("/api/") && path != "/healthz" && path != "/viewer" {
        return serve_static(&mut stream, &ctx, path).await;
    }

    match (method.as_str(), path) {
        ("GET", "/healthz") => respond(&mut stream, 200, "text/plain", b"ok").await,
        ("GET", "/viewer") => respond(&mut stream, 200, "text/html", VIEWER_HTML).await,

        // --- SPEC §2 core auth ---
        ("GET", "/api/auth/me") => {
            if authorized {
                respond(
                    &mut stream,
                    200,
                    "application/json",
                    br#"{"ok":true,"data":{"username":"admin","role":"admin"}}"#,
                )
                .await
            } else {
                respond(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"ok":false,"error":"unauthorized"}"#,
                )
                .await
            }
        }
        ("POST", "/api/auth/login") => {
            let given = json_string_field(&body, "password").unwrap_or_default();
            if !ctx.open_mode() && given != ctx.password {
                return respond(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"ok":false,"error":"invalid_credentials"}"#,
                )
                .await;
            }
            let token = ctx.mint_session();
            ctx.sessions
                .lock()
                .expect("sessions lock")
                .insert(token.clone(), Instant::now() + SESSION_TTL);
            let body = br#"{"ok":true,"data":{"username":"admin","role":"admin"}}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Set-Cookie: session={token}; HttpOnly; Path=/; SameSite=Strict; Max-Age=86400\r\n\
                 Set-Cookie: csrf-token={token}; Path=/; SameSite=Strict\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(body).await?;
            stream.flush().await
        }
        ("POST", "/api/auth/logout") => {
            if let Some(token) = cookie_value(&cookies, "session") {
                ctx.sessions.lock().expect("sessions lock").remove(&token);
            }
            respond(
                &mut stream,
                200,
                "application/json",
                br#"{"ok":true,"data":{"username":null}}"#,
            )
            .await
        }

        // --- SPEC §3 core ---
        ("GET", "/api/health") if authorized => {
            let body = format!(
                "{{\"ok\":true,\"data\":{{\"status\":\"ok\",\"uptime\":{}}}}}",
                ctx.boot.elapsed().as_secs()
            );
            respond(&mut stream, 200, "application/json", body.as_bytes()).await
        }
        ("GET", "/api/status") if authorized => {
            let st = ctx.hub.status();
            let body = format!(
                "{{\"ok\":true,\"data\":{{\"device_name\":\"{}\",\"model\":\"MLX90640 Thermal\",\"vendor\":\"MiBee\",\
                 \"firmware\":\"{}\",\"uptime\":{},\"source\":\"{}\",\"sensor_fps\":{:.3},\
                 \"ta\":{:.2}}}}}",
                ctx.device_name,
                env!("CARGO_PKG_VERSION"),
                ctx.boot.elapsed().as_secs(),
                if st.origin == FrameOrigin::Sensor {
                    "sensor"
                } else {
                    "synthetic"
                },
                st.sensor_fps,
                st.common.as_ref().map(|c| c.ta).unwrap_or(0.0),
            );
            respond(&mut stream, 200, "application/json", body.as_bytes()).await
        }
        ("GET", "/api/capabilities") if authorized => {
            let st = ctx.hub.status();
            let available = st.origin == FrameOrigin::Sensor;
            let body = format!(
                "{{\"ok\":true,\"data\":{{\"spec_version\":\"1\",\
                 \"device\":{{\"name\":\"{}\",\"model\":\"MLX90640 Thermal\",\"vendor\":\"MiBee\"}},\
                 \"auth\":{{\"model\":\"session\",\"setup\":false,\"open\":{}}},\
                 \"thermal\":{{\"available\":{available},\"radiometric\":{},\"max_fps\":{:.3},\"rules\":0,\"hardware\":true}},\
                 \"events\":[\"thermal.temperature\"]}}}}",
                ctx.device_name,
                ctx.open_mode(),
                st.common.is_some(),
                st.sensor_fps,
            );
            respond(&mut stream, 200, "application/json", body.as_bytes()).await
        }
        ("GET", "/api/events") if authorized => sse_loop(&mut stream, &ctx).await,

        // --- thermal extension (SPEC draft) ---
        ("GET", "/api/thermal/current") if authorized => {
            let with_grid = q("grid").as_deref() == Some("1");
            let body = current_json(&ctx.hub.status(), with_grid);
            respond(&mut stream, 200, "application/json", body.as_bytes()).await
        }
        ("GET", "/api/thermal/snapshot") if authorized => match q("format").as_deref() {
            Some("bmp") => {
                let st = ctx.hub.status();
                let lo = q("lo").and_then(|v| v.parse::<f32>().ok());
                let hi = q("hi").and_then(|v| v.parse::<f32>().ok());
                // On-image temperature overlays by default (thermal-camera
                // look); `?overlay=0` renders the clean pseudo-color frame.
                let rgb = if q("overlay").as_deref() == Some("0") {
                    render_rgb_range(&st.to, 640, 480, lo, hi)
                } else {
                    crate::render::render_rgb_annotated(&st.to, 640, 480, lo, hi)
                };
                let bmp = rgb_to_bmp(&rgb, 640, 480);
                respond(&mut stream, 200, "image/bmp", &bmp).await
            }
            // Default json = radiometric snapshot (SPEC alternative form).
            _ => {
                let body = current_json(&ctx.hub.status(), true);
                respond(&mut stream, 200, "application/json", body.as_bytes()).await
            }
        },

        // --- hardware bring-up (SPEC §3.7 hardware): pins/address + scan ---
        ("GET", "/api/hardware") if authorized => {
            let t = ctx.hub.hardware.target();
            let scan = ctx.hub.hardware.scan_state();
            let body = serde_json::json!({
                "ok": true,
                "data": {
                    "bus": serde_json::to_value(&t.bus).unwrap_or(serde_json::Value::Null),
                    "addr": t.addr,
                    "persist_path": ctx.persist_path.as_ref().map(|p| p.display().to_string()),
                    "scan": scan_json(&scan),
                }
            });
            respond(
                &mut stream,
                200,
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/api/hardware/scan") if authorized => {
            let body =
                serde_json::json!({"ok": true, "data": scan_json(&ctx.hub.hardware.scan_state())});
            respond(
                &mut stream,
                200,
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("POST", "/api/hardware/scan") if authorized => {
            if ctx.hub.hardware.scan_state().running {
                return respond(
                    &mut stream,
                    409,
                    "application/json",
                    br#"{"ok":false,"error":"scan already running"}"#,
                )
                .await;
            }
            // Optional body {"gpio_candidates":[[sda,scl],...]} — default
            // is the classic Orange Pi Zero 3 PH2/PH3 pair.
            let mut cands: Vec<(u32, u32)> = vec![(226, 227)];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(list) = v.get("gpio_candidates").and_then(|c| c.as_array()) {
                    let parsed: Vec<(u32, u32)> = list
                        .iter()
                        .filter_map(|p| {
                            let a = p.as_array()?;
                            Some((a.first()?.as_u64()? as u32, a.get(1)?.as_u64()? as u32))
                        })
                        .collect();
                    if !parsed.is_empty() {
                        cands = parsed;
                    }
                }
            }
            let hw = ctx.hub.hardware.clone();
            let _ = std::thread::Builder::new()
                .name("hw-scan".into())
                .spawn(move || hw.run_scan(&cands));
            respond(
                &mut stream,
                200,
                "application/json",
                br#"{"ok":true,"data":{"running":true}}"#,
            )
            .await
        }
        ("PUT", "/api/hardware/config") if authorized => {
            #[derive(serde::Deserialize)]
            struct HardwarePut {
                bus: crate::settings::BusSpec,
                addr: u8,
            }
            let put: HardwarePut = match serde_json::from_str(&body) {
                Ok(p) => p,
                Err(e) => {
                    let m = format!("{{\"ok\":false,\"error\":\"bad config: {e}\"}}");
                    return respond(&mut stream, 400, "application/json", m.as_bytes()).await;
                }
            };
            if !crate::scan::ADDR_RANGE.contains(&put.addr) {
                return respond(
                    &mut stream,
                    400,
                    "application/json",
                    br#"{"ok":false,"error":"addr must be 0x30..=0x3E (MLX90640 strapping window)"}"#,
                )
                .await;
            }
            if let crate::settings::BusSpec::Gpio { sda, scl } = &put.bus {
                if sda == scl || *sda == 0 || *scl == 0 {
                    return respond(
                        &mut stream,
                        400,
                        "application/json",
                        br#"{"ok":false,"error":"gpio sda/scl must be distinct non-zero pins"}"#,
                    )
                    .await;
                }
            }
            ctx.hub.hardware.set_target(crate::hub::SensorTarget {
                bus: put.bus.clone(),
                addr: put.addr,
            });
            // Persist for reboot (best-effort — runtime apply already done).
            let (persisted, persist_error) = match &ctx.persist_path {
                None => (false, Some("persistence disabled".into())),
                Some(path) => {
                    match std::fs::write(path, crate::settings::overlay_toml(&put.bus, put.addr)) {
                        Ok(()) => (true, None),
                        Err(e) => (false, Some(format!("{e}"))),
                    }
                }
            };
            let body = serde_json::json!({
                "ok": true,
                "data": {
                    "bus": serde_json::to_value(&put.bus).unwrap_or(serde_json::Value::Null),
                    "addr": put.addr,
                    "persisted": persisted,
                    "persist_error": persist_error,
                }
            });
            respond(
                &mut stream,
                200,
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }

        // --- bring-up debug ---
        ("GET", "/api/debug/alarm-test") if authorized => {
            let body = match ctx.notifier.as_ref() {
                Some(n) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let (y, mo, dd, h, mi, s) = civil_from_unix(now);
                    let time = format!("{y:04}-{mo:02}-{dd:02}T{h:02}:{mi:02}:{s:02}");
                    let sent = n.send_alarm("4", "5", &time, "2", "thermal integration test alarm");
                    format!("{{\"sent\":{sent},\"time\":\"{time}\"}}")
                }
                None => "{\"sent\":false,\"error\":\"gb28181 disabled\"}".to_string(),
            };
            respond(&mut stream, 200, "application/json", body.as_bytes()).await
        }

        _ => {
            if path.starts_with("/api/") && !authorized {
                respond(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"error":"unauthorized"}"#,
                )
                .await
            } else {
                respond(&mut stream, 404, "text/plain", b"not found").await
            }
        }
    }
}

/// SSE (SPEC §6): periodic `thermal.temperature` + keepalive comments.
/// One event per second (SPEC draft cadence: >=1 s); a dropped connection
/// ends the loop on the next write error.
async fn sse_loop(stream: &mut TcpStream, ctx: &WebCtx) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut since_keepalive = 0u32;
    loop {
        tick.tick().await;
        let st = ctx.hub.status();
        let payload = sse_temperature_payload(&st);
        let frame = format!("event: thermal.temperature\ndata: {payload}\n\n");
        if stream.write_all(frame.as_bytes()).await.is_err() {
            break;
        }
        stream.flush().await?;
        since_keepalive += 1;
        if since_keepalive >= 15 {
            if stream.write_all(b": keepalive\n\n").await.is_err() {
                break;
            }
            since_keepalive = 0;
        }
    }
    Ok(())
}

/// ScanState → SPEC §3.7 hardware JSON (hits carry their paste-ready
/// config snippet so the UI never re-implements formatting).
fn scan_json(st: &crate::hub::ScanState) -> serde_json::Value {
    serde_json::json!({
        "running": st.running,
        "started_ms": st.started_ms,
        "finished_ms": st.finished_ms,
        "hits": st.hits.iter().map(|h| serde_json::json!({
            "bus": h.bus,
            "addr": h.addr,
            "id": h.id,
            "config": crate::scan::config_snippet(h),
        })).collect::<Vec<_>>(),
        "pair_errors": st.pair_errors,
        "error": st.error,
    })
}

/// §6 `thermal.temperature` payload (same fields as /api/thermal/current
/// minus the grid).
fn sse_temperature_payload(st: &ThermalStatus) -> String {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let (mut min_i, mut max_i) = (0usize, 0usize);
    for (i, &v) in st.to.iter().enumerate() {
        if v < min {
            min = v;
            min_i = i;
        }
        if v > max {
            max = v;
            max_i = i;
        }
    }
    use std::fmt::Write;
    let mut out = String::with_capacity(256);
    let _ = write!(
        out,
        "{{\"timestamp_ms\":{},\"ta\":{:.2},\"trusted\":{},\
         \"max\":{{\"value_c\":{:.1},\"x\":{},\"y\":{}}},\
         \"min\":{{\"value_c\":{:.1},\"x\":{},\"y\":{}}}}}",
        st.timestamp_ms,
        st.common.as_ref().map(|c| c.ta).unwrap_or(0.0),
        st.common.is_some(),
        max,
        max_i % WIDTH,
        max_i / WIDTH,
        min,
        min_i % WIDTH,
        min_i / WIDTH,
    );
    out
}

async fn serve_static(stream: &mut TcpStream, ctx: &WebCtx, path: &str) -> std::io::Result<()> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    // Only index.html, style.css and js/<one-component> are exposed.
    let safe = match rel.split_once('/') {
        Some(("js", name)) if !name.is_empty() && !name.contains('/') => Some(format!("js/{name}")),
        Some(_) => None,
        None => match rel {
            "index.html" | "style.css" => Some(rel.to_string()),
            _ => None,
        },
    };
    let Some(rel) = safe else {
        return respond(stream, 404, "text/plain", b"not found").await;
    };
    let full = ctx.static_dir.join(&rel);
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let ctype = match Path::new(&rel).extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("css") => "text/css",
                Some("js") => "application/javascript",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                _ => "application/octet-stream",
            };
            respond(stream, 200, ctype, &bytes).await
        }
        Err(_) => {
            if rel == "index.html" {
                // No front-end bundle synced — fall back to the viewer page
                // so the port is never a dead end.
                respond(stream, 200, "text/html", VIEWER_HTML).await
            } else {
                respond(stream, 404, "text/plain", b"not found").await
            }
        }
    }
}

async fn respond(
    stream: &mut TcpStream,
    code: u16,
    ctype: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// SPEC v1 `GET /api/thermal/current` body (pure over the status snapshot).
pub fn current_json(st: &ThermalStatus, with_grid: bool) -> String {
    use std::fmt::Write;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let (mut min_i, mut max_i) = (0usize, 0usize);
    for (i, &v) in st.to.iter().enumerate() {
        if v < min {
            min = v;
            min_i = i;
        }
        if v > max {
            max = v;
            max_i = i;
        }
    }
    let mut out = String::with_capacity(4096);
    let _ = write!(
        out,
        "{{\"timestamp_ms\":{},\"frame_number\":{},\"trusted\":{},\"source\":\"{}\"",
        st.timestamp_ms,
        st.frame_number,
        st.common.is_some(),
        if st.origin == FrameOrigin::Sensor {
            "sensor"
        } else {
            "synthetic"
        },
    );
    if let Some(c) = &st.common {
        let _ = write!(out, ",\"ta\":{:.2},\"vdd\":{:.3}", c.ta, c.vdd);
    }
    let _ = write!(
        out,
        ",\"max\":{{\"value_c\":{:.1},\"x\":{},\"y\":{}}}",
        max,
        max_i % WIDTH,
        max_i / WIDTH
    );
    let _ = write!(
        out,
        ",\"min\":{{\"value_c\":{:.1},\"x\":{},\"y\":{}}}",
        min,
        min_i % WIDTH,
        min_i / WIDTH
    );
    let _ = write!(out, ",\"sensor_fps\":{:.3}", st.sensor_fps);
    if let Some(err) = &st.last_sensor_error {
        let _ = write!(out, ",\"last_sensor_error\":{:?}", err);
    }
    if with_grid {
        let mut vals = String::with_capacity(768 * 6);
        for (i, v) in st.to.iter().enumerate() {
            if i > 0 {
                vals.push(',');
            }
            let _ = write!(vals, "{v:.1}");
        }
        let _ = write!(
            out,
            ",\"grid\":{{\"w\":{WIDTH},\"h\":{HEIGHT},\"unit\":\"deg_c\",\"values\":[{vals}]}}"
        );
    }
    out.push('}');
    out
}

/// Civil date from unix seconds (UTC — fine for debug timestamps; the
/// platform clock is the authority anyway).
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        m as u32,
        d as u32,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// Bring-up viewer page (same role as the retired python viewer, now with
/// absolute temperatures, a legend, min/max markers and hover readout).
const VIEWER_HTML: &[u8] = br#"<!doctype html><html><head><meta charset=utf-8><title>MLX90640 Thermal</title>
<style>body{background:#111;color:#ddd;font-family:sans-serif;text-align:center}
img{width:960px;max-width:96vw;border:1px solid #444;display:block}p{font-size:13px}
#wrap{position:relative;display:inline-block}canvas{position:absolute;inset:0;width:100%;height:100%;cursor:crosshair}
#tip{position:fixed;background:#000c;color:#fff;padding:3px 8px;border-radius:4px;font-size:13px;pointer-events:none;display:none;z-index:9}
#legend{width:60%;max-width:560px;height:14px;margin:8px auto;border:1px solid #555;border-radius:3px;
background:linear-gradient(90deg,#000000,#120840,#400c78,#8c1e82,#d8583c,#f8a830,#ffe878,#ffffe0)}
#lgrad{display:flex;justify-content:space-between;width:60%;max-width:560px;margin:0 auto;font-size:12px;color:#aaa}</style></head><body>
<h3>MLX90640 &middot; sensor 32&times;24 &rarr; render 640&times;480 &middot; absolute &deg;C</h3>
<div id=wrap><img id=f src="/api/thermal/snapshot?format=bmp"><canvas id=c></canvas></div>
<div id=legend></div><div id=lgrad><span id=llo>...</span><span id=lmid>...</span><span id=lhi>...</span></div>
<p id=s>warming up&hellip;</p>
<p style="font-size:12px;color:#888">sensor bit-bang ~40s/frame until pull-up fix &middot; hover the image for point temperature &middot; image carries max/min/center readings</p>
<div id=tip></div>
<script>
let grid=null,GW=32,GH=24,last=null;
const f=document.getElementById('f'),c=document.getElementById('c'),tip=document.getElementById('tip');
// Temperature markers are baked into the image by the renderer
// (max/min crosshairs + center spot); this page keeps hover readout only.
c.addEventListener('mousemove',e=>{
 if(!grid){tip.style.display='none';return;}
 const r=c.getBoundingClientRect();
 const gx=Math.max(0,Math.min(GW-1,Math.floor((e.clientX-r.left)/r.width*GW)));
 const gy=Math.max(0,Math.min(GH-1,Math.floor((e.clientY-r.top)/r.height*GH)));
 const v=grid[gy*GW+gx];
 tip.style.display='block';tip.style.left=(e.clientX+14)+'px';tip.style.top=(e.clientY-24)+'px';
 tip.textContent=v.toFixed(1)+'\u00b0C  @ col '+(gx+1)+', row '+(gy+1);
});
c.addEventListener('mouseleave',()=>tip.style.display='none');
function tick(){fetch('/api/thermal/current?grid=1').then(r=>r.json()).then(j=>{
 last=j;grid=j.grid?j.grid.values:null;GW=j.grid?j.grid.w:32;GH=j.grid?j.grid.h:24;
 s.textContent=(j.source==='sensor'?'SENSOR':'SYNTHETIC')+'  Ta '+(j.ta!=null?j.ta.toFixed(1):'-')+'\u00b0C  max '+j.max.value_c.toFixed(1)+'  min '+j.min.value_c.toFixed(1)+'  @ '+new Date(j.timestamp_ms).toLocaleTimeString();
 llo.textContent=j.min.value_c.toFixed(1)+'\u00b0C';lhi.textContent=j.max.value_c.toFixed(1)+'\u00b0C';
 lmid.textContent=((j.min.value_c+j.max.value_c)/2).toFixed(1)+'\u00b0C';
 if(j.timestamp_ms){f.src='/api/thermal/snapshot?format=bmp&t='+j.timestamp_ms;}
}).catch(()=>{});}
tick();setInterval(tick,5000);</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::FrameOrigin;

    #[test]
    fn current_json_shape() {
        let st = ThermalStatus::synthetic(1.0, 7);
        let body = current_json(&st, false);
        assert!(body.contains("\"source\":\"synthetic\""), "{body}");
        assert!(body.contains("\"frame_number\":7"));
        assert!(body.contains("\"max\":{\"value_c\":"));
        assert!(body.contains("\"min\":{\"value_c\":"));
        assert!(!body.contains("\"grid\""));
        let grid = current_json(&st, true);
        assert!(grid.contains("\"grid\":{\"w\":32,\"h\":24"));
        assert!(grid.contains(",\"sensor_fps\""));
    }

    #[test]
    fn real_frame_json_marks_trusted_with_ta() {
        let mut st = ThermalStatus::synthetic(0.0, 9);
        st.origin = FrameOrigin::Sensor;
        st.common = Some(crate::ee::CommonParams {
            res_corr: 1.0,
            vdd: 3.29,
            ta: 33.5,
            k_gain: 1.0,
        });
        let body = current_json(&st, false);
        assert!(body.contains("\"trusted\":true"));
        assert!(body.contains("\"ta\":33.50"));
        assert!(body.contains("\"source\":\"sensor\""));
    }

    #[test]
    fn sse_payload_has_min_max_ta() {
        let mut st = ThermalStatus::synthetic(0.0, 3);
        st.common = Some(crate::ee::CommonParams {
            res_corr: 1.0,
            vdd: 3.3,
            ta: 30.0,
            k_gain: 1.0,
        });
        let p = sse_temperature_payload(&st);
        assert!(p.starts_with("{\"timestamp_ms\":"));
        assert!(p.contains("\"ta\":30.00"));
        assert!(p.contains("\"max\":{\"value_c\":"));
    }

    #[test]
    fn login_password_extraction() {
        let body = r#"{"username":"admin","password":"s3cret-pass"}"#;
        assert_eq!(
            json_string_field(body, "password").as_deref(),
            Some("s3cret-pass")
        );
        assert_eq!(
            json_string_field(body, "username").as_deref(),
            Some("admin")
        );
        assert_eq!(json_string_field("{}", "password"), None);
    }

    #[test]
    fn cookie_parsing() {
        let h = "session=abc123; csrf-token=xyz";
        assert_eq!(cookie_value(h, "session").as_deref(), Some("abc123"));
        assert_eq!(cookie_value(h, "csrf-token").as_deref(), Some("xyz"));
        assert_eq!(cookie_value(h, "missing"), None);
    }

    #[test]
    fn civil_date_spot_check() {
        // 2026-11-03 13:41:14 UTC = 1793710874
        assert_eq!(civil_from_unix(1_793_713_274), (2026, 11, 3, 13, 41, 14));
    }
}
