//! Minimal SPEC-v1 thermal HTTP surface (standalone bring-up mode).
//!
//! Endpoints (see docs/spec-thermal-api.md — proposal draft):
//! - `GET /api/thermal/current[?grid=1]` — JSON frame reading (NVR adapter
//!   data source)
//! - `GET /api/thermal/snapshot[?format=bmp|json]` — rendered pseudo-color
//!   image (BMP in v1 bring-up; JPEG lands with the webui encoder pass) or
//!   the radiometric equivalent of `current?grid=1`
//! - `GET /healthz` — liveness
//!
//! No session auth in standalone bring-up mode (LAN deployment behind the
//! board firewall); the family webui envelope (login + CSRF) arrives with
//! the M5 webui integration. GB28181 carries the production stream.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::hub::{ThermalHub, ThermalStatus};
use crate::mlx90640::{HEIGHT, WIDTH};
use crate::render::{render_rgb, rgb_to_bmp};

pub async fn serve(hub: Arc<ThermalHub>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("http: listening on 0.0.0.0:{port}");
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let hub = Arc::clone(&hub);
        tokio::spawn(async move {
            let _ = handle_conn(stream, hub).await;
        });
    }
}

async fn handle_conn(mut stream: TcpStream, hub: Arc<ThermalHub>) -> std::io::Result<()> {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if method != "GET" {
        return respond(&mut stream, 405, "text/plain", b"method not allowed").await;
    }
    let qp: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    let q = |key: &str| qp.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    match path {
        "/healthz" => respond(&mut stream, 200, "text/plain", b"ok").await,
        "/" => {
            // Bring-up viewer page (same role as the retired python viewer,
            // now with absolute temperatures from the M2 chain).
            let html = br#"<!doctype html><html><head><meta charset=utf-8><title>MLX90640 Thermal</title>
<style>body{background:#111;color:#ddd;font-family:sans-serif;text-align:center}
img{width:960px;max-width:96vw;border:1px solid #444}p{font-size:13px}</style></head><body>
<h3>MLX90640 &middot; sensor 32&times;24 &rarr; render 640&times;480 &middot; absolute &deg;C</h3>
<img id=f src="/api/thermal/snapshot?format=bmp">
<p id=s>warming up&hellip;</p>
<p style="font-size:12px;color:#888">sensor bit-bang ~40s/frame until pull-up fix; page refreshes every 5s</p>
<script>
function tick(){fetch('/api/thermal/current?grid=0').then(r=>r.json()).then(j=>{
 s.textContent=(j.source==='sensor'?'SENSOR':'SYNTHETIC')+'  Ta '+(j.ta!=null?j.ta.toFixed(1)+'\u00b0C':'-')+'  max '+j.max.value_c.toFixed(1)+'  min '+j.min.value_c.toFixed(1)+'  @ '+new Date(j.timestamp_ms).toLocaleTimeString();
 if(j.timestamp_ms) f.src='/api/thermal/snapshot?format=bmp&t='+j.timestamp_ms;}).catch(()=>{});}
tick();setInterval(tick,5000);</script></body></html>"#;
            respond(&mut stream, 200, "text/html", html).await
        }
        "/api/thermal/current" => {
            let with_grid = q("grid").as_deref() == Some("1");
            let body = current_json(&hub.status(), with_grid);
            respond(&mut stream, 200, "application/json", body.as_bytes()).await
        }
        "/api/thermal/snapshot" => match q("format").as_deref() {
            Some("bmp") => {
                let st = hub.status();
                let rgb = render_rgb(&st.to, 640, 480);
                let bmp = rgb_to_bmp(&rgb, 640, 480);
                respond(&mut stream, 200, "image/bmp", &bmp).await
            }
            // Default json = radiometric snapshot (SPEC alternative form).
            _ => {
                let body = current_json(&hub.status(), true);
                respond(&mut stream, 200, "application/json", body.as_bytes()).await
            }
        },
        _ => respond(&mut stream, 404, "text/plain", b"not found").await,
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
        if st.origin == crate::hub::FrameOrigin::Sensor {
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
}
