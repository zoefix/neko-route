//! 共享功能的 Rust 端 FRP 隧道客户端。
//!
//! 与 Go 服务端建立一条 `yamux over TLS` 长连接：
//! 1. TCP+TLS 拨入服务端，发 HTTP/1.1 升级请求（id+secret 在头里），期待 101 Switching Protocols；
//! 2. 在该连接上以 **yamux server 模式** 接受服务端按需开的 stream；
//! 3. 每个 stream = 一条 HTTP/1.1 请求，原样裸字节双向管道转发到本机 `127.0.0.1:<port>`
//!    （服务端已在请求里注入 `X-Neko-Share` 标记，本机据此强制令牌鉴权）。
//!
//! 由「共享开关」驱动启停，断线指数退避重连，状态上报给 UI。

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::poll_fn;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;

/// 与 Go 服务端共享的控制协议（见 server/internal/protocol）。
const PROTOCOL_VERSION: i32 = 1;
const CONTROL_PATH: &str = "/tunnel";
const UPGRADE_TOKEN: &str = "neko-tunnel";
const HANDSHAKE_MAX_BYTES: usize = 8192;

/// 隧道连接状态（上报给 UI）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareTunnelState {
    Disabled,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareTunnelStatus {
    pub state: ShareTunnelState,
    pub message: Option<String>,
}

impl Default for ShareTunnelStatus {
    fn default() -> Self {
        Self {
            state: ShareTunnelState::Disabled,
            message: None,
        }
    }
}

/// 启动隧道所需的参数（由 config + 环境变量组装）。
#[derive(Debug, Clone)]
pub struct ShareTunnelConfig {
    pub server_host: String,
    pub server_port: u16,
    pub identity: String,
    pub secret: String,
    pub local_port: u16,
    pub insecure_skip_verify: bool,
}

/// 隧道管理器：持有当前连接任务与状态，供 Tauri 命令启停 / 查询。
#[derive(Clone)]
pub struct ShareTunnel {
    status: Arc<RwLock<ShareTunnelStatus>>,
    cancel: Arc<RwLock<Option<CancellationToken>>>,
}

impl Default for ShareTunnel {
    fn default() -> Self {
        Self::new()
    }
}

impl ShareTunnel {
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(ShareTunnelStatus::default())),
            cancel: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn status(&self) -> ShareTunnelStatus {
        self.status.read().await.clone()
    }

    /// 启动（或以新参数重启）隧道：取消旧任务，起新连接循环。
    pub async fn start(&self, config: ShareTunnelConfig) {
        self.stop().await;
        let token = CancellationToken::new();
        *self.cancel.write().await = Some(token.clone());
        set_status(&self.status, ShareTunnelState::Connecting, None).await;
        let status = self.status.clone();
        tokio::spawn(async move { run_loop(config, status, token).await });
    }

    /// 停止隧道并置为 Disabled。
    pub async fn stop(&self) {
        if let Some(token) = self.cancel.write().await.take() {
            token.cancel();
        }
        set_status(&self.status, ShareTunnelState::Disabled, None).await;
    }
}

async fn set_status(
    status: &Arc<RwLock<ShareTunnelStatus>>,
    state: ShareTunnelState,
    message: Option<String>,
) {
    *status.write().await = ShareTunnelStatus { state, message };
}

/// 连接循环：断开即指数退避重连，直到被取消。
async fn run_loop(
    config: ShareTunnelConfig,
    status: Arc<RwLock<ShareTunnelStatus>>,
    cancel: CancellationToken,
) {
    let mut backoff = 1u64;
    while !cancel.is_cancelled() {
        set_status(&status, ShareTunnelState::Connecting, None).await;
        match connect_once(&config, &status, &cancel).await {
            Ok(()) => backoff = 1, // 干净断开（被取消或服务端关闭）
            Err(error) => {
                set_status(&status, ShareTunnelState::Error, Some(error)).await;
            }
        }
        if cancel.is_cancelled() {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
        }
        backoff = (backoff.saturating_mul(2)).min(30);
    }
    set_status(&status, ShareTunnelState::Disabled, None).await;
}

/// 建一次连接，跑到断开为止。返回 Ok = 干净断开，Err = 出错（触发退避重连）。
async fn connect_once(
    config: &ShareTunnelConfig,
    status: &Arc<RwLock<ShareTunnelStatus>>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let tcp = TcpStream::connect((config.server_host.as_str(), config.server_port))
        .await
        .map_err(|e| format!("connect {}:{} failed: {e}", config.server_host, config.server_port))?;
    tcp.set_nodelay(true).ok();

    let connector = build_connector(config.insecure_skip_verify)?;
    let domain = ServerName::try_from(config.server_host.clone())
        .map_err(|e| format!("invalid tunnel host: {e}"))?;
    let mut tls = connector
        .connect(domain, tcp)
        .await
        .map_err(|e| format!("TLS handshake failed: {e}"))?;

    // 控制握手：HTTP/1.1 升级，身份放头里，期待 101 Switching Protocols。
    let request = format!(
        "GET {CONTROL_PATH} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: {UPGRADE_TOKEN}\r\nX-Neko-Id: {id}\r\nX-Neko-Secret: {secret}\r\nX-Neko-Ver: {PROTOCOL_VERSION}\r\n\r\n",
        host = config.server_host,
        id = config.identity,
        secret = config.secret,
    );
    tls.write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write upgrade: {e}"))?;
    tls.flush().await.map_err(|e| e.to_string())?;

    // 读响应头（逐字节到 \r\n\r\n，避免吞掉随后的 yamux 字节）。
    let head = read_http_response_head(&mut tls).await?;
    let status_line = head.lines().next().unwrap_or_default();
    if !status_line.contains("101") {
        return Err(format!("control upgrade rejected: {}", status_line.trim()));
    }
    set_status(status, ShareTunnelState::Connected, None).await;

    // yamux server 模式：服务端按需开 stream，我们接受。
    let socket = tls.compat();
    // 接收窗口 yamux 0.13 已自动调优(起始 256KB→上限 1GB)；这里只调大发送帧，
    // 减少大响应/图片下行的分帧开销。
    let mut yamux_cfg = yamux::Config::default();
    yamux_cfg.set_split_send_size(64 * 1024);
    let mut connection = yamux::Connection::new(socket, yamux_cfg, yamux::Mode::Server);
    let local_port = config.local_port;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            inbound = poll_fn(|cx| connection.poll_next_inbound(cx)) => {
                match inbound {
                    Some(Ok(stream)) => {
                        tokio::spawn(forward_stream(stream, local_port));
                    }
                    Some(Err(error)) => return Err(format!("tunnel stream error: {error}")),
                    None => return Ok(()), // 服务端关闭会话
                }
            }
        }
    }
}

/// 把一个 yamux stream 当作一条 HTTP 请求，裸字节双向管道到本机 neko-route。
async fn forward_stream(stream: yamux::Stream, local_port: u16) {
    let tcp = match TcpStream::connect(("127.0.0.1", local_port)).await {
        Ok(tcp) => tcp,
        Err(_) => return,
    };
    tcp.set_nodelay(true).ok();

    let (mut yamux_read, mut yamux_write) = tokio::io::split(stream.compat());
    let (mut local_read, mut local_write) = tcp.into_split();

    // 请求方向：隧道 → 本机。Go 读完响应后关闭 stream → 此向 EOF。
    let to_local = async {
        let _ = tokio::io::copy(&mut yamux_read, &mut local_write).await;
        let _ = local_write.shutdown().await;
    };
    // 响应方向：本机 → 隧道。复制完必须 flush + 干净 shutdown(发 FIN)，
    // 否则 drop stream 会 reset，Go 端读不到完整响应。
    let to_tunnel = async {
        let _ = tokio::io::copy(&mut local_read, &mut yamux_write).await;
        let _ = yamux_write.shutdown().await;
    };
    // 等两个方向都收尾，确保响应字节落到 Go 端、stream 干净关闭。
    tokio::join!(to_local, to_tunnel);
}

/// 逐字节读 HTTP 响应头直到 \r\n\r\n（不读过界，保留随后的 yamux 字节）。
async fn read_http_response_head<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<String, String> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = reader
            .read(&mut byte)
            .await
            .map_err(|e| format!("read upgrade reply: {e}"))?;
        if n == 0 {
            return Err("tunnel closed during upgrade".into());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
        if buf.len() > HANDSHAKE_MAX_BYTES {
            return Err("upgrade reply too long".into());
        }
    }
}

fn build_connector(insecure: bool) -> Result<TlsConnector, String> {
    // rustls 0.23 需要进程级默认 CryptoProvider；幂等安装（已装则忽略）。
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();

    let config = if insecure {
        tokio_rustls::rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(danger::NoVerify))
            .with_no_client_auth()
    } else {
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    Ok(TlsConnector::from(Arc::new(config)))
}

/// 仅供本地自签证书联调：跳过证书校验。生产默认走 webpki 根校验。
mod danger {
    use tokio_rustls::rustls::client::danger::{
        HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
    };
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    pub struct NoVerify;

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            tokio_rustls::rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // 真·端到端冒烟测试：假本地 neko-route + 真隧道（命中已部署服务端）+ 公网穿透回来。
    // 默认 ignore（需网络 + 在线服务端）。手动跑：
    //   cargo test share_tunnel::live_tests -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn end_to_end_tunnel() {
        // 1. 假本地 neko-route：任何请求回固定 JSON。
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    let _ = conn.read(&mut buf).await;
                    let body = "{\"object\":\"list\",\"via\":\"tunnel\"}";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes()).await;
                });
            }
        });

        // 2. 启动隧道连真服务端。
        let server = std::env::var("NEKO_SHARE_SERVER")
            .unwrap_or_else(|_| "server.neko.arm.moe:443".to_string());
        let (host, port) = server
            .rsplit_once(':')
            .map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(443)))
            .unwrap_or((server.clone(), 443));
        let identity = crate::share::generate_identity();
        let tunnel = ShareTunnel::new();
        tunnel
            .start(ShareTunnelConfig {
                server_host: host,
                server_port: port,
                identity: identity.clone(),
                secret: crate::share::generate_secret(),
                local_port,
                insecure_skip_verify: std::env::var("NEKO_SHARE_INSECURE").is_ok(),
            })
            .await;

        // 3. 等连接（最多 ~12s）。
        let mut connected = false;
        for _ in 0..60 {
            if tunnel.status().await.state == ShareTunnelState::Connected {
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(connected, "tunnel did not connect: {:?}", tunnel.status().await);

        // 4. 经公网 share host 穿透回来。
        let share_host =
            std::env::var("NEKO_SHARE_HOST").unwrap_or_else(|_| "share.neko.arm.moe".to_string());
        let url = format!("https://{share_host}/{identity}/v1/models");
        let body = reqwest::Client::new()
            .get(&url)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .expect("friend request failed")
            .text()
            .await
            .expect("read body");
        tunnel.stop().await;
        assert!(
            body.contains("\"via\":\"tunnel\""),
            "unexpected tunnel body: {body}"
        );
    }
}
